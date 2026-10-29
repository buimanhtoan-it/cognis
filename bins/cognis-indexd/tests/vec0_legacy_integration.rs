//! Integration tests for `indexd-vec0-legacy-crash` (task 4.3).
//!
//! These exercise the real `cognis-indexd` rebuild flow end-to-end against a
//! legacy-`vec0` workspace `.cognis/uckg.db`, asserting the daemon's DB-open
//! path (`IndexerPipeline::open`, the exact call `run_args` makes) self-heals
//! instead of crashing (`no such module: vec0` → exit 1), that a `--full-rebuild`
//! (`index_repo(root, true)`) repopulates the plain-BLOB `symbol_vec`, and that
//! the fixture's indexed UCKG symbol data is preserved byte-for-count across the
//! heal-on-open pass.
//!
//! The buggy fixture is crafted **module-free** (no sqlite-vec toolchain, so the
//! suite runs under plain `cargo test`) on a copy of the checked-in
//! `crates/cognis-store/tests/fixtures/uckg_oracle.db`: we drop the plain-BLOB
//! `symbol_vec`, add a couple of `symbol_vec_*` shadow tables, and insert a
//! `symbol_vec` `sqlite_master` row whose `sql` is
//! `CREATE VIRTUAL TABLE symbol_vec USING vec0(...)` via
//! `PRAGMA writable_schema=ON`. That makes the schema probe see a `vec0` table
//! while any live `symbol_vec` query (or `DROP`) raises `no such module: vec0` —
//! exactly the shape a legacy engine leaves behind.
//!
//! Requirements covered: 2.5 (indexd rebuild repopulates BLOB vectors so
//! semantic search can activate), plus the data-preservation guarantee (Req 2.4)
//! the heal must uphold. Cross-surface `health`/`doctor` consistency (Req 3.4,
//! 3.5) is covered by the companion CLI test
//! (`bins/cognis-cli/tests/vec0_legacy_cross_surface.rs`), which can spawn the
//! `cognis-cli` binary via `CARGO_BIN_EXE_cognis-cli`.

use std::fs;
use std::path::{Path, PathBuf};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;
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
///
/// The `Database::open` used for crafting runs against the *plain-BLOB* oracle
/// (heal is a guarded no-op there); the `vec0` rewrite happens afterwards, so
/// the on-disk fixture is genuinely legacy-`vec0` when the tests re-open it.
fn craft_legacy_vec0(dst: &Path) {
    let src = oracle_src();
    assert!(
        src.exists(),
        "missing oracle fixture {src:?}; it is a checked-in frozen fixture"
    );
    fs::copy(&src, dst).expect("copy oracle fixture");
    // Copied file perms can be read-only on Windows; clear so the heal can write.
    let mut perms = fs::metadata(dst).expect("stat copy").permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(dst, perms).expect("clear readonly on copy");

    let db = Database::open(dst).expect("open copy for crafting");
    {
        let conn = db.connect().expect("connect for crafting");
        conn.execute_batch(CRAFT_LEGACY_VEC0_SQL)
            .expect("craft legacy vec0 schema");
    }
    // Drop the crafting connection so the next open re-reads the schema cookie.
    db.close_thread_connection();
}

/// A bug-condition workspace: `<dir>/.cognis/uckg.db` is a legacy-`vec0` DB and
/// `<dir>` is the repo root indexd watches. The temp dir is kept alive by the
/// caller (dropping it deletes the tree).
struct LegacyWorkspace {
    _dir: TempDir,
    repo_root: PathBuf,
    db_path: PathBuf,
}

fn legacy_workspace() -> LegacyWorkspace {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().to_path_buf();
    let cognis_dir = repo_root.join(".cognis");
    fs::create_dir_all(&cognis_dir).expect("mkdir .cognis");
    let db_path = cognis_dir.join("uckg.db");
    craft_legacy_vec0(&db_path);
    LegacyWorkspace {
        _dir: dir,
        repo_root,
        db_path,
    }
}

/// Config selecting the deterministic `stub` embedder (384-d, no model, no I/O)
/// so `IndexerPipeline::open` builds an embedder and the rebuild pass persists
/// BLOB vectors — the exact daemon config path that reaches the heal on open.
fn stub_embedder_config() -> Config {
    let mut cfg = Config::default();
    cfg.embedder.backend = "stub".into();
    cfg.embedder.dim = 384;
    cfg
}

/// Count rows via a **raw** connection that never runs migrations or the heal,
/// so it reflects the fixture's true on-disk counts before any heal-on-open.
/// Only reads non-`symbol_vec` tables, so it never instantiates the `vec0`
/// module.
fn raw_count(db_path: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("raw open");
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("raw count {table}: {e}"))
}

/// **indexd end-to-end (Req 2.5).** Drive the daemon's rebuild flow against a
/// legacy-`vec0` workspace: `IndexerPipeline::open` (the call `run_args` makes,
/// which crashed indexd with exit 1) must self-heal and return `Ok`, and a
/// `--full-rebuild` (`index_repo(root, true)`) must repopulate the plain-BLOB
/// `symbol_vec` so semantic search can activate (`vec_row_count > 0`).
#[test]
fn indexd_rebuild_flow_heals_legacy_vec0_and_repopulates_vectors() {
    let ws = legacy_workspace();

    // A small synthetic source file so the full rebuild has something to embed
    // (kept tiny — the point is the rebuild pass runs and persists BLOB vectors,
    // not to index a large repo).
    fs::write(
        ws.repo_root.join("alpha.py"),
        b"def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
    )
    .expect("write synthetic source");

    // 1. Daemon open path: self-heals instead of exiting 1.
    let opened = IndexerPipeline::open(&ws.db_path, stub_embedder_config());
    assert!(
        opened.is_ok(),
        "indexd open must self-heal a legacy vec0 DB (no exit 1): {:?}",
        opened.err()
    );
    let mut pipeline = opened.expect("pipeline");

    // 2. `--full-rebuild`: re-embeds BLOB vectors for the walked repo.
    let stats = pipeline
        .index_repo(&ws.repo_root, true)
        .expect("full rebuild must succeed on a healed DB");
    assert!(
        stats.symbols_indexed >= 1,
        "full rebuild should index the synthetic source (got {} symbols)",
        stats.symbols_indexed
    );

    // 3. BLOB vectors are repopulated and the table is a queryable plain BLOB.
    let db = pipeline.database();
    assert!(
        db.vec_symbol_ids().is_ok(),
        "symbol_vec must be a queryable plain-BLOB table after heal"
    );
    let vec_rows = db.vec_row_count().expect("vec_row_count");
    assert!(
        vec_rows > 0,
        "full rebuild must repopulate BLOB vectors (vec_row_count = {vec_rows})"
    );

    pipeline.database().close_thread_connection();
}

/// **Data preservation (Req 2.4).** The fixture's indexed UCKG row counts
/// (`symbol` / `edge` / `symbol_fts` / `symbol_attribute`) are identical before
/// (raw, pre-heal) and after opening through the healed store path — the heal
/// removes only the legacy vector artifacts, never the indexed symbol data.
#[test]
fn heal_on_open_preserves_indexed_row_counts() {
    let ws = legacy_workspace();
    let tables = ["symbol", "edge", "symbol_fts", "symbol_attribute"];

    // Before: raw counts straight off the crafted legacy DB (no heal runs).
    let before: Vec<(&str, i64)> = tables
        .iter()
        .map(|t| (*t, raw_count(&ws.db_path, t)))
        .collect();
    // Sanity: the seeded oracle really has indexed rows to preserve.
    assert_eq!(before[0].1, 4, "fixture should carry 4 seeded symbols");

    // Open through the healed store path (this is where heal-on-open fires).
    let db = Database::open(&ws.db_path).expect("open must self-heal, not crash");
    // The legacy vec0 artifact is gone — a live symbol_vec query succeeds.
    assert!(
        db.vec_symbol_ids().is_ok(),
        "symbol_vec should be plain BLOB after heal-on-open"
    );

    // After: counts via the healed handle are identical to the raw pre-heal set.
    for (table, before_count) in &before {
        let after = db
            .count(table)
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        assert_eq!(
            after, *before_count,
            "row count for `{table}` changed across heal-on-open ({before_count} → {after})"
        );
    }

    db.close_thread_connection();
}
