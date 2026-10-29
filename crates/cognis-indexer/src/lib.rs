//! cognis-indexer — indexing pipeline, static + inter-procedural complexity
//! analyzer, optimization advisor, and in-memory shadow patch evaluator.

pub use cognis_core::Result;

pub mod complexity;
pub mod embed_queue;
pub mod enricher;
pub mod parser;
pub mod pipeline;
pub mod planner;
pub mod resolver;
pub mod shadow;
pub mod writer;

pub use complexity::{analyze_symbol_ast, analyze_workspace_complexity};
pub use embed_queue::{DrainOutcome, EmbeddingQueueDrainer};
pub use enricher::{EnrichedSymbol, Enricher};
pub use parser::{language_for_path, parse_source, Language, ParseOutput};
pub use pipeline::{
    admitted_rel_paths, ArtifactKind, IndexStats, IndexerPipeline, PipelineWorkSnapshot,
};
pub use planner::{ChangePlanner, PlanRequest};
pub use resolver::{normalize_ident, resolve_edges, to_edge, to_edges, ResolvedEdge};
pub use shadow::{analyze_shadow_patch, parse_notation_to_class, ShadowFileEdit};
pub use writer::{FileWritePayload, IndexWriter};
