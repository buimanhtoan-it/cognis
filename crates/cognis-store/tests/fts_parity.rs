//! FTS5 parity test (rust-engine-migration, task 3.2 / Requirement 4.2).
//!
//! Asserts `SymbolStore::fts_search` returns a hit set (symbol ids) **identical
//! to the Python oracle** on the same DB — P-PAR-FTS:
//! `∀ query: tập symbol_id FTS5 Rust == Python (cùng DB, cùng k)`.
//!
//! The oracle is captured in `tests/fixtures/fts_parity_golden.json` from the
//! Python oracle — the exact `symbol_fts MATCH` query the engine's lexical layer
//! issued against the checked-in fixture `uckg_oracle.db`. Capturing the
//! golden lets this run under plain `cargo test` with no Python runtime,
//! mirroring the approach in `tests/python_db_compat.rs`. The golden and the
//! fixture DB are checked in as frozen oracle output; there is no toolchain in
//! this repo to regenerate them.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use cognis_store::{Database, SymbolStore};
use serde_json::Value;

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
    Database::open(&dst).expect("open oracle fixture DB")
}

fn load_golden() -> Value {
    let path = fixtures_dir().join("fts_parity_golden.json");
    assert!(
        path.exists(),
        "missing golden {path:?}; it is a checked-in frozen oracle fixture"
    );
    let text = fs::read_to_string(&path).expect("read golden");
    serde_json::from_str(&text).expect("parse golden json")
}

#[test]
fn fts_search_hit_set_matches_python_oracle() {
    let golden = load_golden();
    let db_name = golden["db"].as_str().expect("golden.db");
    let k = golden["k"].as_u64().expect("golden.k") as usize;
    let cases = golden["cases"].as_array().expect("golden.cases");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, db_name);

    let mut checked_nonempty = 0usize;
    for case in cases {
        let query = case["query"].as_str().expect("case.query");
        let expected: BTreeSet<String> = case["expected_ids"]
            .as_array()
            .expect("case.expected_ids")
            .iter()
            .map(|v| v.as_str().expect("id is string").to_string())
            .collect();

        let hits = db.fts_search(query, k).expect("fts_search");
        let actual: BTreeSet<String> = hits.iter().map(|h| h.symbol_id.clone()).collect();

        assert_eq!(
            actual, expected,
            "FTS5 hit set diverges from Python oracle for query {query:?}"
        );

        // Every hit must carry the lexical layer contract: layer + a reason +
        // a snippet evidence key (shape the retrieval mesh / MCP rely on).
        for h in &hits {
            assert_eq!(h.layer, "lexical", "hit layer for query {query:?}");
            assert!(!h.reason.is_empty(), "hit reason for query {query:?}");
            assert!(
                h.evidence.get("snippet").is_some(),
                "hit evidence.snippet for query {query:?}"
            );
        }
        if !expected.is_empty() {
            checked_nonempty += 1;
        }
    }
    assert!(
        checked_nonempty >= 1,
        "golden should exercise at least one matching query"
    );

    db.close_thread_connection();
}

#[test]
fn fts_search_respects_k_limit_and_blank_query() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, "uckg_oracle.db");

    // "tokens" matches all 4 fixture symbols; k caps the result.
    let top1 = db.fts_search("tokens", 1).expect("fts_search k=1");
    assert_eq!(top1.len(), 1, "k=1 must return exactly one hit");
    assert_eq!(top1[0].layer, "lexical");

    // Blank query and k=0 are no-ops (graceful), not errors.
    assert!(db.fts_search("   ", 10).expect("blank").is_empty());
    assert!(db.fts_search("tokens", 0).expect("k=0").is_empty());

    // A malformed FTS5 query degrades to empty rather than erroring.
    assert!(
        db.fts_search("\"unterminated", 10)
            .expect("malformed degrades")
            .is_empty(),
        "malformed FTS5 query should degrade to an empty result"
    );

    db.close_thread_connection();
}
