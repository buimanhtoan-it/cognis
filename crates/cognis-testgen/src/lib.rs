//! cognis-testgen — the shared `proptest` generators for the
//! `cognis-one-toggle-engine` property suite.
//!
//! **Dev-only.** This crate is `publish = false` and is pulled in exclusively as
//! a `dev-dependency` of `cognis-store`, `cognis-indexer`, `cognis-mcp`, and
//! (from task 13.2) `cognis-owner`. Nothing in the shipped single binary links
//! against it.
//!
//! Design reference: `.kiro/specs/cognis-one-toggle-engine/design.md`,
//! section "Generators". The properties are only as good as what they generate,
//! so the ten generators here are defined once and shared rather than
//! re-invented per test crate:
//!
//! | Generator | Produces | Feeds |
//! | --- | --- | --- |
//! | [`arb_source_tree`] | trees with mixed languages, empty files, non-UTF-8 bytes, unicode paths, deep nesting, duplicate symbol names, cross-file call graphs | P5–P12 |
//! | [`arb_transition_sequence`] | sequences over the eleven `Transition_Matrix` transitions, including a deliberately missed delete | P5, P6, P7 |
//! | [`arb_publish_batch`] | batches populating every mutation category, plus empty and delete-only batches | P1, P2, P3 |
//! | [`arb_interruption_point`] | ordinal positions inside the publish statement sequence | P1, P18 |
//! | [`arb_file_cache_state`] | caches current, stale, ahead of the tree, hash-matching with absent symbols, and parse-failed | P10 |
//! | [`arb_fingerprint_pair`] | fingerprints equal, differing in one component, and equal-dimension-but-different | P17, P18 |
//! | [`arb_event_burst`] | watcher bursts far exceeding the cap, repeated paths, rapid create/modify/delete cycles | P23 |
//! | [`arb_path_spelling`] | case variants, trailing separators, symlinks, UNC and `\\?\` verbatim prefixes | P21 |
//! | [`arb_arrival_sequence`] | tool-call arrivals with per-stage costs up to and beyond the concurrency cap | P24 |
//!
//! _Requirements: 12.1, 18.2, 24.1, 24.4_
//!
//! # Types that do not exist yet
//!
//! Several generators describe values whose production types land in later
//! tasks (`PublishBatch`, `FileCacheRow`, `QueuedSymbol`, `PublishReason`,
//! `VectorSpaceFingerprint` in task 1.2; `WatcherQueue` in task 13.3). Those
//! generators are defined here over **self-contained mirror structs** carrying
//! the design's exact field names and shapes, each with a `TODO(task N)` note
//! and, where the mapping is already unambiguous, a conversion into the
//! `cognis-core` type. When the production type lands, the mirror gains a
//! `From`/`to_*` conversion rather than being replaced, so existing tests keep
//! compiling.

pub mod arrivals;
pub mod events;
pub mod file_cache;
pub mod fingerprint;
pub mod paths;
pub mod publish;
pub mod source_tree;
pub mod transition;

pub use arrivals::{
    arb_arrival_sequence, arb_stage_costs, Arrival, ArrivalSequence, StageCosts, STAGES,
};
pub use events::{
    arb_event_burst, arb_event_burst_with_cap, EventBurst, FsEvent, FsEventKind,
    DEFAULT_WATCHER_QUEUE_MAX,
};
pub use file_cache::{
    arb_file_cache_state, arb_file_cache_state_over, FileCacheEntry, FileCacheKind,
    FileCacheRowState, FileCacheState,
};
pub use fingerprint::{
    arb_fingerprint, arb_fingerprint_pair, FingerprintPair, FingerprintRelation, FingerprintSpec,
};
pub use paths::{arb_path_spelling, arb_path_spelling_pair, spell, PathSpelling, SpellingKind};
pub use publish::{
    arb_interruption_point, arb_interruption_points_distinct, arb_publish_batch, FileCacheRowSpec,
    InterruptionMode, InterruptionPoint, PublishBatchSpec, PublishReasonSpec, PublishStage,
    QueuedSymbolSpec,
};
pub use source_tree::{
    arb_source_tree, arb_source_tree_sized, content_hash, FileBody, FunctionSpec, SourceFileSpec,
    SourceTree,
};
pub use transition::{
    apply_sequence, arb_transition, arb_transition_sequence, arb_transition_sequence_covering_all,
    Transition, TransitionOutcome,
};

/// Every generator in this crate draws from this deterministic name pool, so a
/// shrunk counterexample reads as `alpha`/`beta` rather than as noise.
///
/// Duplicates across files are deliberate: `arb_source_tree` reuses the pool
/// per file, which is what produces the duplicate-symbol-name and ambiguous
/// call-resolution cases the design calls for.
pub(crate) const NAME_POOL: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "handler", "process", "render", "load", "resolve", "emit",
];

/// Unicode directory segments used by `arb_source_tree` and `arb_path_spelling`
/// so path handling is exercised outside ASCII.
pub(crate) const UNICODE_SEGMENTS: &[&str] = &["café", "日本語", "модуль", "ünïcødé", "naïve"];
