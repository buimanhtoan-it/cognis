//! cognis-indexer — indexing pipeline.
//!
//! Task 8.1 lands the **Parser** stage: tree-sitter extraction for TS/JS,
//! Python, Go, C#, and Java (Requirement 9.1) with a per-file textual fallback
//! that keeps the batch alive when a file cannot be parsed (Requirement 9.4).
//!
//! Task 8.2 lands the next three stages, mirroring the Python
//! `cognis_indexer` package field-for-field (Requirement 9.2 parity):
//!
//! * [`resolver`] — resolve `calls` / `inherits` / `implements` edges between a
//!   batch's symbols and convert them to [`cognis_core::Edge`]s.
//! * [`enricher`] — scrub secrets and set `untrusted_flags` **before** anything
//!   is persisted (Requirement 9.3).
//! * [`writer`] — persist enriched symbols + resolved edges through
//!   `cognis-store`'s [`SymbolWriter`](cognis_store::SymbolWriter) trait, one
//!   file at a time.
//!
//! Task 8.3 lands the [`pipeline`] orchestrator + incremental indexing: the
//! native callable entry (`index_repo` / `index_changed_files` / `remove_file`
//! / `index_batch`) the `cognis-indexd` watcher plugs into, and the global
//! edge-resolution scope the symbol/edge count-parity gate (Requirement 9.2,
//! Property P-PAR-IDX) depends on.
pub use cognis_core::Result;

pub mod enricher;
pub mod parser;
pub mod pipeline;
pub mod resolver;
pub mod writer;

pub use enricher::{EnrichedSymbol, Enricher};
pub use parser::{language_for_path, parse_source, Language, ParseOutput};
pub use pipeline::{
    admitted_rel_paths, ArtifactKind, IndexStats, IndexerPipeline, PipelineWorkSnapshot,
};
pub use resolver::{normalize_ident, resolve_edges, to_edge, to_edges, ResolvedEdge};
pub use writer::{FileWritePayload, IndexWriter};
