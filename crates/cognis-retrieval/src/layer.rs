//! `RetrievalLayer` — the per-layer retrieval seam (mirror of `base.py`).
//!
//! Task 5.1 lands the [`RetrievalLayer`] trait and its two confident layers over
//! `cognis-store`:
//!
//! * [`LexicalLayer`] — FTS5 (BM25) lexical search, delegating to
//!   [`SymbolStore::fts_search`]. Hits carry `layer = "lexical"`.
//! * [`SemanticLayer`] — dense vector KNN, embedding the query through the
//!   shared [`Embedder`] seam (Task 6 / Requirement 7.1) and delegating to
//!   [`SymbolStore::vec_search`]. Hits carry `layer = "semantic"`.
//!
//! Both layers are thin adapters: the *hit sets* are produced by the store's
//! parity-tested `fts_search` / `vec_search` (which mirror the Python
//! `LexicalLayer` / `SemanticLayer` field-for-field), so the lexical and
//! semantic hit sets are identical to the Python oracle on the same DB
//! (Requirement 4.2 / Property 2 — P-PAR-FTS, P-PAR-VEC). The trait gives the
//! capsule composer (Task 5.3) and the MCP tools (Task 7) one uniform way to
//! drive a layer, independent of how it reaches the store.
//!
//! A layer never query-rewrites here beyond the store's own contract: an empty
//! / whitespace-only query or `k == 0` degrades to an empty result rather than
//! erroring (graceful degradation, design § Error Handling), matching the
//! Python layers' early returns.

use cognis_core::{Hit, Result};
use cognis_embed::Embedder;
use cognis_store::SymbolStore;

/// One retrieval layer: a named source of [`Hit`]s for a query, run against a
/// read-only [`SymbolStore`].
///
/// Mirrors the Python `RetrievalLayer` protocol (`base.py`): `name` identifies
/// the layer (and is the `Hit::layer` tag its hits carry), and `search` returns
/// the layer's top-`k` hits, best-first. Implementations are `&self` and
/// read-only — a layer never mutates the index.
pub trait RetrievalLayer {
    /// The layer's stable name (also the `layer` tag on the hits it produces),
    /// e.g. `"lexical"` or `"semantic"`.
    fn name(&self) -> &'static str;

    /// Return this layer's top-`k` hits for `query`, best-first.
    ///
    /// `db` is the read surface the layer searches. A blank query or `k == 0`
    /// degrades to an empty `Vec` rather than erroring.
    fn search(&self, query: &str, k: usize, db: &dyn SymbolStore) -> Result<Vec<Hit>>;
}

/// Lexical FTS5 (BM25) layer.
///
/// Delegates to [`SymbolStore::fts_search`]; hits carry `layer = "lexical"` and
/// the FTS5 BM25 reason/snippet the store produces. Stateless and cheap to
/// construct.
#[derive(Debug, Default, Clone, Copy)]
pub struct LexicalLayer;

impl LexicalLayer {
    /// Create a lexical layer.
    pub fn new() -> Self {
        LexicalLayer
    }
}

impl RetrievalLayer for LexicalLayer {
    fn name(&self) -> &'static str {
        "lexical"
    }

    fn search(&self, query: &str, k: usize, db: &dyn SymbolStore) -> Result<Vec<Hit>> {
        if query.trim().is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        // The store's fts_search already mirrors `lexical.py` (BM25 scoring,
        // snippet, graceful degradation), so the hit set is parity-identical.
        db.fts_search(query, k)
    }
}

/// Semantic dense-vector (KNN) layer.
///
/// Embeds the query string through the shared [`Embedder`] seam, then delegates
/// the nearest-neighbour search to [`SymbolStore::vec_search`]; hits carry
/// `layer = "semantic"`. The embedder is obtained from the single
/// `cognis_embed::build_embedder` factory at the call site (Requirement 7.1),
/// so the backend is chosen once from config — this layer holds whatever
/// embedder it was given.
///
/// When the configured embedder is the zero-vector `stub` (or no vector index
/// is populated), `vec_search` returns no hits — the same graceful degradation
/// the Python `semantic_search` applies when the semantic index is unavailable.
pub struct SemanticLayer {
    embedder: Box<dyn Embedder>,
}

impl SemanticLayer {
    /// Create a semantic layer driven by `embedder` (typically the output of
    /// `cognis_embed::build_embedder`).
    pub fn new(embedder: Box<dyn Embedder>) -> Self {
        SemanticLayer { embedder }
    }

    /// The embedding dimensionality of the backing embedder.
    pub fn embedding_dim(&self) -> usize {
        self.embedder.embedding_dim()
    }
}

impl RetrievalLayer for SemanticLayer {
    fn name(&self) -> &'static str {
        "semantic"
    }

    fn search(&self, query: &str, k: usize, db: &dyn SymbolStore) -> Result<Vec<Hit>> {
        if query.trim().is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        // Embed the query once, then hand the vector to the store's KNN (which
        // mirrors `semantic.py`: vec0 MATCH or BLOB cosine fallback, ordered
        // nearest-first). The hit set is parity-identical on the same DB.
        let embedding = self.embedder.embed_text(query)?;
        db.vec_search(&embedding, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_core::{Symbol, SymbolKind};
    use cognis_embed::StubEmbedder;
    use cognis_store::{Database, SymbolWriter};

    /// A deterministic test embedder mapping a fixed query to a chosen vector,
    /// everything else to the zero vector. Not a mock of the system under test
    /// — it is a real [`Embedder`] that lets the semantic layer's
    /// query→embedding→KNN path run end-to-end over a real SQLite store.
    struct FixedEmbedder {
        dim: usize,
        hot: &'static str,
        vector: Vec<f32>,
    }

    impl Embedder for FixedEmbedder {
        fn embedding_dim(&self) -> usize {
            self.dim
        }
        fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
            if text == self.hot {
                Ok(self.vector.clone())
            } else {
                Ok(vec![0.0; self.dim])
            }
        }
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed_text(t)).collect()
        }
    }

    fn sym(id: &str, name: &str, qn: &str, body: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: qn.to_string(),
            language: "python".to_string(),
            module: "m".to_string(),
            file_path: "src/m.py".to_string(),
            line_start: 1,
            line_end: 5,
            signature: Some(format!("def {name}()")),
            docstring: None,
            content_hash: "h".to_string(),
            body_excerpt: Some(body.to_string()),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        }
    }

    fn floats_le(v: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        bytes
    }

    /// Build a `:memory:` UCKG with two symbols and (optionally) their BLOB
    /// embeddings for the semantic-layer test.
    fn db_with(symbols: &[Symbol], vectors: &[(&str, Vec<f32>)]) -> Database {
        let mut db = Database::new(":memory:");
        db.connect().unwrap();
        db.upsert_symbols(symbols).unwrap();
        if !vectors.is_empty() {
            let conn = db.connect().unwrap();
            for (id, v) in vectors {
                conn.execute(
                    "INSERT INTO symbol_vec(symbol_id, embedding) VALUES(?1, ?2)",
                    rusqlite::params![id, floats_le(v)],
                )
                .unwrap();
            }
        }
        db
    }

    #[test]
    fn layer_names_are_stable() {
        assert_eq!(LexicalLayer::new().name(), "lexical");
        let sem = SemanticLayer::new(Box::new(StubEmbedder::new(4)));
        assert_eq!(sem.name(), "semantic");
        assert_eq!(sem.embedding_dim(), 4);
    }

    #[test]
    fn lexical_layer_finds_matching_symbol() {
        let symbols = vec![
            sym(
                "py:src/m.py:m.authenticate@1",
                "authenticate",
                "m.authenticate",
                "authenticate the user and start a session",
            ),
            sym(
                "py:src/m.py:m.hash_password@2",
                "hash_password",
                "m.hash_password",
                "hash a password with the configured algorithm",
            ),
        ];
        let db = db_with(&symbols, &[]);
        let layer = LexicalLayer::new();
        let hits = layer.search("authenticate", 10, &db).unwrap();
        assert!(
            hits.iter().any(|h| h.symbol_id.contains("authenticate")),
            "lexical layer should find the 'authenticate' symbol, got {hits:?}"
        );
        assert!(hits.iter().all(|h| h.layer == "lexical"));
    }

    #[test]
    fn semantic_layer_ranks_nearest_first() {
        let symbols = vec![sym("a", "a", "m.a", "alpha"), sym("b", "b", "m.b", "beta")];
        // a ≈ query direction [1,0,0,0]; b is orthogonal.
        let vectors = vec![
            ("a", vec![1.0_f32, 0.0, 0.0, 0.0]),
            ("b", vec![0.0_f32, 1.0, 0.0, 0.0]),
        ];
        let db = db_with(&symbols, &vectors);
        let layer = SemanticLayer::new(Box::new(FixedEmbedder {
            dim: 4,
            hot: "find alpha",
            vector: vec![1.0, 0.0, 0.0, 0.0],
        }));
        let hits = layer.search("find alpha", 5, &db).unwrap();
        assert!(!hits.is_empty(), "semantic layer produced no hits");
        assert_eq!(hits[0].symbol_id, "a", "nearest vector should rank first");
        assert!(hits.iter().all(|h| h.layer == "semantic"));
    }

    #[test]
    fn blank_query_and_zero_k_degrade_to_empty() {
        let db = db_with(&[sym("a", "a", "m.a", "alpha")], &[]);
        let lex = LexicalLayer::new();
        assert!(lex.search("   ", 10, &db).unwrap().is_empty());
        assert!(lex.search("alpha", 0, &db).unwrap().is_empty());

        let sem = SemanticLayer::new(Box::new(StubEmbedder::new(4)));
        assert!(sem.search("", 10, &db).unwrap().is_empty());
        assert!(sem.search("alpha", 0, &db).unwrap().is_empty());
    }
}
