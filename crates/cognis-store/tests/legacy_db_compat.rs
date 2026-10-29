//! Compatibility test (rust-engine-migration, task 3.1 / Requirement 2.1, 2.2).
//!
//! Opens a `.cognis/uckg.db` **created by the legacy engine** (the frozen parity
//! oracle fixture) and reads symbol / edge / FTS5 / vec without error, proving:
//!
//!  * `run_migrations` is an idempotent no-op on a DB already at the latest
//!    schema version (no break-the-world migrate),
//!  * the `symbol` and `edge` columns map cleanly back into the core models
//!    (table/column names preserved, `node_id` format intact),
//!  * the contentless `symbol_fts` FTS5 table is queryable, and
//!  * the BLOB-fallback `symbol_vec` table is readable.
//!
//! The fixture at `tests/fixtures/uckg_oracle.db` is a static SQLite DB that was
//! generated once (during the Rust migration) by the legacy engine's migration
//! runner + write helpers, with sqlite-vec forced off (the portable CI shape).
//! It is checked in as a frozen compatibility oracle; it is test data only and
//! carries no runtime dependency — there is no toolchain here to regenerate it.

use std::fs;
use std::path::PathBuf;

use cognis_core::EdgeKind;
use cognis_store::{Database, LATEST_SCHEMA_VERSION};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/uckg_oracle.db")
}

/// Copy the fixture into a temp dir so the test never mutates the checked-in
/// file (opening in WAL mode would otherwise create sidecar files next to it).
fn open_temp_copy(tmp: &tempfile::TempDir) -> Database {
    let src = fixture_path();
    assert!(
        src.exists(),
        "missing fixture {src:?}; it is a checked-in frozen compatibility fixture"
    );
    let dst = tmp.path().join("uckg_oracle.db");
    fs::copy(&src, &dst).expect("copy fixture");
    Database::open(&dst).expect("open oracle fixture DB")
}

#[test]
fn opens_legacy_built_db_without_breaking_migrate() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp);

    // The legacy engine wrote schema_version=1; our migration runner must treat
    // an at-version DB as a no-op (Requirement 2.1).
    assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
    let conn = db.connect().unwrap();
    let applied = cognis_store::run_migrations(&conn).unwrap();
    assert_eq!(
        applied, LATEST_SCHEMA_VERSION,
        "re-running migrations must no-op"
    );

    db.close_thread_connection();
}

#[test]
fn reads_symbol_edge_fts_vec_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp);

    // symbol — full column mapping round-trips into the core model.
    let symbols = db.list_symbols().expect("read symbols");
    assert_eq!(symbols.len(), 4, "fixture has 4 symbols");
    for s in &symbols {
        s.validate()
            .expect("legacy-written symbol passes core validation");
        // node_id format <lang>:<path>:<qname>@<hash> is preserved (Req 2.2).
        assert!(
            s.id.starts_with("python:src/mod.py:") && s.id.contains('@'),
            "unexpected node_id shape: {}",
            s.id
        );
    }

    // edge — including the meta.dst_missing convention (Req 2.2).
    let edges = db.list_edges().expect("read edges");
    assert_eq!(edges.len(), 3, "fixture has 3 edges");
    let dangling: Vec<_> = edges.iter().filter(|e| e.dst_missing()).collect();
    assert_eq!(dangling.len(), 1, "one edge carries meta.dst_missing=true");
    assert_eq!(dangling[0].kind, EdgeKind::Imports);
    assert!(edges.iter().any(|e| e.kind == EdgeKind::Calls));

    // FTS5 — contentless symbol_fts is queryable and returns indexed ids.
    let hits = db.fts_match_ids("request", 10).expect("fts query");
    assert!(
        hits.iter().all(|id| symbols.iter().any(|s| &s.id == id)),
        "every FTS hit id resolves to a known symbol"
    );
    // A token we know is indexed (docstrings mention "requests"/"tokens").
    let token_hits = db.fts_match_ids("tokens", 10).expect("fts query 2");
    assert!(
        !token_hits.is_empty(),
        "expected at least one FTS match for 'tokens'"
    );

    // vec — BLOB-fallback symbol_vec is readable; one row per symbol.
    let vec_ids = db.vec_symbol_ids().expect("read symbol_vec");
    assert_eq!(vec_ids.len(), 4, "fixture has 4 vec rows");
    for id in &vec_ids {
        assert!(symbols.iter().any(|s| &s.id == id));
    }

    db.close_thread_connection();
}
