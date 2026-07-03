//! `StoreEngine` — the production [`RetrievalEngine`] over `cognis-store`.
//!
//! Wires the read-only MCP server to a real UCKG (`cognis-store::Database`):
//! lexical hits via FTS5, semantic hits via an embedder + `SymbolStore::vec_search`,
//! CSAR diffusion via the resident code graph (`cognis-csar`), and symbol
//! hydration / lookup / dependency trace over the `symbol` + `edge` tables.
//!
//! ## Semantic leg (query-time half of the embedding pipeline)
//!
//! [`StoreEngine::open`] loads `.cognis/config.yaml` (relative to the DB) and
//! builds the configured embedder best-effort via `cognis_embed::build_embedder`
//! (`embedder.backend`, default `local` → ONNX). When an embedder is present
//! **and** `symbol_vec` is populated (the indexer's index-time half ran), the
//! server embeds the query and runs a KNN over the vector index; otherwise the
//! semantic leg degrades to empty — the same graceful behaviour the Python
//! server has when the vector index is unpopulated. A build without the `onnx`
//! feature (or with the model assets absent) simply yields no embedder, so the
//! server still serves lexical + structural retrieval.
//!
//! The same type also backs the **contract e2e fixture**: [`in_memory_fixture`]
//! seeds a `:memory:` UCKG with a small, deterministic call graph so the live
//! `cognis-mcpd` can serve the full 8-tool contract without depending on the
//! indexer (Task 8). This keeps Property 4 (contract invariance) isolated from
//! retrieval-quality concerns (Properties 2/5), which are gated by other tasks.
//!
//! [`in_memory_fixture`]: StoreEngine::in_memory_fixture

use std::path::Path;

use cognis_core::{Config, Hit, Result, Symbol};
use cognis_embed::Embedder;
use cognis_store::{Database, SymbolStore};

use crate::engine::RetrievalEngine;

/// A [`RetrievalEngine`] backed by a `cognis-store` UCKG database.
pub struct StoreEngine {
    db: Database,
    /// The configured embedder, when one could be built for this process. When
    /// `None` the semantic leg degrades to empty (lexical/structural only).
    embedder: Option<Box<dyn Embedder>>,
}

impl StoreEngine {
    /// Build an engine over an already-open [`Database`] with no embedder
    /// (lexical + structural + CSAR only). Used by the contract fixture.
    pub fn new(db: Database) -> Self {
        StoreEngine { db, embedder: None }
    }

    /// Build an engine over `db` with an explicit embedder (dependency-injected
    /// in tests; also the shape [`open`](StoreEngine::open) constructs).
    pub fn with_embedder(db: Database, embedder: Option<Box<dyn Embedder>>) -> Self {
        StoreEngine { db, embedder }
    }

    /// Open the UCKG at `path` (runs migrations) and build an engine over it,
    /// wiring the configured embedder for the semantic leg.
    ///
    /// The config is loaded from `<repo>/.cognis/config.yaml` inferred from the
    /// DB path (`<repo>/.cognis/uckg.db`), falling back to defaults when it
    /// can't be located. The embedder is built best-effort: an unavailable
    /// backend (e.g. `onnx` not compiled in, or model assets missing) degrades
    /// to no embedder rather than failing the server to start.
    pub fn open(path: &str) -> Result<Self> {
        let db = Database::open(path)?;
        let config = config_for_db(path);
        let embedder = cognis_embed::build_embedder(&config).ok();
        Ok(StoreEngine::with_embedder(db, embedder))
    }

    /// Seed a `:memory:` UCKG with a deterministic fixture call graph and build
    /// an engine over it — the live-server backing for the contract e2e. No
    /// embedder / vectors, so the semantic leg is empty (contract shape only).
    pub fn in_memory_fixture() -> Result<Self> {
        let db = build_fixture_db()?;
        Ok(StoreEngine::new(db))
    }

    /// Load all symbols once (milestone hydration path).
    ///
    /// `hydrate` / `lookup` / `dependency_trace` read the full symbol/edge set
    /// and resolve in-memory. This is correct and simple for the migration
    /// milestone; the indexed point-read primitives land with the store's
    /// read-surface completion. The contract e2e runs on a tiny fixture, so the
    /// O(n) scan is immaterial here.
    fn all_symbols(&self) -> Result<Vec<Symbol>> {
        self.db.list_symbols()
    }
}

impl RetrievalEngine for StoreEngine {
    fn fts_search(&self, query: &str, k: usize) -> Result<Vec<Hit>> {
        SymbolStore::fts_search(&self.db, query, k)
    }

    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<Hit>> {
        // Query-time half of the semantic pipeline: embed the query with the
        // configured embedder, then KNN over the persisted `symbol_vec`.
        // Degrades to empty (never errors the tool) when no embedder is wired,
        // the query is blank, or the embedder fails — mirroring the Python
        // server's behaviour when the vector index / model is unavailable.
        let Some(embedder) = self.embedder.as_ref() else {
            return Ok(Vec::new());
        };
        let q = query.trim();
        if q.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let query_vec = match embedder.embed_text(q) {
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
        // an embedder to embed the query, and a populated `symbol_vec` to search.
        self.embedder.is_some() && self.db.vec_row_count().map(|n| n > 0).unwrap_or(false)
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
}
