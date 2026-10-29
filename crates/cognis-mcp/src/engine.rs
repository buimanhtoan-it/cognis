//! `RetrievalEngine` — the read-only seam the tool handlers depend on.

use std::sync::{Arc, OnceLock};

use cognis_core::{CodeGraph, Hit, Result, Symbol};
use cognis_store::{Database, GenerationSnapshot};

use crate::server::ToolCtx;

fn scratch_snapshot_db() -> &'static Database {
    static SCRATCH: OnceLock<Database> = OnceLock::new();
    SCRATCH.get_or_init(|| Database::new(":memory:"))
}

/// The read-only retrieval and analysis operations the MCP tools are composed from.
///
/// All methods are `&self` (no `&mut`) — the server can never mutate the index.
pub trait RetrievalEngine {
    /// Open the pinned read snapshot a single tool call will use.
    fn open_snapshot(&self) -> Result<GenerationSnapshot<'_>> {
        GenerationSnapshot::open(scratch_snapshot_db())
    }

    /// Lexical FTS5 hits for a raw query, best-first (`layer = "lexical"`).
    fn fts_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> Result<Vec<Hit>>;

    /// Semantic KNN hits for a query string, best-first (`layer = "semantic"`).
    fn semantic_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> Result<Vec<Hit>>;

    /// Diffuse the per-layer seed hits over the resident code graph and return
    /// the top-`k` CSAR hits (`layer = "csar"`, each carrying `on_path` /
    /// `ppr_score` evidence). `alpha`/`eps` are the forward-push parameters.
    fn diffuse(
        &self,
        ctx: &ToolCtx<'_>,
        seeds: &[Vec<Hit>],
        k: usize,
        alpha: f64,
        eps: f64,
    ) -> Result<Vec<Hit>>;

    /// Hydrate full [`Symbol`] records for `ids`.
    fn hydrate(&self, ctx: &ToolCtx<'_>, ids: &[String]) -> Result<Vec<Symbol>>;

    /// Resolve a single symbol by exact id, then qualified name, then a fuzzy
    /// name match, optionally constrained to `kind`.
    fn lookup(
        &self,
        ctx: &ToolCtx<'_>,
        name_or_id: &str,
        kind: Option<&str>,
    ) -> Result<Option<Symbol>>;

    /// Trace dependencies from `symbol_id` along the call graph.
    fn dependency_trace(
        &self,
        ctx: &ToolCtx<'_>,
        symbol_id: &str,
        direction: &str,
        depth: u8,
    ) -> Result<Vec<Hit>>;

    /// Return the generation-pinned CSR [`CodeGraph`], using a resident cache
    /// when backed by `StoreEngine`.
    fn resident_code_graph(&self, ctx: &ToolCtx<'_>) -> Result<Arc<CodeGraph>> {
        Ok(Arc::new(ctx.snapshot.build_code_graph(None)?))
    }

    /// Required indexes absent from the workspace database (Requirement 9.7).
    fn missing_required_indexes(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Whether a usable semantic (vector) index is currently available.
    fn semantic_available(&self, _ctx: &ToolCtx<'_>) -> bool {
        false
    }

    /// Whether the additive integration-edge capsule context is enabled.
    fn integration_edge_context(&self, _ctx: &ToolCtx<'_>) -> bool {
        false
    }
}
