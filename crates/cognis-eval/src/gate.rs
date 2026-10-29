//! Retrieval_Gate — before/after ranking and metric comparison (Requirement 26).
//!
//! The gate is constructed with a tolerance that is read once and never
//! mutated (Requirement 26.5). It runs live before/after evaluations against
//! the same committed `source_generation` and corpus identity (Requirement
//! 26.1), reports Recall / Precision / MRR / Contamination on both sides
//! (Requirement 26.3), requires identical top-k identifier sequences for
//! every query whose retrieval path was not intentionally altered
//! (Requirement 26.2), and fails when any metric degrades by more than the
//! tolerance (Requirement 26.4). The same inputs produce the same report
//! (Requirement 26.6). Corpus identity records whether a fresh clone can
//! reproduce the snapshot (Requirement 26.7).

use std::collections::HashSet;

use cognis_core::CognisError;
use cognis_store::{Database, GenerationSnapshot};

use crate::bench::{FairHarness, GoldenSet, MethodAggregate, Metrics};
use crate::Result;

/// One side of a Retrieval_Gate comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalSide {
    /// Committed Source_Generation the evaluation ran against.
    pub source_generation: i64,
    /// Corpus identity (Requirement 26.7).
    pub corpus_id: String,
    /// Aggregated Recall / Precision / MRR / Contamination.
    pub metrics: Metrics,
    /// Per-query top-k identifier sequences.
    pub queries: Vec<QueryRanking>,
}

/// Top-k identifiers for one golden query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRanking {
    /// Query identity (stable across before/after).
    pub query: String,
    /// Ranked identifiers, best first, already truncated to k.
    pub top_k: Vec<String>,
    /// When true, this query's retrieval path is intentionally altered and
    /// ranking equality is not required.
    pub intentionally_altered: bool,
}

/// Outcome of [`RetrievalGate::evaluate`].
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalGateReport {
    /// True only when rankings are stable and no metric exceeded the tolerance.
    pub passed: bool,
    /// True when every unaltered query has identical top-k identifiers.
    pub ranking_stable: bool,
    /// Metrics from the before evaluation.
    pub before: Metrics,
    /// Metrics from the after evaluation.
    pub after: Metrics,
    /// Metric names that degraded by more than the tolerance.
    pub degraded: Vec<&'static str>,
}

/// Identity of the corpus both evaluations must share (Requirement 26.1, 26.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusIdentity {
    /// Stable corpus id recorded on both evaluation sides.
    pub id: String,
    /// `true` only when a fresh clone can rebuild this corpus.
    pub reproducible_from_fresh_clone: bool,
}

impl CorpusIdentity {
    /// A corpus a fresh clone can reproduce.
    pub fn reproducible(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            reproducible_from_fresh_clone: true,
        }
    }

    /// A local snapshot a fresh clone cannot reproduce (Requirement 26.7).
    pub fn local_snapshot(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            reproducible_from_fresh_clone: false,
        }
    }

    /// Limitation note placed next to figures when the corpus is local.
    pub fn limitation_note(&self) -> Option<&'static str> {
        if self.reproducible_from_fresh_clone {
            None
        } else {
            Some("corpus is a local snapshot that a fresh clone cannot reproduce")
        }
    }
}

/// Outcome of [`RetrievalGate::compare`], including corpus identity (26.7).
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalGateRun {
    /// Comparison verdict and the four metrics on each side.
    pub report: RetrievalGateReport,
    /// Corpus both evaluations ran over.
    pub corpus: CorpusIdentity,
    /// Committed generation both sides shared (refused otherwise).
    pub source_generation: i64,
    /// Before evaluation as measured.
    pub before: RetrievalSide,
    /// After evaluation as measured.
    pub after: RetrievalSide,
}

impl RetrievalGateRun {
    /// Local-snapshot limitation, when the corpus is not clone-reproducible.
    pub fn corpus_limitation(&self) -> Option<&'static str> {
        self.corpus.limitation_note()
    }
}

/// Default pre-declared Retrieval_Gate tolerance (absolute metric units).
pub const DEFAULT_RETRIEVAL_TOLERANCE: f64 = 0.05;

/// Fixed-tolerance Retrieval_Gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetrievalGate {
    tolerance: f64,
}

impl RetrievalGate {
    /// Construct a gate. `tolerance` is captured once and left unchanged.
    pub fn new(tolerance: f64) -> Self {
        Self {
            tolerance: tolerance.max(0.0),
        }
    }

    /// The tolerance captured at construction (Requirement 26.5).
    pub fn tolerance(&self) -> f64 {
        self.tolerance
    }

    /// Run a live evaluation against `db`'s committed `source_generation`.
    ///
    /// Pins generation first (WAL snapshot), then ranks the golden set on the
    /// lexical channel. Unresolved queries are skipped, never fabricated.
    pub fn measure(
        db: &Database,
        golden: &GoldenSet,
        corpus: &CorpusIdentity,
        k: usize,
        intentionally_altered: &HashSet<String>,
    ) -> Result<RetrievalSide> {
        let snapshot = GenerationSnapshot::open(db)?;
        let source_generation = snapshot.source_generation();
        drop(snapshot);

        let harness = FairHarness::new(db)?.with_k(k);
        let mut per_query = Vec::new();
        let mut queries = Vec::new();
        for query in &golden.queries {
            let Some((order, metrics)) = harness.evaluate_query(query) else {
                continue;
            };
            let top_k = order.into_iter().take(k).collect();
            per_query.push(metrics);
            queries.push(QueryRanking {
                query: query.q.clone(),
                top_k,
                intentionally_altered: intentionally_altered.contains(&query.q),
            });
        }
        let agg = MethodAggregate::mean("lexical", &per_query);
        Ok(RetrievalSide {
            source_generation,
            corpus_id: corpus.id.clone(),
            metrics: Metrics {
                recall: agg.recall,
                precision: agg.precision,
                mrr: agg.mrr,
                contamination: agg.contamination,
            },
            queries,
        })
    }

    /// Measure before/after on live databases, then compare.
    ///
    /// Tolerance was captured at construction, before these measurements
    /// (Requirement 26.5). Refuses when the two DBs differ in generation or
    /// corpus (Requirement 26.1).
    pub fn compare(
        &self,
        before_db: &Database,
        after_db: &Database,
        golden: &GoldenSet,
        corpus: &CorpusIdentity,
        k: usize,
        intentionally_altered: &HashSet<String>,
    ) -> Result<RetrievalGateRun> {
        let before = Self::measure(before_db, golden, corpus, k, intentionally_altered)?;
        let after = Self::measure(after_db, golden, corpus, k, intentionally_altered)?;
        let report = self.evaluate(&before, &after)?;
        Ok(RetrievalGateRun {
            source_generation: before.source_generation,
            corpus: corpus.clone(),
            report,
            before,
            after,
        })
    }

    /// Compare `before` and `after`. Refuses when generation or corpus differ.
    pub fn evaluate(
        &self,
        before: &RetrievalSide,
        after: &RetrievalSide,
    ) -> Result<RetrievalGateReport> {
        if before.source_generation != after.source_generation {
            return Err(CognisError::Eval(format!(
                "Retrieval_Gate refused: source_generation {} != {}",
                before.source_generation, after.source_generation
            )));
        }
        if before.corpus_id != after.corpus_id {
            return Err(CognisError::Eval(format!(
                "Retrieval_Gate refused: corpus {} != {}",
                before.corpus_id, after.corpus_id
            )));
        }

        let ranking_stable = rankings_stable(&before.queries, &after.queries);
        let degraded = degraded_metrics(&before.metrics, &after.metrics, self.tolerance);
        Ok(RetrievalGateReport {
            passed: ranking_stable && degraded.is_empty(),
            ranking_stable,
            before: before.metrics,
            after: after.metrics,
            degraded,
        })
    }
}

/// Higher-is-better metrics degrade when `after < before - tolerance`.
/// Contamination degrades when `after > before + tolerance`.
pub fn degraded_metrics(before: &Metrics, after: &Metrics, tolerance: f64) -> Vec<&'static str> {
    let mut degraded = Vec::new();
    if after.recall < before.recall - tolerance {
        degraded.push("Recall");
    }
    if after.precision < before.precision - tolerance {
        degraded.push("Precision");
    }
    if after.mrr < before.mrr - tolerance {
        degraded.push("MRR");
    }
    if after.contamination > before.contamination + tolerance {
        degraded.push("Contamination");
    }
    degraded
}

fn rankings_stable(before: &[QueryRanking], after: &[QueryRanking]) -> bool {
    before.iter().all(|b| {
        if b.intentionally_altered {
            return true;
        }
        after
            .iter()
            .find(|a| a.query == b.query)
            .is_some_and(|a| a.top_k == b.top_k)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(recall: f64, precision: f64, mrr: f64, contamination: f64) -> Metrics {
        Metrics {
            recall,
            precision,
            mrr,
            contamination,
        }
    }

    fn side(metrics: Metrics, top_k: Vec<&str>, altered: bool) -> RetrievalSide {
        RetrievalSide {
            source_generation: 3,
            corpus_id: "golden".into(),
            metrics,
            queries: vec![QueryRanking {
                query: "q".into(),
                top_k: top_k.into_iter().map(str::to_string).collect(),
                intentionally_altered: altered,
            }],
        }
    }

    #[test]
    fn matching_rankings_and_metrics_pass() {
        let gate = RetrievalGate::new(0.05);
        let before = side(metrics(0.8, 0.5, 0.7, 0.1), vec!["a", "b"], false);
        let after = before.clone();
        let report = gate
            .evaluate(&before, &after)
            .expect("same generation/corpus");
        assert!(report.passed);
        assert!(report.ranking_stable);
        assert!(report.degraded.is_empty());
    }

    #[test]
    fn ranking_drift_on_unaltered_path_fails() {
        let gate = RetrievalGate::new(0.5);
        let before = side(metrics(0.8, 0.5, 0.7, 0.1), vec!["a", "b"], false);
        let mut after = before.clone();
        after.queries[0].top_k = vec!["b".into(), "a".into()];
        let report = gate.evaluate(&before, &after).unwrap();
        assert!(!report.ranking_stable);
        assert!(!report.passed);
    }

    #[test]
    fn refuses_when_source_generation_differs() {
        let gate = RetrievalGate::new(0.05);
        let before = side(metrics(0.8, 0.5, 0.7, 0.1), vec!["a"], false);
        let mut after = before.clone();
        after.source_generation = before.source_generation + 1;
        let err = gate.evaluate(&before, &after).expect_err("26.1 generation");
        let msg = err.to_string();
        assert!(msg.contains("refused"), "{msg}");
        assert!(msg.contains("source_generation"), "{msg}");
    }

    #[test]
    fn refuses_when_corpus_differs() {
        let gate = RetrievalGate::new(0.05);
        let before = side(metrics(0.8, 0.5, 0.7, 0.1), vec!["a"], false);
        let mut after = before.clone();
        after.corpus_id = "other-snapshot".into();
        let err = gate.evaluate(&before, &after).expect_err("26.1 corpus");
        let msg = err.to_string();
        assert!(msg.contains("refused"), "{msg}");
        assert!(msg.contains("corpus"), "{msg}");
    }

    #[test]
    fn reports_recall_precision_mrr_and_contamination_on_both_sides() {
        let gate = RetrievalGate::new(0.05);
        let before = side(metrics(0.80, 0.40, 0.70, 0.10), vec!["a"], false);
        let mut after = before.clone();
        after.metrics = metrics(0.81, 0.41, 0.71, 0.09);
        let report = gate
            .evaluate(&before, &after)
            .expect("same generation/corpus");
        for side_metrics in [report.before, report.after] {
            for value in [
                side_metrics.recall,
                side_metrics.precision,
                side_metrics.mrr,
                side_metrics.contamination,
            ] {
                assert!(value.is_finite(), "26.3 four metrics must be reported");
            }
        }
        assert_eq!(report.before.recall, 0.80);
        assert_eq!(report.before.precision, 0.40);
        assert_eq!(report.before.mrr, 0.70);
        assert_eq!(report.before.contamination, 0.10);
        assert_eq!(report.after.recall, 0.81);
        assert_eq!(report.after.precision, 0.41);
        assert_eq!(report.after.mrr, 0.71);
        assert_eq!(report.after.contamination, 0.09);
    }

    #[test]
    fn reads_tolerance_once_at_construction() {
        let mut declared = 0.05;
        let gate = RetrievalGate::new(declared);
        declared = 0.90;
        let before = side(metrics(0.80, 0.50, 0.70, 0.10), vec!["a"], false);
        let mut after = before.clone();
        after.metrics.recall = 0.70;
        let report = gate.evaluate(&before, &after).unwrap();
        assert_eq!(gate.tolerance(), 0.05, "tolerance is not re-read (26.5)");
        assert!(
            report.degraded.contains(&"Recall"),
            "0.10 drop exceeds the captured 0.05, not the later 0.90: {:?}",
            report.degraded
        );
        let again = gate.evaluate(&before, &after).unwrap();
        assert_eq!(again.degraded, report.degraded);
        assert_eq!(gate.tolerance(), 0.05);
        let _ = declared;
    }

    #[test]
    fn local_snapshot_records_clone_limitation() {
        let local = CorpusIdentity::local_snapshot("tmp-eval");
        assert!(!local.reproducible_from_fresh_clone);
        let note = local.limitation_note().expect("26.7 local snapshot note");
        assert!(note.contains("fresh clone"), "{note}");
        assert!(note.contains("local snapshot"), "{note}");
        assert!(CorpusIdentity::reproducible("golden")
            .limitation_note()
            .is_none());
    }
}
