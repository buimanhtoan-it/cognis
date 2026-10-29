//! `StoreEngine` — the production [`RetrievalEngine`] over `cognis-store`.
//!
//! Wires the read-only MCP server to a real UCKG (`cognis-store::Database`):
//! lexical hits via FTS5, semantic hits via an embedder + `symbol_vec`, CSAR
//! diffusion via the resident code graph, and symbol hydration / lookup /
//! dependency trace over the `symbol` + `edge` tables.
//!
//! Query-side scans are replaced by snapshot-bounded methods (`hydrate_ids`,
//! `resolve_symbol`, `edges_incident`). A database missing any required index
//! is refused rather than scanned (Requirement 9.7).

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use cognis_core::{CodeGraph, CognisError, Config, Hit, Result, SemanticWarmPolicy, Symbol};
use cognis_embed::{
    failure_cooldown_from_env, Embedder, ModelFingerprint, ModelSlot, DEFAULT_FAILURE_COOLDOWN,
};
use cognis_store::{Database, Direction, GenerationSnapshot, VectorSpaceFingerprint};

use crate::engine::RetrievalEngine;
use crate::server::ToolCtx;

/// Outcome of comparing the persisted embedding space against the query embedder
/// (Requirement 14.2). `Allow` is the only path that may call `embed_text` /
/// `vec_search`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    Allow,
    Withhold { persisted: String, query: String },
}

/// Compare the workspace's persisted vector-space identity with the query
/// embedder's fingerprint. A missing persisted row cannot be verified and is
/// withheld (Requirement 14.2, 14.7).
pub fn guard(persisted: Option<&VectorSpaceFingerprint>, query: &ModelFingerprint) -> GuardVerdict {
    match persisted {
        Some(space) if !space.digest.is_empty() && space.digest == query.digest => {
            GuardVerdict::Allow
        }
        Some(space) => GuardVerdict::Withhold {
            persisted: space.digest.clone(),
            query: query.digest.clone(),
        },
        None => GuardVerdict::Withhold {
            persisted: String::new(),
            query: query.digest.clone(),
        },
    }
}

/// A [`RetrievalEngine`] backed by a `cognis-store` UCKG database.
pub struct StoreEngine {
    db: Database,
    model: ModelSlot,
    embedder_config: Config,
    query_fingerprint: ModelFingerprint,
    failure_cooldown: Duration,
    allow_demand_load: bool,
    integration_edge_context: bool,
    /// Cosine comparisons against persisted `symbol_vec` rows. Incremented only
    /// after the Vector_Space_Guard allows scoring (Requirement 14.7, 14.8).
    similarity_computations: AtomicU64,
    /// Generation-keyed resident CSR graph cache (`(source_generation, Arc<CodeGraph>)`).
    /// Eliminates per-call SQLite graph rebuilds while the generation is unchanged.
    resident_graph: RwLock<Option<(i64, Arc<CodeGraph>)>>,
}

impl StoreEngine {
    pub fn new(db: Database) -> Self {
        let embedder_config = Config::default();
        let query_fingerprint = ModelFingerprint::from_env_or_derive(&embedder_config);
        StoreEngine {
            db,
            model: ModelSlot::empty(),
            embedder_config,
            query_fingerprint,
            failure_cooldown: DEFAULT_FAILURE_COOLDOWN,
            allow_demand_load: false,
            integration_edge_context: false,
            similarity_computations: AtomicU64::new(0),
            resident_graph: RwLock::new(None),
        }
    }

    pub fn with_embedder(db: Database, embedder: Option<Box<dyn Embedder>>) -> Self {
        let embedder_config = Config::default();
        let query_fingerprint = ModelFingerprint::from_env_or_derive(&embedder_config);
        StoreEngine {
            db,
            model: ModelSlot::from_optional(embedder),
            embedder_config,
            query_fingerprint,
            failure_cooldown: DEFAULT_FAILURE_COOLDOWN,
            allow_demand_load: false,
            integration_edge_context: false,
            similarity_computations: AtomicU64::new(0),
            resident_graph: RwLock::new(None),
        }
    }

    /// Bind the query-side fingerprint the Vector_Space_Guard compares.
    pub fn with_query_fingerprint(mut self, fingerprint: ModelFingerprint) -> Self {
        self.query_fingerprint = fingerprint;
        self
    }

    /// How many persisted-vector similarity comparisons this engine has run.
    pub fn similarity_computations(&self) -> u64 {
        self.similarity_computations.load(Ordering::SeqCst)
    }

    pub fn open(path: &str) -> Result<Self> {
        Self::open_with_policy(path, SemanticWarmPolicy::from_env())
    }

    pub fn open_with_policy(path: &str, policy: SemanticWarmPolicy) -> Result<Self> {
        let db = Database::open(path)?;
        let config = config_for_db(path);
        let integration_edge_context = config.artifact.integration_edge_context;
        let failure_cooldown = failure_cooldown_from_env();

        let model = if policy.is_eager() {
            let embedder = cognis_embed::build_embedder(&config).ok();
            ModelSlot::from_optional(embedder)
        } else {
            ModelSlot::empty()
        };
        let query_fingerprint = ModelFingerprint::from_env_or_derive(&config);

        Ok(StoreEngine {
            db,
            model,
            embedder_config: config,
            query_fingerprint,
            failure_cooldown,
            allow_demand_load: true,
            integration_edge_context,
            similarity_computations: AtomicU64::new(0),
            resident_graph: RwLock::new(None),
        })
    }

    /// Wrap an already-open workspace database (the `Workspace_Owner` path).
    pub fn from_open_database(db: Database, policy: SemanticWarmPolicy) -> Result<Self> {
        let path = db.path().to_string();
        let config = config_for_db(&path);
        let integration_edge_context = config.artifact.integration_edge_context;
        let failure_cooldown = failure_cooldown_from_env();

        let model = if policy.is_eager() {
            let embedder = cognis_embed::build_embedder(&config).ok();
            ModelSlot::from_optional(embedder)
        } else {
            ModelSlot::empty()
        };
        let query_fingerprint = ModelFingerprint::from_env_or_derive(&config);

        Ok(StoreEngine {
            db,
            model,
            embedder_config: config,
            query_fingerprint,
            failure_cooldown,
            allow_demand_load: true,
            integration_edge_context,
            similarity_computations: AtomicU64::new(0),
            resident_graph: RwLock::new(None),
        })
    }

    pub fn in_memory_fixture() -> Result<Self> {
        let db = build_fixture_db()?;
        Ok(StoreEngine::new(db))
    }

    fn demand_embedder(&self) -> Option<cognis_embed::ModelBorrow<'_>> {
        if let Some(b) = self.model.try_borrow() {
            return Some(b);
        }
        if !self.allow_demand_load {
            return None;
        }
        let config = self.embedder_config.clone();
        let cooldown = self.failure_cooldown;
        self.model
            .borrow_or_load(cooldown, || cognis_embed::build_embedder(&config))
            .ok()
    }

    /// True only when the persisted vector space matches this process's query
    /// embedder. Must run before `embed_text` / `vec_search` (Requirement 14.7).
    fn vector_space_allows(&self, ctx: &ToolCtx<'_>) -> bool {
        let persisted = match ctx.snapshot.vector_space_fingerprint() {
            Ok(fingerprint) => fingerprint,
            Err(_) => return false,
        };
        matches!(
            guard(persisted.as_ref(), &self.query_fingerprint),
            GuardVerdict::Allow
        )
    }
}

impl RetrievalEngine for StoreEngine {
    fn open_snapshot(&self) -> Result<GenerationSnapshot<'_>> {
        refuse_missing_indexes(&self.db)?;
        GenerationSnapshot::open(&self.db)
    }

    fn missing_required_indexes(&self) -> Result<Vec<String>> {
        self.db.missing_required_indexes()
    }

    fn fts_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> Result<Vec<Hit>> {
        ctx.snapshot.fts_search(query, k)
    }

    fn semantic_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> Result<Vec<Hit>> {
        if !self.vector_space_allows(ctx) {
            return Ok(Vec::new());
        }
        let Some(borrow) = self.demand_embedder() else {
            return Ok(Vec::new());
        };
        let q = query.trim();
        if q.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let query_vec = match borrow.embedder().embed_text(q) {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };
        let scored = ctx.snapshot.vec_row_count().unwrap_or(0) as u64;
        self.similarity_computations
            .fetch_add(scored, Ordering::SeqCst);
        ctx.snapshot.vec_search(&query_vec, k)
    }

    fn resident_code_graph(&self, ctx: &ToolCtx<'_>) -> Result<Arc<CodeGraph>> {
        let pinned = ctx.pinned;
        if let Ok(guard) = self.resident_graph.read() {
            if let Some((gen, ref graph)) = *guard {
                if gen == pinned && !graph.is_empty() {
                    return Ok(Arc::clone(graph));
                }
            }
        }
        let built = Arc::new(ctx.snapshot.build_code_graph(None)?);
        if let Ok(mut guard) = self.resident_graph.write() {
            *guard = Some((pinned, Arc::clone(&built)));
        }
        Ok(built)
    }

    fn diffuse(
        &self,
        ctx: &ToolCtx<'_>,
        seeds: &[Vec<Hit>],
        k: usize,
        alpha: f64,
        eps: f64,
    ) -> Result<Vec<Hit>> {
        let graph = self.resident_code_graph(ctx)?;
        cognis_csar::diffuse_seed_hits(&graph, seeds, k, alpha, eps)
    }

    fn hydrate(&self, ctx: &ToolCtx<'_>, ids: &[String]) -> Result<Vec<Symbol>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // Requirement 9.3: only the requested primary keys, never `list_symbols`.
        ctx.snapshot.hydrate_ids(ids)
    }

    fn lookup(
        &self,
        ctx: &ToolCtx<'_>,
        name_or_id: &str,
        kind: Option<&str>,
    ) -> Result<Option<Symbol>> {
        let needle = name_or_id.trim();
        if needle.is_empty() {
            return Ok(None);
        }
        // Requirement 9.4: indexed id / qname / name ladder, never `all_symbols`.
        ctx.snapshot.resolve_symbol(needle, kind)
    }

    fn dependency_trace(
        &self,
        ctx: &ToolCtx<'_>,
        symbol_id: &str,
        direction: &str,
        depth: u8,
    ) -> Result<Vec<Hit>> {
        let Some(direction) = Direction::parse_str(direction) else {
            return Ok(Vec::new());
        };

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(symbol_id.to_string());
        let mut frontier = vec![symbol_id.to_string()];
        let mut hits = Vec::new();

        for hop in 1..=depth {
            // Requirement 9.5: only edges incident to this hop's frontier.
            let edges = ctx.snapshot.edges_incident(&frontier, direction)?;
            let mut next = Vec::new();
            for node in &frontier {
                for edge in &edges {
                    if edge.dst_missing() {
                        continue;
                    }
                    let neighbor = match direction {
                        Direction::Out if edge.src_id == *node => Some(&edge.dst_id),
                        Direction::In if edge.dst_id == *node => Some(&edge.src_id),
                        Direction::Both if edge.src_id == *node => Some(&edge.dst_id),
                        Direction::Both if edge.dst_id == *node => Some(&edge.src_id),
                        _ => None,
                    };
                    if let Some(neighbor) = neighbor {
                        if visited.insert(neighbor.clone()) {
                            hits.push(
                                Hit::new(
                                    neighbor.clone(),
                                    1.0 / hop as f64,
                                    "structural",
                                    "dependency",
                                )
                                .with_evidence(serde_json::json!({ "depth": hop })),
                            );
                            next.push(neighbor.clone());
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(hits)
    }

    fn semantic_available(&self, ctx: &ToolCtx<'_>) -> bool {
        self.model.is_loaded()
            && ctx.snapshot.vec_row_count().map(|n| n > 0).unwrap_or(false)
            && self.vector_space_allows(ctx)
    }

    fn integration_edge_context(&self, _ctx: &ToolCtx<'_>) -> bool {
        self.integration_edge_context
    }
}

fn refuse_missing_indexes(db: &Database) -> Result<()> {
    let missing = db.missing_required_indexes()?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CognisError::Retrieval(format!(
            "missing required index: {}",
            missing.join(", ")
        )))
    }
}

fn config_for_db(db_path: &str) -> Config {
    let p = Path::new(db_path);
    match p.parent().and_then(Path::parent) {
        Some(repo_root) => Config::load(repo_root).unwrap_or_default(),
        None => Config::default(),
    }
}

fn build_fixture_db() -> Result<Database> {
    use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
    use cognis_store::SymbolWriter;

    fn sym(id: &str, name: &str, qn: &str, path: &str, body: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: qn.to_string(),
            language: "python".to_string(),
            module: qn.split('.').next().unwrap_or("").to_string(),
            file_path: path.to_string(),
            line_start: 1,
            line_end: 10,
            signature: Some(format!("def {name}(... )")),
            docstring: None,
            content_hash: "abcd1234".to_string(),
            body_excerpt: Some(body.to_string()),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1_700_000_000,
        }
    }

    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            src_id: src.to_string(),
            dst_id: dst.to_string(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            meta: serde_json::Value::Null,
        }
    }

    let authenticate = "python:src/auth.py:auth.authenticate@a1";
    let verify = "python:src/auth.py:auth.verify_credentials@a2";
    let login = "python:src/routes.py:routes.login_handler@a3";
    let hashpw = "python:src/crypto.py:crypto.hash_password@a4";

    let symbols = vec![
        sym(
            authenticate,
            "authenticate",
            "auth.authenticate",
            "src/auth.py",
            "authenticate the user: verify the password then start a session",
        ),
        sym(
            verify,
            "verify_credentials",
            "auth.verify_credentials",
            "src/auth.py",
            "verify credentials against the stored password hash",
        ),
        sym(
            login,
            "login_handler",
            "routes.login_handler",
            "src/routes.py",
            "handle the login route and call authenticate for the user",
        ),
        sym(
            hashpw,
            "hash_password",
            "crypto.hash_password",
            "src/crypto.py",
            "hash a password using the configured algorithm",
        ),
    ];
    let edges = vec![
        edge(login, authenticate),
        edge(authenticate, verify),
        edge(verify, hashpw),
    ];

    let mut db = Database::new(":memory:");
    db.connect()?;
    db.upsert_symbols(&symbols)?;
    db.upsert_edges(&edges)?;
    Ok(db)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn fixture_engine() -> StoreEngine {
        StoreEngine::in_memory_fixture().expect("fixture db")
    }

    #[derive(Debug)]
    struct BagOfLettersEmbedder;

    impl Embedder for BagOfLettersEmbedder {
        fn embedding_dim(&self) -> usize {
            26
        }
        fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
            let mut v = vec![0.0f32; 26];
            for c in text.to_ascii_lowercase().chars() {
                if c.is_ascii_lowercase() {
                    v[(c as u8 - b'a') as usize] += 1.0;
                }
            }
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            Ok(v)
        }
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed_text(t)).collect()
        }
    }

    fn semantic_fixture_engine() -> StoreEngine {
        use cognis_store::SymbolWriter;
        let mut db = build_fixture_db().expect("fixture db");
        let embedder = BagOfLettersEmbedder;
        db.reconcile_embedding_dim(embedder.embedding_dim())
            .unwrap();
        let symbols = db.list_symbols().unwrap();
        let rows: Vec<(String, Vec<f32>)> = symbols
            .iter()
            .map(|s| {
                let text = format!(
                    "{}\n{}",
                    s.qualified_name,
                    s.body_excerpt.as_deref().unwrap_or("")
                );
                (s.id.clone(), embedder.embed_text(&text).unwrap())
            })
            .collect();
        db.upsert_embeddings(&rows).unwrap();
        persist_matching_vector_space(&db);
        StoreEngine::with_embedder(db, Some(Box::new(BagOfLettersEmbedder)))
    }

    fn persist_matching_vector_space(db: &Database) {
        let fingerprint = ModelFingerprint::from_env_or_derive(&Config::default());
        persist_vector_space(db, fingerprint.as_str());
    }

    fn persist_vector_space(db: &Database, digest: &str) {
        let cfg = Config::default();
        let conn = db.connect().expect("connect fixture db");
        conn.execute(
            "INSERT INTO vector_space(id, fingerprint, embedding_dim, model_id, established_at) \
             VALUES (1, ?1, ?2, ?3, 0)",
            rusqlite::params![digest, cfg.embedder.dim as i64, cfg.embedder.model],
        )
        .expect("insert vector_space");
    }

    #[derive(Debug)]
    struct PanicOnEmbed;

    impl Embedder for PanicOnEmbed {
        fn embedding_dim(&self) -> usize {
            26
        }
        fn embed_text(&self, _text: &str) -> Result<Vec<f32>> {
            panic!("embed_text must not run when Vector_Space_Guard withholds");
        }
        fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            panic!("embed_batch must not run when Vector_Space_Guard withholds");
        }
    }

    #[test]
    fn semantic_absent_without_embedder_or_vectors() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        assert!(!e.semantic_available(&ctx), "no embedder, no vectors");
        assert!(e
            .semantic_search(&ctx, "authenticate", 5)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn semantic_search_returns_hits_when_wired() {
        let e = semantic_fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        assert!(
            e.semantic_available(&ctx),
            "embedder present + symbol_vec populated"
        );
        let hits = e
            .semantic_search(&ctx, "authenticate the user and verify the password", 10)
            .unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.layer == "semantic"));
    }

    #[test]
    fn fts_finds_authenticate() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        let hits = e.fts_search(&ctx, "authenticate", 10).unwrap();
        assert!(hits.iter().any(|h| h.symbol_id.contains("authenticate")));
    }

    #[test]
    fn lookup_by_name_and_qualified_name() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        assert!(e.lookup(&ctx, "authenticate", None).unwrap().is_some());
        assert!(e
            .lookup(&ctx, "auth.verify_credentials", None)
            .unwrap()
            .is_some());
        assert!(e.lookup(&ctx, "does_not_exist", None).unwrap().is_none());
    }

    #[test]
    fn dependency_trace_reaches_callees() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        let login = "python:src/routes.py:routes.login_handler@a3";
        let hits = e.dependency_trace(&ctx, login, "out", 3).unwrap();
        assert!(hits.iter().any(|h| h.symbol_id.contains("authenticate")));
        assert!(hits.iter().any(|h| h.symbol_id.contains("hash_password")));
    }

    #[test]
    fn diffuse_tags_on_path() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        let seeds = vec![e.fts_search(&ctx, "authenticate", 25).unwrap()];
        let hits = e.diffuse(&ctx, &seeds, 10, 0.15, 1e-5).unwrap();
        assert!(!hits.is_empty());
        for h in &hits {
            assert!(h.evidence.get("on_path").is_some());
            assert!(h.evidence.get("ppr_score").is_some());
        }
    }

    #[test]
    fn guard_allows_matching_digests_and_withholds_otherwise() {
        let query = ModelFingerprint::from_digest("abc");
        assert_eq!(
            guard(
                Some(&VectorSpaceFingerprint {
                    digest: "abc".into(),
                    dim: 384,
                    model_id: "m".into(),
                }),
                &query
            ),
            GuardVerdict::Allow
        );
        assert!(matches!(
            guard(
                Some(&VectorSpaceFingerprint {
                    digest: "def".into(),
                    dim: 384,
                    model_id: "m".into(),
                }),
                &query
            ),
            GuardVerdict::Withhold { .. }
        ));
        assert!(matches!(
            guard(None, &query),
            GuardVerdict::Withhold { persisted, .. } if persisted.is_empty()
        ));
    }

    fn withhold_engine(digest: Option<&str>) -> StoreEngine {
        use cognis_store::SymbolWriter;
        let mut db = build_fixture_db().expect("fixture db");
        db.reconcile_embedding_dim(26).unwrap();
        let symbols = db.list_symbols().unwrap();
        let rows: Vec<(String, Vec<f32>)> = symbols
            .iter()
            .map(|s| (s.id.clone(), vec![1.0f32; 26]))
            .collect();
        db.upsert_embeddings(&rows).unwrap();
        if let Some(digest) = digest {
            persist_vector_space(&db, digest);
        }
        StoreEngine::with_embedder(db, Some(Box::new(PanicOnEmbed)))
    }

    #[test]
    fn mismatch_withholds_semantic_without_scoring_and_keeps_lexical_graph() {
        let e = withhold_engine(Some(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        ));
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        assert!(!e.semantic_available(&ctx));
        assert!(e
            .semantic_search(&ctx, "authenticate", 5)
            .unwrap()
            .is_empty());
        assert!(e
            .fts_search(&ctx, "authenticate", 10)
            .unwrap()
            .iter()
            .any(|h| h.symbol_id.contains("authenticate")));
        let login = "python:src/routes.py:routes.login_handler@a3";
        assert!(e
            .dependency_trace(&ctx, login, "out", 3)
            .unwrap()
            .iter()
            .any(|h| h.symbol_id.contains("authenticate")));
        let seeds = vec![e.fts_search(&ctx, "authenticate", 25).unwrap()];
        assert!(!e.diffuse(&ctx, &seeds, 10, 0.15, 1e-5).unwrap().is_empty());
    }

    #[test]
    fn missing_vector_space_withholds_without_scoring() {
        let e = withhold_engine(None);
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        assert!(!e.semantic_available(&ctx));
        assert!(e
            .semantic_search(&ctx, "authenticate", 5)
            .unwrap()
            .is_empty());
    }

    fn engine_with_dropped_index(name: &str) -> StoreEngine {
        let db = build_fixture_db().expect("fixture db");
        db.connect()
            .expect("connect")
            .execute(&format!("DROP INDEX {name}"), [])
            .expect("drop required index");
        StoreEngine::new(db)
    }

    #[test]
    fn missing_required_index_refuses_snapshot_and_names_the_index() {
        let e = engine_with_dropped_index("idx_symbol_name_lower");
        assert_eq!(
            e.missing_required_indexes().unwrap(),
            vec!["idx_symbol_name_lower".to_string()]
        );
        let error = match e.open_snapshot() {
            Ok(_) => panic!("must refuse to pin a snapshot when a required index is missing"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("idx_symbol_name_lower"),
            "refusal must name the missing index, got {error}"
        );
    }

    #[test]
    fn hydrate_lookup_and_trace_use_snapshot_bounded_reads() {
        let e = fixture_engine();
        let caps = crate::caps::Caps::default();
        let snapshot = e.open_snapshot().unwrap();
        let ctx = ToolCtx::new(snapshot, crate::caps::Deadline::start(&caps), &caps);
        let login = "python:src/routes.py:routes.login_handler@a3";
        let hydrated = e.hydrate(&ctx, &[login.to_string()]).unwrap();
        assert_eq!(hydrated.len(), 1);
        assert_eq!(hydrated[0].id, login);
        assert!(e.lookup(&ctx, "authenticate", None).unwrap().is_some());
        assert!(e
            .dependency_trace(&ctx, login, "out", 3)
            .unwrap()
            .iter()
            .any(|h| h.symbol_id.contains("authenticate")));
    }
}
