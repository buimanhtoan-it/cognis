//! `RetrievalEngine` — the read-only seam the tool handlers depend on.
//!
//! The MCP server is **read-only** (Requirement 3.4): it is wired to this trait,
//! which exposes only retrieval primitives, never a writer. The concrete
//! [`StoreEngine`](crate::store_engine::StoreEngine) implements it over
//! `cognis-store` + `cognis-csar` + `cognis-retrieval`; tests use an in-memory
//! fake. Decoupling the handlers from the data source lets Task 7.1 verify the
//! server core (framing, dispatch, caps, audit, envelope) without a live DB,
//! and lets later tasks deepen the retrieval wiring behind the same seam.
//!
//! Each method maps onto a primitive that already exists in the workspace:
//!
//! | method               | backing implementation                          |
//! | -------------------- | ----------------------------------------------- |
//! | [`fts_search`]       | `SymbolStore::fts_search` (FTS5)                 |
//! | [`semantic_search`]  | `SymbolStore::vec_search` + embedder (Task 6)    |
//! | [`diffuse`]          | `build_code_graph` + `csar::diffuse_seed_hits`   |
//! | [`hydrate`]          | symbol-row read-back (`SymbolStore::hydrate`)    |
//! | [`lookup`]           | id / qualified-name / fuzzy symbol resolution    |
//! | [`dependency_trace`] | directed call-graph BFS over `edge`              |
//!
//! [`fts_search`]: RetrievalEngine::fts_search
//! [`semantic_search`]: RetrievalEngine::semantic_search
//! [`diffuse`]: RetrievalEngine::diffuse
//! [`hydrate`]: RetrievalEngine::hydrate
//! [`lookup`]: RetrievalEngine::lookup
//! [`dependency_trace`]: RetrievalEngine::dependency_trace

use cognis_core::{Hit, Result, Symbol};

/// The read-only retrieval operations the 8 MCP tools are composed from.
///
/// All methods are `&self` (no `&mut`) — the server can never mutate the index.
/// Implementations degrade gracefully (return empty / `None`) rather than
/// erroring on absent capabilities, mirroring the Python tools' behaviour when
/// e.g. the semantic index is not populated.
pub trait RetrievalEngine {
    /// Lexical FTS5 hits for a raw query, best-first (`layer = "lexical"`).
    fn fts_search(&self, query: &str, k: usize) -> Result<Vec<Hit>>;

    /// Semantic KNN hits for a query string, best-first (`layer = "semantic"`).
    ///
    /// Returns an empty vector when no embedder / populated vector index is
    /// available — the same graceful degradation the Python `semantic_search`
    /// applies (it returns `[]` when `_semantic_index_available` is false).
    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<Hit>>;

    /// Diffuse the per-layer seed hits over the resident code graph and return
    /// the top-`k` CSAR hits (`layer = "csar"`, each carrying `on_path` /
    /// `ppr_score` evidence). `alpha`/`eps` are the forward-push parameters.
    fn diffuse(&self, seeds: &[Vec<Hit>], k: usize, alpha: f64, eps: f64) -> Result<Vec<Hit>>;

    /// Hydrate full [`Symbol`] records for `ids`. Only found symbols are
    /// returned (missing ids are simply absent); order is unspecified — callers
    /// index the result by id.
    fn hydrate(&self, ids: &[String]) -> Result<Vec<Symbol>>;

    /// Resolve a single symbol by exact id, then qualified name, then a fuzzy
    /// name match, optionally constrained to `kind`. Returns `None` when no
    /// symbol matches.
    fn lookup(&self, name_or_id: &str, kind: Option<&str>) -> Result<Option<Symbol>>;

    /// Trace dependencies from `symbol_id` along the call graph.
    ///
    /// `direction` is `"out"` (callees), `"in"` (callers), or `"both"`; `depth`
    /// bounds traversal. Returns reached symbols as hits (`layer =
    /// "structural"`, evidence carrying the hop `depth`), excluding the start
    /// symbol.
    fn dependency_trace(&self, symbol_id: &str, direction: &str, depth: u8) -> Result<Vec<Hit>>;

    /// Whether a usable semantic (vector) index is currently available. Used by
    /// handlers to decide whether to attempt a semantic leg. Defaults to
    /// `false`.
    fn semantic_available(&self) -> bool {
        false
    }

    /// Whether the additive integration-edge capsule context is enabled
    /// (`config.artifact.integration_edge_context`, default `false`).
    ///
    /// When `false` (the default for every implementation, including test
    /// fakes) the capsule composer emits a capsule byte-for-byte identical to
    /// the pre-feature output — integration edges contribute no context entry
    /// (Requirement 11.5). The concrete [`StoreEngine`](crate::store_engine::StoreEngine)
    /// threads the loaded config flag; edges are never a fused ranking signal.
    fn integration_edge_context(&self) -> bool {
        false
    }
}
