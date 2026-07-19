//! `StoreEngine` — the production [`RetrievalEngine`] over `cognis-store`.
//!
//! Wires the read-only MCP server to a real UCKG (`cognis-store::Database`):
//! lexical hits via FTS5, semantic hits via an embedder + `SymbolStore::vec_search`,
//! CSAR diffusion via the resident code graph (`cognis-csar`), and symbol
//! hydration / lookup / dependency trace over the `symbol` + `edge` tables.
//!
//! ## Semantic leg (query-time half of the embedding pipeline)
//!
//! [`StoreEngine::open`] / [`StoreEngine::open_with_policy`] load
//! `.cognis/config.yaml` (relative to the DB) and wire a lazy
//! [`cognis_embed::ModelSlot`]:
//!
//! * [`SemanticWarmPolicy::Eager`] — warm the slot up front (best-effort) so
//!   the legacy / direct-launch path still maps the model at open.
//! * [`SemanticWarmPolicy::Lazy`] — leave the slot empty so **zero** ONNX
//!   session is resident before demand. Concurrent first demand coalesces into
//!   one single-flight load; failures enter a bounded cooldown; in-flight
//!   borrows refuse eviction (Requirement 2.5; Correctness Property 6).
//!
//! When an embedder is present **and** `symbol_vec` is populated (the indexer's
//! index-time half ran), the server embeds the query and runs a KNN over the
//! vector index; otherwise the semantic leg degrades to empty — the same
//! graceful behaviour the Python server has when the vector index is
//! unpopulated. A build without the `onnx` feature (or with the model assets
//! absent) simply fails the factory once and cools down, so the server still
//! serves lexical + structural retrieval.
//!
//! The same type also backs the **contract e2e fixture**: [`in_memory_fixture`]
//! seeds a `:memory:` UCKG with a small, deterministic call graph so the live
//! `cognis-mcpd` can serve the full 8-tool contract without depending on the
//! indexer (Task 8). This keeps Property 4 (contract invariance) isolated from
//! retrieval-quality concerns (Properties 2/5), which are gated by other tasks.
//!
//! [`in_memory_fixture`]: StoreEngine::in_memory_fixture

use std::path::Path;
use std::time::Duration;

use cognis_core::{Config, Hit, Result, SemanticWarmPolicy, Symbol};
use cognis_embed::{failure_cooldown_from_env, Embedder, ModelSlot, DEFAULT_FAILURE_COOLDOWN};
use cognis_store::{Database, SymbolStore};

use crate::engine::RetrievalEngine;

/// A [`RetrievalEngine`] backed by a `cognis-store` UCKG database.
pub struct StoreEngine {
    db: Database,
    /// Lazy single-flight embedder slot (Requirement 2.5).
    ///
    /// * Under [`SemanticWarmPolicy::Lazy`] the slot starts empty — zero ONNX
    ///   resident before demand; first semantic demand loads via single-flight.
    /// * Under [`SemanticWarmPolicy::Eager`] the slot is warmed in
    ///   [`open_with_policy`](StoreEngine::open_with_policy) (best-effort).
    /// * Fixture / injected engines use [`ModelSlot::empty`] or
    ///   [`ModelSlot::from_optional`].
    ///
    /// `semantic_available()` probes `is_loaded()` only (never triggers a load).
    /// Semantic demand goes through [`ModelSlot::borrow_or_load`]; load
    /// failures and an empty slot degrade to an empty semantic leg (never a
    /// hard tool error).
    model: ModelSlot,
    /// Config used by the on-demand factory (loaded at open). Default for
    /// fixture / injected engines.
    embedder_config: Config,
    /// Failure cooldown between load attempts (env-resolved at open).
    failure_cooldown: Duration,
    /// When false, demand never calls the factory (fixture / pure-lexical).
    allow_demand_load: bool,
    /// Whether additive integration-edge capsule context is enabled
    /// (`config.artifact.integration_edge_context`, default `false`). Threaded
    /// from the loaded config in [`open`](StoreEngine::open); `false` for the
    /// no-config constructors so their capsules stay pre-feature identical.
    integration_edge_context: bool,
}

impl StoreEngine {
    /// Build an engine over an already-open [`Database`] with no embedder
    /// (lexical + structural + CSAR only). Used by the contract fixture.
    pub fn new(db: Database) -> Self {
        StoreEngine {
            db,
            model: ModelSlot::empty(),
            embedder_config: Config::default(),
            failure_cooldown: DEFAULT_FAILURE_COOLDOWN,
            allow_demand_load: false,
            integration_edge_context: false,
        }
    }

    /// Build an engine over `db` with an explicit embedder (dependency-injected
    /// in tests).
    ///
    /// `Some(e)` seeds a ready [`ModelSlot`]; `None` leaves the slot empty.
    /// Demand-load is disabled: the caller supplied (or omitted) the embedder
    /// explicitly.
    pub fn with_embedder(db: Database, embedder: Option<Box<dyn Embedder>>) -> Self {
        StoreEngine {
            db,
            model: ModelSlot::from_optional(embedder),
            embedder_config: Config::default(),
            failure_cooldown: DEFAULT_FAILURE_COOLDOWN,
            allow_demand_load: false,
            integration_edge_context: false,
        }
    }

    /// Open the UCKG at `path` (runs migrations) and build an engine over it,
    /// wiring the configured embedder for the semantic leg according to the
    /// process warm policy ([`SemanticWarmPolicy::from_env`]).
    ///
    /// Delegates to [`open_with_policy`](StoreEngine::open_with_policy). Prefer
    /// that entry point when the caller already resolved the policy (daemon
    /// entry points) so the resolution site is explicit and testable.
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with_policy(path, SemanticWarmPolicy::from_env())
    }

    /// Open the UCKG at `path` and build an engine under an explicit warm
    /// policy (Requirement 2.4 / 2.5; Correctness Properties 5–6).
    ///
    /// * [`SemanticWarmPolicy::Eager`] — warm the [`ModelSlot`] up front
    ///   (best-effort; unavailable backends leave the slot Empty / Failed with
    ///   cooldown, equivalent to the historical `.ok()` degradation). This is
    ///   the legacy / direct-launch behavior and the safe fallback for invalid
    ///   env values.
    /// * [`SemanticWarmPolicy::Lazy`] — leave the slot empty so zero
    ///   ONNX/session is resident before demand. First semantic demand
    ///   initializes via single-flight; concurrent waiters share the outcome;
    ///   load failures cool down before retry. Until then the semantic leg
    ///   degrades to empty (same empty-degradation contract as "no embedder").
    ///
    /// Non-semantic retrieval (FTS / structural / lookup / diffuse / capsule)
    /// never waits on ONNX under either policy (preservation 3.3). Semantic
    /// hits and error envelopes remain equivalent for the same
    /// repo/DB/fingerprint/query once the embedder is present (preservation
    /// 3.4).
    pub fn open_with_policy(path: &str, policy: SemanticWarmPolicy) -> Result<Self> {
        let db = Database::open(path)?;
        let config = config_for_db(path);
        let integration_edge_context = config.artifact.integration_edge_context;
        let failure_cooldown = failure_cooldown_from_env();

        // Eager: honor the historical up-front warm via a best-effort factory
        // call, then seed the slot. Lazy: start Empty so zero ONNX is resident
        // before demand (bug facets `semanticWarmPolicyIsIgnoredOrInconsistent`
        // and `processLoadsDuplicateModelWithoutDemand`).
        let model = if policy.is_eager() {
            let embedder = cognis_embed::build_embedder(&config).ok();
            ModelSlot::from_optional(embedder)
        } else {
            ModelSlot::empty()
        };

        Ok(StoreEngine {
            db,
            model,
            embedder_config: config,
            failure_cooldown,
            // Daemon / open path: demand may load the configured backend.
            allow_demand_load: true,
            integration_edge_context,
        })
    }

    /// Seed a `:memory:` UCKG with a deterministic fixture call graph and build
    /// an engine over it — the live-server backing for the contract e2e. No
    /// embedder / vectors, so the semantic leg is empty (contract shape only).
    pub fn in_memory_fixture() -> Result<Self> {
        let db = build_fixture_db()?;
        Ok(StoreEngine::new(db))
    }

    /// Load all symbols once (milestone hydration path).
    fn all_symbols(&self) -> Result<Vec<Symbol>> {
        self.db.list_symbols()
    }

    /// Borrow the embedder for a semantic call, loading on demand (single-flight).
    ///
    /// Returns `None` when the slot cannot supply an embedder (empty + no
    /// demand-load, cooldown, or factory failure) — the empty-degradation
    /// contract (never a hard tool error).
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
}

impl RetrievalEngine for StoreEngine {
    fn fts_search(&self, query: &str, k: usize) -> Result<Vec<Hit>> {
        SymbolStore::fts_search(&self.db, query, k)
    }

    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<Hit>> {
        // Query-time half of the semantic pipeline: demand the embedder
        // (single-flight load under Lazy; already Ready under Eager), embed the
        // query, then KNN over the persisted `symbol_vec`. Degrades to empty
        // (never errors the tool) when the slot cannot supply an embedder, the
        // query is blank, or the embedder fails — mirroring the Python server's
        // behaviour when the vector index / model is unavailable.
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
        SymbolStore::vec_search(&self.db, &query_vec, k)
    }

    fn diffuse(&self, seeds: &[Vec<Hit>], k: usize, alpha: f64, eps: f64) -> Result<Vec<Hit>> {
        let graph = SymbolStore::build_code_graph(&self.db, None)?;
        cognis_csar::diffuse_seed_hits(&graph, seeds, k, alpha, eps)
    }

    fn hydrate(&self, ids: &[String]) -> Result<Vec<Symbol>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let want: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        Ok(self
            .all_symbols()?
            .into_iter()
            .filter(|s| want.contains(s.id.as_str()))
            .collect())
    }

    fn lookup(&self, name_or_id: &str, kind: Option<&str>) -> Result<Option<Symbol>> {
        let needle = name_or_id.trim();
        if needle.is_empty() {
            return Ok(None);
        }
        let needle_lower = needle.to_lowercase();
        let symbols = self.all_symbols()?;

        let kind_ok = |s: &Symbol| match kind {
            Some(want) => {
                serde_json::to_value(s.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .as_deref()
                    == Some(want)
            }
            None => true,
        };

        // Resolution order mirrors the Python `_score_symbol_match` ladder:
        // exact id → exact qualified_name → exact name → case-insensitive name
        // → substring. The first (best) match wins.
        let candidates = symbols.iter().filter(|s| kind_ok(s));
        let mut best: Option<(u32, &Symbol)> = None;
        for s in candidates {
            let rank = if s.id == needle {
                6
            } else if s.qualified_name == needle {
                5
            } else if s.name == needle {
                4
            } else if s.name.to_lowercase() == needle_lower {
                3
            } else if s.name.to_lowercase().contains(&needle_lower)
                || s.qualified_name.to_lowercase().contains(&needle_lower)
            {
                2
            } else if s.id.to_lowercase().contains(&needle_lower) {
                1
            } else {
                0
            };
            if rank > 0 && best.map(|(r, _)| rank > r).unwrap_or(true) {
                best = Some((rank, s));
            }
        }
        Ok(best.map(|(_, s)| s.clone()))
    }

    fn dependency_trace(&self, symbol_id: &str, direction: &str, depth: u8) -> Result<Vec<Hit>> {
        let edges = self.db.list_edges()?;

        // BFS over the directed call graph from `symbol_id`, bounded by `depth`,
        // excluding the start symbol. Each reached symbol becomes a structural
        // hit carrying its hop depth in evidence.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(symbol_id.to_string());
        let mut frontier = vec![symbol_id.to_string()];
        let mut hits = Vec::new();

        for hop in 1..=depth {
            let mut next = Vec::new();
            for node in &frontier {
                for e in &edges {
                    if e.dst_missing() {
                        continue;
                    }
                    let neighbor = match direction {
                        "out" if e.src_id == *node => Some(&e.dst_id),
                        "in" if e.dst_id == *node => Some(&e.src_id),
                        "both" if e.src_id == *node => Some(&e.dst_id),
                        "both" if e.dst_id == *node => Some(&e.src_id),
                        _ => None,
                    };
                    if let Some(n) = neighbor {
                        if visited.insert(n.clone()) {
                            hits.push(
                                Hit::new(n.clone(), 1.0 / hop as f64, "structural", "dependency")
                                    .with_evidence(serde_json::json!({ "depth": hop })),
                            );
                            next.push(n.clone());
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

    fn semantic_available(&self) -> bool {
        // Semantic is usable only when both halves of the pipeline are present:
        // a *currently resident* embedder (is_loaded — never triggers a load),
        // and a populated `symbol_vec` to search. Under Lazy this stays false
        // until first demand warms the slot (Requirement 2.5; preservation 3.3).
        self.model.is_loaded() && self.db.vec_row_count().map(|n| n > 0).unwrap_or(false)
    }

    fn integration_edge_context(&self) -> bool {
        self.integration_edge_context
    }
}

/// Load the config for a UCKG at `db_path`, inferring the repo root from the
/// conventional `<repo>/.cognis/uckg.db` layout. Falls back to defaults when
/// the path has no `.cognis` parent (e.g. `:memory:` or a bare filename), so a
/// non-standard DB location still yields a usable (default-backed) embedder.
fn config_for_db(db_path: &str) -> Config {
    let p = Path::new(db_path);
    match p.parent().and_then(Path::parent) {
        Some(repo_root) => Config::load(repo_root).unwrap_or_default(),
        None => Config::default(),
    }
}

/// Build a `:memory:` UCKG seeded with the fixture call graph.
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
            signature: Some(format!("def {name}(...)")),
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
    db.connect()?; // open + migrate on this thread before writing
    db.upsert_symbols(&symbols)?;
    db.upsert_edges(&edges)?;
    Ok(db)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Shared fixture engine for the server tests.
    pub fn fixture_engine() -> StoreEngine {
        StoreEngine::in_memory_fixture().expect("fixture db")
    }

    /// A tiny deterministic offline embedder (26-d bag-of-letters, L2-normed)
    /// so the semantic seam can be asserted without the ONNX backend.
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

    /// A fixture engine with a populated vector index + injected embedder, so
    /// the semantic leg is genuinely exercised (not just shape-checked).
    fn semantic_fixture_engine() -> StoreEngine {
        use cognis_store::SymbolWriter;
        let mut db = build_fixture_db().expect("fixture db");
        let embedder = BagOfLettersEmbedder;
        db.reconcile_embedding_dim(embedder.embedding_dim())
            .unwrap();
        // Embed each fixture symbol's qualified name + body and persist.
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
        StoreEngine::with_embedder(db, Some(Box::new(BagOfLettersEmbedder)))
    }

    #[test]
    fn semantic_absent_without_embedder_or_vectors() {
        let e = fixture_engine();
        assert!(!e.semantic_available(), "no embedder, no vectors");
        assert!(e.semantic_search("authenticate", 5).unwrap().is_empty());
    }

    #[test]
    fn semantic_search_returns_hits_when_wired() {
        let e = semantic_fixture_engine();
        assert!(
            e.semantic_available(),
            "embedder present + symbol_vec populated"
        );
        let hits = e
            .semantic_search("authenticate the user and verify the password", 10)
            .unwrap();
        assert!(
            !hits.is_empty(),
            "semantic search must return hits once the pipeline is wired"
        );
        assert!(
            hits.iter().all(|h| h.layer == "semantic"),
            "semantic hits carry the semantic layer tag"
        );
    }

    #[test]
    fn fts_finds_authenticate() {
        let e = fixture_engine();
        let hits = e.fts_search("authenticate", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.symbol_id.contains("authenticate")),
            "expected an 'authenticate' hit, got {hits:?}"
        );
    }

    #[test]
    fn lookup_by_name_and_qualified_name() {
        let e = fixture_engine();
        assert!(e.lookup("authenticate", None).unwrap().is_some());
        assert!(e.lookup("auth.verify_credentials", None).unwrap().is_some());
        assert!(e.lookup("does_not_exist", None).unwrap().is_none());
    }

    #[test]
    fn dependency_trace_reaches_callees() {
        let e = fixture_engine();
        let login = "python:src/routes.py:routes.login_handler@a3";
        let hits = e.dependency_trace(login, "out", 3).unwrap();
        // login → authenticate → verify_credentials → hash_password
        assert!(hits.iter().any(|h| h.symbol_id.contains("authenticate")));
        assert!(hits.iter().any(|h| h.symbol_id.contains("hash_password")));
    }

    #[test]
    fn diffuse_tags_on_path() {
        let e = fixture_engine();
        let seeds = vec![e.fts_search("authenticate", 25).unwrap()];
        let hits = e.diffuse(&seeds, 10, 0.15, 1e-5).unwrap();
        assert!(!hits.is_empty(), "diffusion produced no hits");
        for h in &hits {
            assert!(h.evidence.get("on_path").is_some());
            assert!(h.evidence.get("ppr_score").is_some());
        }
    }

    /// Lazy policy: open leaves the slot empty; first semantic demand warms it.
    #[test]
    fn lazy_open_defers_load_until_semantic_demand() {
        use cognis_store::SymbolWriter;

        let dir = std::env::temp_dir().join(format!(
            "cognis-slot-lazy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".cognis")).unwrap();
        std::fs::write(
            dir.join(".cognis").join("config.yaml"),
            "embedder:\n  backend: stub\n  dim: 8\n",
        )
        .unwrap();
        let db_path = dir.join(".cognis").join("uckg.db");
        {
            use cognis_core::{Symbol, SymbolKind};
            let mut db = Database::open(&db_path).unwrap();
            let sym = Symbol {
                id: "python:src/a.py:a.f@1".into(),
                kind: SymbolKind::Function,
                name: "f".into(),
                qualified_name: "a.f".into(),
                language: "python".into(),
                module: "a".into(),
                file_path: "src/a.py".into(),
                line_start: 1,
                line_end: 2,
                signature: None,
                docstring: None,
                content_hash: "x".into(),
                body_excerpt: Some("f body".into()),
                semantic_summary: None,
                risk_score: 0.0,
                ambiguous: false,
                untrusted_flags: Vec::new(),
                updated_at: 1,
            };
            db.upsert_symbols(std::slice::from_ref(&sym)).unwrap();
            db.reconcile_embedding_dim(8).unwrap();
            db.upsert_embeddings(&[(sym.id, vec![1.0; 8])]).unwrap();
        }

        let engine =
            StoreEngine::open_with_policy(db_path.to_str().unwrap(), SemanticWarmPolicy::Lazy)
                .expect("open lazy");
        assert!(
            !engine.semantic_available(),
            "Lazy open must not map a model before demand"
        );
        // First demand loads the stub via single-flight.
        let _ = engine.semantic_search("f", 5).unwrap();
        assert!(
            engine.semantic_available(),
            "after demand the slot must be Ready and vectors are present"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Eager policy: open warms the slot so semantic is available immediately
    /// when vectors are present.
    #[test]
    fn eager_open_warms_slot_up_front() {
        let dir = std::env::temp_dir().join(format!(
            "cognis-slot-eager-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".cognis")).unwrap();
        std::fs::write(
            dir.join(".cognis").join("config.yaml"),
            "embedder:\n  backend: stub\n  dim: 8\n",
        )
        .unwrap();
        let db_path = dir.join(".cognis").join("uckg.db");
        {
            use cognis_core::{Symbol, SymbolKind};
            use cognis_store::SymbolWriter;
            let mut db = Database::open(&db_path).unwrap();
            let sym = Symbol {
                id: "python:src/a.py:a.f@1".into(),
                kind: SymbolKind::Function,
                name: "f".into(),
                qualified_name: "a.f".into(),
                language: "python".into(),
                module: "a".into(),
                file_path: "src/a.py".into(),
                line_start: 1,
                line_end: 2,
                signature: None,
                docstring: None,
                content_hash: "x".into(),
                body_excerpt: Some("f body".into()),
                semantic_summary: None,
                risk_score: 0.0,
                ambiguous: false,
                untrusted_flags: Vec::new(),
                updated_at: 1,
            };
            db.upsert_symbols(std::slice::from_ref(&sym)).unwrap();
            db.reconcile_embedding_dim(8).unwrap();
            db.upsert_embeddings(&[(sym.id, vec![1.0; 8])]).unwrap();
        }

        let engine =
            StoreEngine::open_with_policy(db_path.to_str().unwrap(), SemanticWarmPolicy::Eager)
                .expect("open eager");
        assert!(
            engine.semantic_available(),
            "Eager open must warm the slot so semantic is available with zero tool calls"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
