//! Bug-condition exploration tests for `indexd-vec0-legacy-crash` (Property 1).
//!
//! These tests reproduce the reported defect: a workspace `.cognis/uckg.db`
//! whose `symbol_vec` is a legacy sqlite-vec `vec0` virtual table crashes the
//! indexd open path (`no such module: vec0`) on an engine build that cannot
//! load the `vec0` module, while `health`/`cli` open the same DB and merely
//! report `vector: warn` — so self-heal behaviour diverges by surface.
//!
//! CRITICAL (bug-fix workflow): every assertion below encodes the EXPECTED,
//! post-fix behaviour from design.md Property 1. On the UNFIXED code these
//! tests MUST FAIL — the failure is the counterexample that proves the bug.
//! Task 3.5 re-runs *these same tests* after the fix; they then PASS.
//!
//! The buggy fixture is crafted **module-free** (no sqlite-vec toolchain) on a
//! copy of `crates/cognis-store/tests/fixtures/uckg_oracle.db`: we drop the
//! plain-BLOB `symbol_vec`, add a couple of `symbol_vec_*` shadow tables, and
//! insert a `symbol_vec` `sqlite_master` row whose `sql` is
//! `CREATE VIRTUAL TABLE symbol_vec USING vec0(...)` via
//! `PRAGMA writable_schema=ON`. That makes the schema probe see a `vec0` table
//! while any live `symbol_vec` query (or `DROP`) raises `no such module: vec0`.

use std::fs;
use std::path::{Path, PathBuf};

use cognis_cli::{build_health_report, HealthStatus};
use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::{Database, SymbolWriter};
use tempfile::TempDir;

/// The checked-in plain-BLOB oracle DB the fixture is seeded from
/// (4 symbols / 3 edges / 4 FTS rows / 4 `symbol_vec` rows, `schema_version=1`).
fn oracle_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/cognis-store/tests/fixtures/uckg_oracle.db")
}

/// SQL that rewrites the copied DB's plain-BLOB `symbol_vec` into a legacy
/// `vec0` virtual table plus `symbol_vec_*` shadow tables — module-free.
///
/// Dropping the plain table first removes its `sqlite_autoindex_symbol_vec_1`
/// (otherwise the rewritten master row orphans that index and the DB reopens
/// "malformed"). The virtual-table row is inserted with `rootpage = 0`, which
/// is how SQLite records a virtual table on disk.
const CRAFT_LEGACY_VEC0_SQL: &str = "\
    DROP TABLE IF EXISTS symbol_vec;\n\
    CREATE TABLE IF NOT EXISTS symbol_vec_chunks(chunk_id INTEGER PRIMARY KEY, data BLOB);\n\
    CREATE TABLE IF NOT EXISTS symbol_vec_rowids(rowid INTEGER PRIMARY KEY, symbol_id TEXT);\n\
    INSERT INTO symbol_vec_chunks(chunk_id, data) VALUES (1, x'00');\n\
    INSERT INTO symbol_vec_rowids(rowid, symbol_id) VALUES (1, 's:legacy');\n\
    PRAGMA writable_schema=ON;\n\
    INSERT INTO sqlite_master(type,name,tbl_name,rootpage,sql) \
        VALUES('table','symbol_vec','symbol_vec',0,\
        'CREATE VIRTUAL TABLE symbol_vec USING vec0(symbol_id TEXT PRIMARY KEY, embedding FLOAT[384])');\n\
    PRAGMA writable_schema=OFF;\n";

/// Copy the oracle DB to `dst` and rewrite `symbol_vec` into a legacy `vec0`
/// virtual table. Leaves `dst` as a self-contained bug-condition DB.
fn craft_legacy_vec0(dst: &Path) {
    let src = oracle_src();
    assert!(
        src.exists(),
        "missing oracle fixture {src:?}; it is a checked-in frozen fixture"
    );
    fs::copy(&src, dst).expect("copy oracle fixture");

    let db = Database::open(dst).expect("open copy for crafting");
    {
        let conn = db.connect().expect("connect for crafting");
        conn.execute_batch(CRAFT_LEGACY_VEC0_SQL)
            .expect("craft legacy vec0 schema");
    }
    // Drop the crafting connection so the next open re-reads the schema cookie.
    db.close_thread_connection();
}

/// A bug-condition fixture living at `<dir>/.cognis/uckg.db` (the real workspace
/// layout the CLI/indexd resolve), with the temp dir kept alive by the caller.
struct LegacyFixture {
    _dir: TempDir,
    repo_root: PathBuf,
    db_path: PathBuf,
}

fn legacy_fixture() -> LegacyFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().to_path_buf();
    let cognis_dir = repo_root.join(".cognis");
    fs::create_dir_all(&cognis_dir).expect("mkdir .cognis");
    let db_path = cognis_dir.join("uckg.db");
    craft_legacy_vec0(&db_path);
    LegacyFixture {
        _dir: dir,
        repo_root,
        db_path,
    }
}

/// Config that selects the deterministic `stub` embedder (384-d, no model, no
/// I/O) so `IndexerPipeline::open` builds an embedder and reaches
/// `reconcile_embedding_dim` — the code path that hits the `vec0` `DROP`.
fn stub_embedder_config() -> Config {
    let mut cfg = Config::default();
    cfg.embedder.backend = "stub".into();
    cfg.embedder.dim = 384;
    cfg
}

/// Test case 1 (indexd open crash → self-heal). Expected post-fix behaviour:
/// `IndexerPipeline::open` on a legacy-`vec0` DB opens without crashing, and
/// the healed `symbol_vec` is a queryable plain-BLOB table with data preserved.
///
/// On UNFIXED code this fails: `open` returns
/// `Err(store error: no such module: vec0)` (indexd exit 1). — Req 2.1, 2.2, 2.5
///
/// **Validates: Requirements 1.1, 1.2**
#[test]
fn indexd_open_self_heals_legacy_vec0() {
    let fx = legacy_fixture();

    let opened = IndexerPipeline::open(&fx.db_path, stub_embedder_config());
    assert!(
        opened.is_ok(),
        "indexd open must self-heal a legacy vec0 DB, not crash: {:?}",
        opened.err()
    );

    // The heal outcome: symbol_vec is a plain-BLOB table again (no vec0 module
    // error on a live query), empty pending re-embed on the next index pass.
    let db = Database::open(&fx.db_path).expect("reopen healed DB");
    assert!(
        db.vec_symbol_ids().is_ok(),
        "symbol_vec should be a queryable plain-BLOB table after heal"
    );
    assert_eq!(
        db.vec_row_count().expect("vec_row_count"),
        0,
        "legacy BLOB/vec vectors cleared on heal (repopulated on next index)"
    );
    // Indexed UCKG symbol data is preserved (Req 2.4).
    assert_eq!(db.count("symbol").expect("count symbol"), 4);
    assert_eq!(db.count("edge").expect("count edge"), 3);
    assert_eq!(db.count("symbol_fts").expect("count fts"), 4);

    db.close_thread_connection();
}

/// Test case 2 (broken self-heal → module-free heal). Expected post-fix
/// behaviour: calling `reconcile_embedding_dim` directly on a legacy-`vec0` DB
/// succeeds and leaves a plain-BLOB `symbol_vec`.
///
/// On UNFIXED code this fails: `reconcile_embedding_dim` runs
/// `recreate_vec_table`'s `DROP TABLE symbol_vec`, which must instantiate the
/// missing `vec0` module and raises `no such module: vec0` (root cause #1).
/// — Req 2.2, 2.4
///
/// **Validates: Requirements 1.1, 1.4**
#[test]
fn reconcile_embedding_dim_heals_legacy_vec0_without_module() {
    let fx = legacy_fixture();

    let mut db = Database::open(&fx.db_path).expect("open legacy DB");
    let res = db.reconcile_embedding_dim(384);
    assert!(
        res.is_ok(),
        "reconcile must perform a module-free heal, not DROP a live vec0 vtable: {:?}",
        res.err()
    );
    assert!(
        db.vec_symbol_ids().is_ok(),
        "symbol_vec should be plain BLOB after the reconcile heal"
    );

    db.close_thread_connection();
}

/// Test case 3 (health divergence → consistent heal). Expected post-fix
/// behaviour: opening the same legacy DB through the `health`/`cli` surface
/// applies the SAME heal, so the `vector` check degrades to a non-crashing
/// `warn` AND the legacy `vec0` artifact is gone (plain BLOB, queryable).
///
/// On UNFIXED code this fails: `health` reads `vec_symbol_ids()`, maps the
/// `vec0` error to `warn`, and leaves the legacy table in place — so the
/// consistency assertion (`vec_symbol_ids().is_ok()` after the health open)
/// fails while indexd stays dead. — Req 2.3, 3.4
///
/// **Validates: Requirements 1.3, 1.4**
#[test]
fn health_open_path_heals_legacy_vec0_consistently() {
    // Defensive: this surface resolves the DB via COGNIS_DB_PATH first, then
    // the repo default. Clear any inherited override so we exercise the fixture
    // at `<repo>/.cognis/uckg.db`.
    std::env::remove_var("COGNIS_DB_PATH");
    let fx = legacy_fixture();

    // Opening the same DB through the health/cli surface must not crash.
    let report = build_health_report(&fx.repo_root);
    let vector = report
        .checks
        .iter()
        .find(|(name, _)| name == "vector")
        .map(|(_, check)| check.status);
    assert_eq!(
        vector,
        Some(HealthStatus::Warn),
        "vector check should degrade to warn (empty BLOB) after heal, not crash"
    );

    // Consistency (Req 2.3): the health/cli open path heals identically to
    // indexd, so the legacy vec0 artifact no longer exists — a live symbol_vec
    // query succeeds instead of raising `no such module: vec0`.
    let db = Database::open(&fx.db_path).expect("reopen after health");
    assert!(
        db.vec_symbol_ids().is_ok(),
        "the health/cli open path must apply the same heal as indexd (Req 2.3)"
    );

    db.close_thread_connection();
}

/// Test case 4 (data still present). The fixture's indexed UCKG rows are intact
/// *before* any fix runs, so post-fix preservation assertions are meaningful.
/// This holds on both unfixed and fixed code (it reads only non-`symbol_vec`
/// tables) — it anchors the data-safety guarantee for the heal.
///
/// **Validates: Requirements 1.1**
#[test]
fn legacy_fixture_preserves_symbol_data_before_fix() {
    let fx = legacy_fixture();

    let db = Database::open(&fx.db_path).expect("open legacy DB");
    assert_eq!(db.count("symbol").expect("count symbol"), 4);
    assert_eq!(db.count("edge").expect("count edge"), 3);
    assert_eq!(db.count("symbol_fts").expect("count fts"), 4);

    // And the legacy artifact really is a vec0 table this build can't read:
    // a live symbol_vec query raises `no such module: vec0` on unfixed code.
    // (Documents the crash surface; the heal removes it in tasks 3.x.)
    let ids = db.vec_symbol_ids();
    if let Err(err) = &ids {
        assert!(
            err.to_string().contains("vec0"),
            "expected a vec0 module error from the legacy table, got: {err}"
        );
    }

    db.close_thread_connection();
}
