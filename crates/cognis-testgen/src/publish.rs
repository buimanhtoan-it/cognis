//! `arb_publish_batch` and `arb_interruption_point`.
//!
//! Design: "batches populating every mutation category, including empty batches
//! and delete-only batches" (feeds P1, P2, P3) and "ordinal positions inside the
//! publish statement sequence (≥3 distinct points sampled for the crash branch,
//! per 24.4)" (feeds P1, P18).
//!
//! # Relationship to the production types (task 1.2 landed)
//!
//! `PublishBatch`, `FileCacheRow`, `QueuedSymbol`, `PublishReason`, and
//! `VectorSpaceFingerprint` now exist in `crates/cognis-store/src/publish.rs`.
//! The `*Spec` mirrors below are **kept** — every test already written against
//! them keeps compiling — and each gained a `From`/`to_*` conversion into its
//! production counterpart, so a generator feeds the real publisher without the
//! generator itself being rewritten.
//!
//! The mirrors stay meaningful beyond compatibility: [`FingerprintSpec`] carries
//! the individual Requirement 14.4 components the production type folds into a
//! single digest, and [`PublishBatchSpec`] is the type `arb_publish_batch`
//! shrinks over.

use proptest::prelude::*;

use cognis_core::{Edge, EdgeKind, ParseStatus, Symbol, SymbolKind};
use cognis_store::{FileCacheRow, PublishBatch, PublishReason, QueuedSymbol};

use crate::fingerprint::FingerprintSpec;
use crate::NAME_POOL;

/// Mirror of the design's `PublishReason`.
///
/// Converts into [`cognis_store::PublishReason`] via [`From`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishReasonSpec {
    Initial,
    Incremental,
    Reconcile,
    Full,
    ReEmbed,
}

/// Mirror of the design's `FileCacheRow`.
///
/// Converts into [`cognis_store::FileCacheRow`] via [`From`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheRowSpec {
    pub path: String,
    pub language: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub parsed_at: i64,
    pub parse_status: ParseStatus,
}

/// Mirror of the design's `QueuedSymbol`.
///
/// Converts into [`cognis_store::QueuedSymbol`] via [`From`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSymbolSpec {
    pub symbol_id: String,
    pub file_path: String,
}

/// Mirror of the design's `PublishBatch` — one batch's worth of every mutation
/// category the publisher applies in a single transaction.
///
/// Converts into [`cognis_store::PublishBatch`] via [`From`] /
/// [`to_batch`](PublishBatchSpec::to_batch).
#[derive(Debug, Clone, PartialEq)]
pub struct PublishBatchSpec {
    pub symbol_upserts: Vec<Symbol>,
    pub symbol_deletes: Vec<String>,
    pub edge_upserts: Vec<Edge>,
    /// `(src, dst, kind)` — the composite edge key.
    pub edge_deletes: Vec<(String, String, String)>,
    pub vector_upserts: Vec<(String, Vec<f32>)>,
    pub vector_deletes: Vec<String>,
    pub file_rows: Vec<FileCacheRowSpec>,
    pub file_deletes: Vec<String>,
    pub queue_enqueue: Vec<QueuedSymbolSpec>,
    pub queue_remove: Vec<String>,
    pub fingerprint: Option<FingerprintSpec>,
    pub reason: PublishReasonSpec,
}

impl PublishBatchSpec {
    /// A batch that mutates nothing — the empty-batch case P1 and P2 need.
    pub fn empty(reason: PublishReasonSpec) -> Self {
        PublishBatchSpec {
            symbol_upserts: Vec::new(),
            symbol_deletes: Vec::new(),
            edge_upserts: Vec::new(),
            edge_deletes: Vec::new(),
            vector_upserts: Vec::new(),
            vector_deletes: Vec::new(),
            file_rows: Vec::new(),
            file_deletes: Vec::new(),
            queue_enqueue: Vec::new(),
            queue_remove: Vec::new(),
            fingerprint: None,
            reason,
        }
    }

    /// Whether this batch mutates no row at all.
    pub fn is_empty(&self) -> bool {
        self.symbol_upserts.is_empty()
            && self.symbol_deletes.is_empty()
            && self.edge_upserts.is_empty()
            && self.edge_deletes.is_empty()
            && self.vector_upserts.is_empty()
            && self.vector_deletes.is_empty()
            && self.file_rows.is_empty()
            && self.file_deletes.is_empty()
            && self.queue_enqueue.is_empty()
            && self.queue_remove.is_empty()
    }

    /// Whether this batch only removes things — the delete-only case.
    pub fn is_delete_only(&self) -> bool {
        !self.is_empty()
            && self.symbol_upserts.is_empty()
            && self.edge_upserts.is_empty()
            && self.vector_upserts.is_empty()
            && self.file_rows.is_empty()
            && self.queue_enqueue.is_empty()
    }

    /// Paths this batch covers, which is what `publish_batch_file` records
    /// (Requirement 7.5).
    pub fn covered_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .file_rows
            .iter()
            .map(|r| r.path.clone())
            .chain(self.file_deletes.iter().cloned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// The production [`PublishBatch`] this spec describes.
    ///
    /// Convenience alias for the [`From`] impl, so a property body reads
    /// `batch.to_batch()` rather than a turbofished `.into()`.
    pub fn to_batch(&self) -> PublishBatch {
        PublishBatch::from(self)
    }
}

// ---------------------------------------------------------------------------
// Conversions into the production types (task 1.2)
//
// These are total and field-for-field: the mirrors were written from the same
// design table the production types were, so nothing has to be invented or
// dropped here. `FingerprintSpec` is the single lossy step, and it is lossy only
// in the direction the guard does not read (see `fingerprint.rs`).
// ---------------------------------------------------------------------------

impl From<PublishReasonSpec> for PublishReason {
    fn from(spec: PublishReasonSpec) -> Self {
        match spec {
            PublishReasonSpec::Initial => PublishReason::Initial,
            PublishReasonSpec::Incremental => PublishReason::Incremental,
            PublishReasonSpec::Reconcile => PublishReason::Reconcile,
            PublishReasonSpec::Full => PublishReason::Full,
            PublishReasonSpec::ReEmbed => PublishReason::ReEmbed,
        }
    }
}

impl From<&FileCacheRowSpec> for FileCacheRow {
    fn from(spec: &FileCacheRowSpec) -> Self {
        FileCacheRow {
            path: spec.path.clone(),
            language: spec.language.clone(),
            size_bytes: spec.size_bytes,
            content_hash: spec.content_hash.clone(),
            parsed_at: spec.parsed_at,
            parse_status: spec.parse_status,
        }
    }
}

impl From<FileCacheRowSpec> for FileCacheRow {
    fn from(spec: FileCacheRowSpec) -> Self {
        FileCacheRow::from(&spec)
    }
}

impl From<&QueuedSymbolSpec> for QueuedSymbol {
    fn from(spec: &QueuedSymbolSpec) -> Self {
        QueuedSymbol {
            symbol_id: spec.symbol_id.clone(),
            file_path: spec.file_path.clone(),
        }
    }
}

impl From<QueuedSymbolSpec> for QueuedSymbol {
    fn from(spec: QueuedSymbolSpec) -> Self {
        QueuedSymbol::from(&spec)
    }
}

impl From<&PublishBatchSpec> for PublishBatch {
    fn from(spec: &PublishBatchSpec) -> Self {
        PublishBatch {
            symbol_upserts: spec.symbol_upserts.clone(),
            symbol_deletes: spec.symbol_deletes.clone(),
            edge_upserts: spec.edge_upserts.clone(),
            edge_deletes: spec.edge_deletes.clone(),
            vector_upserts: spec.vector_upserts.clone(),
            vector_deletes: spec.vector_deletes.clone(),
            file_rows: spec.file_rows.iter().map(FileCacheRow::from).collect(),
            file_deletes: spec.file_deletes.clone(),
            queue_enqueue: spec.queue_enqueue.iter().map(QueuedSymbol::from).collect(),
            queue_remove: spec.queue_remove.clone(),
            fingerprint: spec.fingerprint.as_ref().map(|fp| fp.to_fingerprint()),
            reason: spec.reason.into(),
        }
    }
}

impl From<PublishBatchSpec> for PublishBatch {
    fn from(spec: PublishBatchSpec) -> Self {
        PublishBatch::from(&spec)
    }
}

// ---------------------------------------------------------------------------
// Interruption points
// ---------------------------------------------------------------------------

/// The ordered statement groups inside `IndexPublisher::publish`, from the
/// design's implementation contract. An [`InterruptionPoint`] names the group
/// *after* which the interruption fires.
///
/// The generation bump is deliberately second-to-last and the batch-record write
/// last, mirroring the design: "`UPDATE generation …` executes inside the same
/// transaction, **after** all row mutations". An interruption before
/// [`PublishStage::GenerationBump`] must therefore leave generation `g`; one
/// after it must leave exactly `g+1`, never a mixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublishStage {
    /// `BEGIN IMMEDIATE` has run; nothing is written yet.
    TxnOpen,
    SymbolUpserts,
    SymbolDeletes,
    FtsSync,
    EdgeUpserts,
    EdgeDeletes,
    VectorUpserts,
    VectorDeletes,
    FileRowUpserts,
    FileRowDeletes,
    QueueEnqueue,
    QueueRemove,
    FingerprintWrite,
    /// `UPDATE generation SET source_generation = source_generation + 1`.
    GenerationBump,
    /// `semantic_generation` advanced iff the queue is empty.
    SemanticGenerationBump,
    /// `publish_batch` / `publish_batch_file` rows.
    BatchRecord,
    /// `COMMIT`.
    Commit,
}

impl PublishStage {
    /// Every stage, in execution order.
    pub const ALL: [PublishStage; 17] = [
        PublishStage::TxnOpen,
        PublishStage::SymbolUpserts,
        PublishStage::SymbolDeletes,
        PublishStage::FtsSync,
        PublishStage::EdgeUpserts,
        PublishStage::EdgeDeletes,
        PublishStage::VectorUpserts,
        PublishStage::VectorDeletes,
        PublishStage::FileRowUpserts,
        PublishStage::FileRowDeletes,
        PublishStage::QueueEnqueue,
        PublishStage::QueueRemove,
        PublishStage::FingerprintWrite,
        PublishStage::GenerationBump,
        PublishStage::SemanticGenerationBump,
        PublishStage::BatchRecord,
        PublishStage::Commit,
    ];

    /// Ordinal position in the publish statement sequence.
    pub fn ordinal(&self) -> usize {
        PublishStage::ALL
            .iter()
            .position(|s| s == self)
            .expect("every stage is in ALL")
    }
}

/// How the publish is interrupted. Requirement 7.3 covers the first two;
/// Requirement 7.4 / 24.3 cover the third.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionMode {
    /// The closure returns `Err`, so the transaction rolls back.
    ErrorReturn,
    /// The transaction is explicitly rolled back.
    Rollback,
    /// The process is terminated mid-transaction; SQLite's WAL recovery decides
    /// the outcome. This is the expensive branch — Requirement 24.4 requires at
    /// least 3 distinct points, and the design permits a reduced case count for
    /// it as long as the reduction is recorded in the test comment.
    ProcessTermination,
}

impl InterruptionMode {
    /// Whether this mode spawns a real process (and therefore justifies a
    /// reduced proptest case count).
    pub fn is_expensive(&self) -> bool {
        matches!(self, InterruptionMode::ProcessTermination)
    }
}

/// An ordinal position inside the publish statement sequence, plus how the
/// interruption is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptionPoint {
    pub stage: PublishStage,
    pub mode: InterruptionMode,
}

impl InterruptionPoint {
    /// Ordinal position, for ordering and for reporting in a counterexample.
    pub fn ordinal(&self) -> usize {
        self.stage.ordinal()
    }

    /// The generation an interruption here must leave behind: `false` before the
    /// bump commits (generation `g`), `true` only when the whole transaction
    /// committed (generation `g+1`).
    ///
    /// Note this is `true` **only** for a commit-stage interruption: an
    /// interruption anywhere before `Commit` — including after the generation
    /// bump statement — rolls the whole transaction back, because the bump is
    /// inside the same transaction. That is precisely the atomicity claim P1
    /// checks, so the generator states it rather than leaving it implicit.
    pub fn expects_new_generation(&self) -> bool {
        self.stage == PublishStage::Commit
    }
}

/// Any single interruption point, uniformly over the statement sequence and the
/// three delivery modes.
pub fn arb_interruption_point() -> impl Strategy<Value = InterruptionPoint> {
    (
        prop::sample::select(PublishStage::ALL.as_slice()),
        prop_oneof![
            Just(InterruptionMode::ErrorReturn),
            Just(InterruptionMode::Rollback),
            Just(InterruptionMode::ProcessTermination),
        ],
    )
        .prop_map(|(stage, mode)| InterruptionPoint { stage, mode })
}

/// At least `n` **distinct** interruption points — Requirement 24.4 requires a
/// minimum of 3 for the crash-during-publish scenario, and a generator that can
/// return the same stage three times would satisfy the letter and miss the
/// point.
///
/// Panics if `n` exceeds the number of stages.
pub fn arb_interruption_points_distinct(
    n: usize,
    mode: InterruptionMode,
) -> impl Strategy<Value = Vec<InterruptionPoint>> {
    assert!(
        n <= PublishStage::ALL.len(),
        "requested {n} distinct interruption points but only {} stages exist",
        PublishStage::ALL.len()
    );
    prop::sample::subsequence(PublishStage::ALL.to_vec(), n).prop_map(move |stages| {
        let mut points: Vec<InterruptionPoint> = stages
            .into_iter()
            .map(|stage| InterruptionPoint { stage, mode })
            .collect();
        points.sort_by_key(InterruptionPoint::ordinal);
        points
    })
}

// ---------------------------------------------------------------------------
// Batch generation
// ---------------------------------------------------------------------------

fn arb_symbol_kind() -> impl Strategy<Value = SymbolKind> {
    prop_oneof![
        Just(SymbolKind::Function),
        Just(SymbolKind::Class),
        Just(SymbolKind::Method),
        Just(SymbolKind::Interface),
        Just(SymbolKind::Route),
        Just(SymbolKind::Module),
        Just(SymbolKind::Var),
        Just(SymbolKind::Const),
    ]
}

fn arb_edge_kind() -> impl Strategy<Value = EdgeKind> {
    prop_oneof![
        Just(EdgeKind::Calls),
        Just(EdgeKind::Imports),
        Just(EdgeKind::Inherits),
        Just(EdgeKind::Implements),
        Just(EdgeKind::Reads),
        Just(EdgeKind::Writes),
        Just(EdgeKind::RoutesTo),
        Just(EdgeKind::Tests),
    ]
}

fn arb_parse_status() -> impl Strategy<Value = ParseStatus> {
    prop_oneof![
        Just(ParseStatus::Ok),
        Just(ParseStatus::Partial),
        Just(ParseStatus::Failed),
    ]
}

/// A `Symbol` that always passes `Symbol::validate` — a batch full of invalid
/// rows would only ever test the validator.
fn arb_symbol() -> impl Strategy<Value = Symbol> {
    (
        0usize..NAME_POOL.len(),
        0usize..8,
        arb_symbol_kind(),
        1u32..500,
        0u32..50,
        0u64..1_000_000,
    )
        .prop_map(
            |(name_index, file_index, kind, line_start, span, hash_seed)| {
                let name = NAME_POOL[name_index];
                let file_path = format!("src/file_{file_index}.py");
                let content_hash = format!("{hash_seed:08x}");
                Symbol {
                    id: format!("python:{file_path}:mod.{name}@{content_hash}"),
                    kind,
                    name: name.to_string(),
                    qualified_name: format!("mod.{name}"),
                    language: "python".to_string(),
                    module: "mod".to_string(),
                    file_path,
                    line_start,
                    line_end: line_start + span,
                    signature: None,
                    docstring: None,
                    content_hash,
                    body_excerpt: None,
                    semantic_summary: None,
                    risk_score: 0.0,
                    ambiguous: false,
                    untrusted_flags: Vec::new(),
                    updated_at: 1_700_000_000,
                }
            },
        )
}

fn arb_edge() -> impl Strategy<Value = Edge> {
    (
        0usize..8,
        0usize..8,
        arb_edge_kind(),
        0u8..=100,
        any::<bool>(),
    )
        .prop_map(|(src, dst, kind, confidence, dst_missing)| Edge {
            src_id: format!("python:src/file_{src}.py:mod.alpha@0000000{src}"),
            dst_id: format!("python:src/file_{dst}.py:mod.beta@0000000{dst}"),
            kind,
            confidence: f64::from(confidence) / 100.0,
            meta: if dst_missing {
                serde_json::json!({ "dst_missing": true })
            } else {
                serde_json::Value::Null
            },
        })
}

fn arb_file_cache_row() -> impl Strategy<Value = FileCacheRowSpec> {
    (
        0usize..8,
        arb_parse_status(),
        0i64..1_000_000,
        0u64..1_000_000,
    )
        .prop_map(
            |(index, parse_status, size_bytes, hash_seed)| FileCacheRowSpec {
                path: format!("src/file_{index}.py"),
                language: "python".to_string(),
                size_bytes,
                content_hash: format!("{hash_seed:016x}"),
                parsed_at: 1_700_000_000,
                parse_status,
            },
        )
}

fn arb_queued_symbol() -> impl Strategy<Value = QueuedSymbolSpec> {
    (0usize..8, 0usize..NAME_POOL.len()).prop_map(|(index, name_index)| {
        let file_path = format!("src/file_{index}.py");
        QueuedSymbolSpec {
            symbol_id: format!(
                "python:{file_path}:mod.{}@0000000{index}",
                NAME_POOL[name_index]
            ),
            file_path,
        }
    })
}

fn arb_vector() -> impl Strategy<Value = (String, Vec<f32>)> {
    (0usize..8, prop::collection::vec(-1.0f32..1.0f32, 4..9)).prop_map(|(index, values)| {
        (
            format!("python:src/file_{index}.py:mod.alpha@0000000{index}"),
            values,
        )
    })
}

fn arb_reason() -> impl Strategy<Value = PublishReasonSpec> {
    prop_oneof![
        Just(PublishReasonSpec::Initial),
        Just(PublishReasonSpec::Incremental),
        Just(PublishReasonSpec::Reconcile),
        Just(PublishReasonSpec::Full),
        Just(PublishReasonSpec::ReEmbed),
    ]
}

/// A batch populating every mutation category.
fn arb_populated_batch() -> impl Strategy<Value = PublishBatchSpec> {
    (
        prop::collection::vec(arb_symbol(), 0..4),
        prop::collection::vec(arb_symbol().prop_map(|s| s.id), 0..3),
        prop::collection::vec(arb_edge(), 0..4),
        prop::collection::vec((0usize..8, 0usize..8, arb_edge_kind()), 0..3),
        prop::collection::vec(arb_vector(), 0..3),
        prop::collection::vec(arb_vector().prop_map(|(id, _)| id), 0..3),
        prop::collection::vec(arb_file_cache_row(), 0..4),
        prop::collection::vec((0usize..8).prop_map(|i| format!("src/file_{i}.py")), 0..3),
        prop::collection::vec(arb_queued_symbol(), 0..3),
        prop::collection::vec(arb_queued_symbol().prop_map(|q| q.symbol_id), 0..3),
        prop::option::of(crate::fingerprint::arb_fingerprint()),
        arb_reason(),
    )
        .prop_map(
            |(
                symbol_upserts,
                symbol_deletes,
                edge_upserts,
                edge_delete_keys,
                vector_upserts,
                vector_deletes,
                file_rows,
                file_deletes,
                queue_enqueue,
                queue_remove,
                fingerprint,
                reason,
            )| {
                let edge_deletes = edge_delete_keys
                    .into_iter()
                    .map(|(src, dst, kind)| {
                        (
                            format!("python:src/file_{src}.py:mod.alpha@0000000{src}"),
                            format!("python:src/file_{dst}.py:mod.beta@0000000{dst}"),
                            edge_kind_label(kind).to_string(),
                        )
                    })
                    .collect();
                PublishBatchSpec {
                    symbol_upserts,
                    symbol_deletes,
                    edge_upserts,
                    edge_deletes,
                    vector_upserts,
                    vector_deletes,
                    file_rows,
                    file_deletes,
                    queue_enqueue,
                    queue_remove,
                    fingerprint,
                    reason,
                }
            },
        )
}

/// A batch that only removes rows.
fn arb_delete_only_batch() -> impl Strategy<Value = PublishBatchSpec> {
    (
        prop::collection::vec(arb_symbol().prop_map(|s| s.id), 1..4),
        prop::collection::vec((0usize..8).prop_map(|i| format!("src/file_{i}.py")), 1..3),
        prop::collection::vec(arb_vector().prop_map(|(id, _)| id), 0..3),
        prop::collection::vec(arb_queued_symbol().prop_map(|q| q.symbol_id), 0..3),
        arb_reason(),
    )
        .prop_map(
            |(symbol_deletes, file_deletes, vector_deletes, queue_remove, reason)| {
                PublishBatchSpec {
                    symbol_deletes,
                    file_deletes,
                    vector_deletes,
                    queue_remove,
                    ..PublishBatchSpec::empty(reason)
                }
            },
        )
}

/// `arb_publish_batch` — batches populating every mutation category, plus empty
/// and delete-only batches.
///
/// The three shapes are weighted 6 : 1 : 2 so the degenerate cases stay common
/// enough to matter (an empty batch must still bump the generation; a
/// delete-only batch must not leave orphan FTS/vector/queue rows) without
/// crowding out the general case.
pub fn arb_publish_batch() -> impl Strategy<Value = PublishBatchSpec> {
    prop_oneof![
        6 => arb_populated_batch(),
        1 => arb_reason().prop_map(PublishBatchSpec::empty),
        2 => arb_delete_only_batch(),
    ]
}

fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Implements => "implements",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::RoutesTo => "routes_to",
        EdgeKind::Tests => "tests",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Generated symbols and edges satisfy the core model invariants, so a
        /// property failure is never just an invalid fixture.
        #[test]
        fn generated_rows_validate(batch in arb_publish_batch()) {
            for symbol in &batch.symbol_upserts {
                prop_assert!(symbol.validate().is_ok(), "{symbol:?}");
            }
            for edge in &batch.edge_upserts {
                prop_assert!(edge.validate().is_ok(), "{edge:?}");
            }
        }

        /// A delete-only batch upserts nothing; an empty batch mutates nothing.
        #[test]
        fn degenerate_batches_are_degenerate(batch in arb_publish_batch()) {
            if batch.is_delete_only() {
                prop_assert!(batch.symbol_upserts.is_empty());
                prop_assert!(batch.edge_upserts.is_empty());
                prop_assert!(batch.file_rows.is_empty());
                prop_assert!(!batch.is_empty());
            }
            if batch.is_empty() {
                prop_assert!(batch.covered_paths().is_empty());
            }
        }

        /// Conversion into the production `PublishBatch` is shape-preserving:
        /// every mutation category keeps its cardinality, the emptiness and
        /// covered-path answers agree, and the reason maps onto the label
        /// migration 002 stores.
        #[test]
        fn conversion_preserves_batch_shape(spec in arb_publish_batch()) {
            let batch = spec.to_batch();

            prop_assert_eq!(batch.symbol_upserts.len(), spec.symbol_upserts.len());
            prop_assert_eq!(batch.symbol_deletes.len(), spec.symbol_deletes.len());
            prop_assert_eq!(batch.edge_upserts.len(), spec.edge_upserts.len());
            prop_assert_eq!(batch.edge_deletes.len(), spec.edge_deletes.len());
            prop_assert_eq!(batch.vector_upserts.len(), spec.vector_upserts.len());
            prop_assert_eq!(batch.vector_deletes.len(), spec.vector_deletes.len());
            prop_assert_eq!(batch.file_rows.len(), spec.file_rows.len());
            prop_assert_eq!(batch.file_deletes.len(), spec.file_deletes.len());
            prop_assert_eq!(batch.queue_enqueue.len(), spec.queue_enqueue.len());
            prop_assert_eq!(batch.queue_remove.len(), spec.queue_remove.len());

            prop_assert_eq!(batch.is_empty(), spec.is_empty());
            prop_assert_eq!(batch.covered_paths(), spec.covered_paths());
            prop_assert_eq!(batch.fingerprint.is_some(), spec.fingerprint.is_some());
            prop_assert!(
                cognis_store::PublishReason::parse_db_str(batch.reason.as_db_str())
                    == Some(batch.reason)
            );
        }

        /// Interruption points are real ordinals in the statement sequence, and
        /// only a commit-stage interruption may land on the new generation.
        #[test]
        fn interruption_points_are_ordinals(point in arb_interruption_point()) {
            prop_assert!(point.ordinal() < PublishStage::ALL.len());
            if point.expects_new_generation() {
                prop_assert_eq!(point.stage, PublishStage::Commit);
            }
        }

        /// Requirement 24.4: at least 3 *distinct* interruption points.
        #[test]
        fn crash_points_are_distinct(
            points in arb_interruption_points_distinct(3, InterruptionMode::ProcessTermination),
        ) {
            prop_assert_eq!(points.len(), 3);
            let stages: BTreeSet<PublishStage> = points.iter().map(|p| p.stage).collect();
            prop_assert_eq!(stages.len(), 3);
            prop_assert!(points.iter().all(|p| p.mode.is_expensive()));
        }
    }

    #[test]
    fn stage_ordinals_are_execution_order() {
        // The generation bump happens after every row mutation and before the
        // commit — the ordering P1 depends on.
        assert!(PublishStage::SymbolUpserts.ordinal() < PublishStage::GenerationBump.ordinal());
        assert!(PublishStage::QueueRemove.ordinal() < PublishStage::GenerationBump.ordinal());
        assert!(PublishStage::GenerationBump.ordinal() < PublishStage::Commit.ordinal());
    }
}
