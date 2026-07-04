//! Semantic vec-KNN parity test (rust-engine-migration, task 3.3 / Req 2.4, 4.2).
//!
//! Asserts `SymbolStore::vec_search` returns the same top-k (ordering + per-hit
//! distance/score within numeric tolerance) as the Python oracle on the same DB
//! — P-PAR-VEC: `∀ query: top-k vec KNN Rust ≈ Python trong tol số học`.
//!
//! The checked-in fixture `tests/fixtures/uckg_oracle.db` (a static SQLite DB,
//! not a Python dependency) stores embeddings in the plain-BLOB `symbol_vec`
//! fallback shape (captured with sqlite-vec forced off), so this exercises the
//! **fallback
//! linear-scan path** — the path that runs whenever sqlite-vec can't be loaded
//! (Requirement 2.4), and the only one available in an offline CI.
//!
//! The oracle is captured in `tests/fixtures/vec_parity_golden.json` from the
//! Python oracle's cosine KNN over the same
//! BLOB vectors. Capturing the golden lets this run under plain `cargo test`
//! with no Python runtime, mirroring `tests/fts_parity.rs`. Query vectors in the
//! golden are f32-quantised so the Rust `&[f32]` query and the reference float64
//! computation use identical numbers and agree on ordering. The golden and the
//! fixture DB are checked in as frozen oracle output; there is no toolchain in
//! this repo to regenerate them.

use std::fs;
use std::path::PathBuf;

use cognis_store::{Database, SymbolStore};
use serde_json::Value;

/// Distance/score tolerance. The fallback accumulates in f64 from the same f32
/// values the oracle uses, so agreement is near-exact; this guards only against
/// platform libm rounding in `sqrt`.
const TOL: f64 = 1e-9;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the fixture DB into a temp dir so the test never mutates the checked-in
/// file (WAL mode would otherwise create sidecar files next to it).
fn open_temp_copy(tmp: &tempfile::TempDir, db_name: &str) -> Database {
    let src = fixtures_dir().join(db_name);
    assert!(
        src.exists(),
        "missing fixture {src:?}; it is a checked-in frozen oracle fixture"
    );
    let dst = tmp.path().join(db_name);
    fs::copy(&src, &dst).expect("copy fixture");
    Database::open(&dst).expect("open Python-built DB")
}

fn load_golden() -> Value {
    let path = fixtures_dir().join("vec_parity_golden.json");
    assert!(
        path.exists(),
        "missing golden {path:?}; it is a checked-in frozen oracle fixture"
    );
    let text = fs::read_to_string(&path).expect("read golden");
    serde_json::from_str(&text).expect("parse golden json")
}

fn query_vec(case: &Value) -> Vec<f32> {
    case["query"]
        .as_array()
        .expect("case.query")
        .iter()
        .map(|v| v.as_f64().expect("query component is number") as f32)
        .collect()
}

#[test]
fn vec_search_topk_matches_python_oracle() {
    let golden = load_golden();
    let db_name = golden["db"].as_str().expect("golden.db");
    let k = golden["k"].as_u64().expect("golden.k") as usize;
    let cases = golden["cases"].as_array().expect("golden.cases");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, db_name);

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let q = query_vec(case);
        let expected = case["expected"].as_array().expect("case.expected");

        let hits = db.vec_search(&q, k).expect("vec_search");

        // Same number of hits and identical symbol-id ordering (nearest first).
        assert_eq!(
            hits.len(),
            expected.len(),
            "hit count diverges from oracle for case {name:?}"
        );
        for (i, (hit, exp)) in hits.iter().zip(expected.iter()).enumerate() {
            let exp_id = exp["symbol_id"].as_str().expect("expected.symbol_id");
            assert_eq!(
                hit.symbol_id, exp_id,
                "rank {i} symbol id diverges from oracle for case {name:?}"
            );

            // Score = max(0, 1 - distance) within tolerance.
            let exp_score = exp["score"].as_f64().expect("expected.score");
            assert!(
                (hit.score - exp_score).abs() <= TOL,
                "rank {i} score {} vs oracle {exp_score} (case {name:?})",
                hit.score
            );

            // evidence.score carries the raw KNN distance (semantic.py shape).
            let exp_dist = exp["distance"].as_f64().expect("expected.distance");
            let got_dist = hit
                .evidence
                .get("score")
                .and_then(Value::as_f64)
                .expect("hit evidence.score");
            assert!(
                (got_dist - exp_dist).abs() <= TOL,
                "rank {i} distance {got_dist} vs oracle {exp_dist} (case {name:?})"
            );

            // Semantic-layer hit contract (shape the mesh / MCP rely on).
            assert_eq!(hit.layer, "semantic", "rank {i} layer (case {name:?})");
            assert!(
                hit.reason.contains("KNN cosine distance"),
                "rank {i} reason (case {name:?}): {}",
                hit.reason
            );
        }
    }

    db.close_thread_connection();
}

#[test]
fn vec_search_self_query_returns_self_top1() {
    // CP-5: searching by a symbol's own embedding returns that symbol top-1.
    let golden = load_golden();
    let cases = golden["cases"].as_array().expect("golden.cases");
    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, golden["db"].as_str().unwrap());

    let mut self_cases = 0usize;
    for case in cases {
        let name = case["name"].as_str().unwrap_or("");
        let Some(self_id) = name.strip_prefix("self::") else {
            continue;
        };
        let q = query_vec(case);
        let hits = db.vec_search(&q, 10).expect("vec_search");
        assert_eq!(
            hits.first().map(|h| h.symbol_id.as_str()),
            Some(self_id),
            "self-query {name:?} must rank its own symbol top-1"
        );
        // A self-match is direction-identical → distance ≈ 0, score ≈ 1.
        assert!(
            hits[0].score >= 1.0 - 1e-6,
            "self-match score should be ≈ 1, got {}",
            hits[0].score
        );
        self_cases += 1;
    }
    assert!(self_cases >= 1, "golden should contain self:: cases");

    db.close_thread_connection();
}

#[test]
fn vec_search_graceful_edge_cases() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, "uckg_oracle.db");

    // Empty query and k = 0 are graceful no-ops, not errors.
    assert!(db.vec_search(&[], 10).expect("empty query").is_empty());
    let some = vec![0.0f32; 384];
    assert!(db.vec_search(&some, 0).expect("k=0").is_empty());

    // k caps the result; the fixture has 4 vec rows.
    let one = db.vec_search(&some, 1).expect("k=1");
    assert_eq!(one.len(), 1, "k=1 returns exactly one hit");

    db.close_thread_connection();
}
