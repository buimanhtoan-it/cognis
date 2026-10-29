//! Live Retrieval_Gate runner over real databases (task 20.2 / Requirement 26).
//!
//! Pins `source_generation`, ranks a golden set on the lexical channel, and
//! compares before/after. Does not fabricate metrics: unresolved queries are
//! skipped, and a generation/corpus mismatch refuses the run.

use std::collections::HashSet;

use cognis_core::{Symbol, SymbolKind};
use cognis_eval::bench::parse_golden_set;
use cognis_eval::gate::{CorpusIdentity, RetrievalGate, DEFAULT_RETRIEVAL_TOLERANCE};
use cognis_store::{Database, IndexPublisher, PublishBatch, PublishReason, SymbolWriter};

fn sym(id: &str, name: &str, file_path: &str, body: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "python".to_string(),
        module: "m".to_string(),
        file_path: file_path.to_string(),
        line_start: 1,
        line_end: 2,
        signature: None,
        docstring: None,
        content_hash: "h".to_string(),
        body_excerpt: Some(body.to_string()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: vec![],
        updated_at: 0,
    }
}

fn baseline_symbols() -> Vec<Symbol> {
    vec![
        sym(
            "python:src/models.py:prepare_body@h1",
            "prepare_body",
            "src/models.py",
            "prepare the body stream",
        ),
        sym(
            "python:src/models.py:PreparedRequest@h2",
            "PreparedRequest",
            "src/models.py",
            "the prepared request",
        ),
        sym(
            "python:src/utils.py:super_len@h3",
            "super_len",
            "src/utils.py",
            "length of a stream",
        ),
        sym(
            "python:src/utils.py:helper@h4",
            "helper",
            "src/utils.py",
            "unrelated helper",
        ),
    ]
}

fn golden() -> cognis_eval::bench::GoldenSet {
    parse_golden_set(
        r#"{
            "repo": "synthetic-gate",
            "queries": [
                {"q": "Fix prepare_body stream detection",
                 "relevant": [["prepare_body", "src/models.py"]]},
                {"q": "super_len fix",
                 "relevant": [["super_len", "src/utils.py"]]}
            ]
        }"#,
    )
    .expect("golden JSON")
}

fn open_indexed(path: &std::path::Path, symbols: &[Symbol]) -> Database {
    let mut db = Database::open(path).expect("open db");
    db.upsert_symbols(symbols).expect("upsert");
    db
}

#[test]
fn live_before_after_same_generation_and_corpus_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("same.db");
    let db = open_indexed(&path, &baseline_symbols());
    let corpus = CorpusIdentity::local_snapshot("synthetic-gate");
    let golden = golden();
    let altered = HashSet::new();

    let mut declared = DEFAULT_RETRIEVAL_TOLERANCE;
    let gate = RetrievalGate::new(declared);
    declared = 0.90;
    assert_eq!(gate.tolerance(), DEFAULT_RETRIEVAL_TOLERANCE);

    let first = gate
        .compare(&db, &db, &golden, &corpus, 2, &altered)
        .expect("same generation/corpus");
    let second = gate
        .compare(&db, &db, &golden, &corpus, 2, &altered)
        .expect("deterministic re-run");

    assert!(
        first.report.passed,
        "unaltered path must pass: {:?}",
        first.report
    );
    assert!(first.report.ranking_stable);
    assert!(first.report.degraded.is_empty());
    assert_eq!(first.report, second.report, "26.6 identical metrics");
    assert_eq!(first.before.metrics, second.before.metrics);
    assert_eq!(first.after.queries, second.after.queries);
    for side in [&first.report.before, &first.report.after] {
        for value in [side.recall, side.precision, side.mrr, side.contamination] {
            assert!(value.is_finite(), "26.3 four metrics");
            assert!((0.0..=1.0).contains(&value));
        }
    }
    assert_eq!(first.corpus.id, "synthetic-gate");
    assert!(!first.corpus.reproducible_from_fresh_clone);
    let note = first.corpus_limitation().expect("26.7");
    assert!(note.contains("fresh clone"), "{note}");
    assert_eq!(gate.tolerance(), DEFAULT_RETRIEVAL_TOLERANCE);
    let _ = declared;

    db.close_thread_connection();
}

#[test]
fn refuses_when_committed_generations_differ() {
    let tmp = tempfile::tempdir().unwrap();
    let before_path = tmp.path().join("before.db");
    let after_path = tmp.path().join("after.db");
    let symbols = baseline_symbols();
    let before = open_indexed(&before_path, &symbols);
    let mut after = open_indexed(&after_path, &symbols);
    after
        .publish(&PublishBatch::empty(PublishReason::Reconcile))
        .expect("advance after generation");

    let corpus = CorpusIdentity::local_snapshot("synthetic-gate");
    let golden = golden();
    let gate = RetrievalGate::new(DEFAULT_RETRIEVAL_TOLERANCE);
    let err = gate
        .compare(&before, &after, &golden, &corpus, 2, &HashSet::new())
        .expect_err("26.1 generation mismatch");
    let msg = err.to_string();
    assert!(msg.contains("refused"), "{msg}");
    assert!(msg.contains("source_generation"), "{msg}");

    before.close_thread_connection();
    after.close_thread_connection();
}

#[test]
fn refuses_when_corpus_identity_differs() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("corpus.db");
    let db = open_indexed(&path, &baseline_symbols());
    let golden = golden();
    let before = RetrievalGate::measure(
        &db,
        &golden,
        &CorpusIdentity::local_snapshot("alpha"),
        2,
        &HashSet::new(),
    )
    .unwrap();
    let after = RetrievalGate::measure(
        &db,
        &golden,
        &CorpusIdentity::local_snapshot("beta"),
        2,
        &HashSet::new(),
    )
    .unwrap();
    let err = RetrievalGate::new(DEFAULT_RETRIEVAL_TOLERANCE)
        .evaluate(&before, &after)
        .expect_err("26.1 corpus mismatch");
    let msg = err.to_string();
    assert!(msg.contains("refused"), "{msg}");
    assert!(msg.contains("corpus"), "{msg}");
    db.close_thread_connection();
}

#[test]
fn ranking_drift_on_unaltered_path_fails_the_change() {
    let tmp = tempfile::tempdir().unwrap();
    let before_path = tmp.path().join("rank-before.db");
    let after_path = tmp.path().join("rank-after.db");
    let before_symbols = baseline_symbols();
    let mut after_symbols = before_symbols.clone();
    after_symbols.push(sym(
        "python:src/models.py:prepare_body_stream@h9",
        "prepare_body_stream",
        "src/models.py",
        "Fix prepare_body stream detection",
    ));

    let before = open_indexed(&before_path, &before_symbols);
    let after = open_indexed(&after_path, &after_symbols);
    let corpus = CorpusIdentity::local_snapshot("synthetic-gate");
    let golden = golden();
    let run = RetrievalGate::new(DEFAULT_RETRIEVAL_TOLERANCE)
        .compare(&before, &after, &golden, &corpus, 2, &HashSet::new())
        .expect("same generation");
    assert_eq!(run.before.source_generation, run.after.source_generation);
    assert!(
        !run.report.ranking_stable,
        "top-k must drift: {:?}",
        run.after.queries
    );
    assert!(!run.report.passed);

    before.close_thread_connection();
    after.close_thread_connection();
}

#[test]
fn metric_degradation_beyond_tolerance_fails_the_change() {
    let tmp = tempfile::tempdir().unwrap();
    let before_path = tmp.path().join("metric-before.db");
    let after_path = tmp.path().join("metric-after.db");
    let before = open_indexed(&before_path, &baseline_symbols());
    let after = open_indexed(
        &after_path,
        &[sym(
            "python:src/utils.py:helper@h4",
            "helper",
            "src/utils.py",
            "unrelated helper",
        )],
    );
    let corpus = CorpusIdentity::local_snapshot("synthetic-gate");
    let golden = golden();
    let mut altered = HashSet::new();
    altered.insert("Fix prepare_body stream detection".into());
    altered.insert("super_len fix".into());

    let run = RetrievalGate::new(0.0)
        .compare(&before, &after, &golden, &corpus, 2, &altered)
        .expect("same generation; rankings waived");
    assert!(
        !run.report.degraded.is_empty(),
        "recall/precision/mrr must drop when relevant symbols vanish: {:?}",
        run.report
    );
    assert!(!run.report.passed);
    assert!(run.report.before.recall > run.report.after.recall);

    before.close_thread_connection();
    after.close_thread_connection();
}
