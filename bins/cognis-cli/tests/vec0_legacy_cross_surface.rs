//! Cross-surface consistency integration test for `indexd-vec0-legacy-crash`
//! (task 4.3).
//!
//! After the heal-on-open pass lands on the shared store open path, every
//! surface that opens a legacy-`vec0` `.cognis/uckg.db` must apply the same
//! outcome and keep reporting its stable JSON contract. This test drives the
//! two operator surfaces the extension consumes — `cognis-cli health --json`
//! and `cognis-cli doctor` — against a crafted legacy-`vec0` DB by spawning the
//! real `cognis-cli` binary (via `CARGO_BIN_EXE_cognis-cli`), and asserts:
//!
//! * neither surface crashes (both exit successfully, emit parseable JSON);
//! * the `health` report still carries the pinned `db` / `index` / `vector`
//!   subchecks with unchanged shape (Req 3.4);
//! * the `doctor` report still carries the pinned `PrerequisiteReport` shape,
//!   including the `semantic_index` item (Req 3.5);
//! * opening the DB after those surfaces have run leaves a queryable plain-BLOB
//!   `symbol_vec` (Req 2.3 — a DB that heals for one path heals for all).
//!
//! The fixture is crafted **module-free** on a copy of the checked-in
//! `crates/cognis-store/tests/fixtures/uckg_oracle.db` (drop the plain-BLOB
//! `symbol_vec`, add `symbol_vec_*` shadow tables, insert a `vec0`
//! `sqlite_master` row via `PRAGMA writable_schema`), so the suite runs under
//! plain `cargo test` with no sqlite-vec toolchain.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cognis_store::Database;
use serde_json::Value;
use tempfile::TempDir;

/// The checked-in plain-BLOB oracle DB the fixture is seeded from
/// (4 symbols / 3 edges / 4 FTS rows / 4 `symbol_vec` rows, `schema_version=1`).
fn oracle_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/cognis-store/tests/fixtures/uckg_oracle.db")
}

/// SQL rewriting the copied DB's plain-BLOB `symbol_vec` into a legacy `vec0`
/// virtual table plus `symbol_vec_*` shadow tables — module-free.
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
/// virtual table (the crafting `Database::open` runs against the plain-BLOB
/// oracle, so heal is a no-op there; the `vec0` rewrite happens afterwards).
fn craft_legacy_vec0(dst: &Path) {
    let src = oracle_src();
    assert!(
        src.exists(),
        "missing oracle fixture {src:?}; it is a checked-in frozen fixture"
    );
    fs::copy(&src, dst).expect("copy oracle fixture");
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
    db.close_thread_connection();
}

/// A bug-condition workspace at `<dir>/.cognis/uckg.db`, kept alive by the dir.
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

/// Run `cognis-cli <args...>` against `repo_root` with `COGNIS_DB_PATH` cleared
/// so the child resolves the fixture DB under the repo (not an inherited
/// override). Returns the parsed stdout JSON, asserting a successful exit.
fn run_cli_json(repo_root: &Path, args: &[&str]) -> Value {
    let exe = env!("CARGO_BIN_EXE_cognis-cli");
    let out = Command::new(exe)
        .arg("--repo-root")
        .arg(repo_root)
        .args(args)
        .env_remove("COGNIS_DB_PATH")
        .output()
        .unwrap_or_else(|e| panic!("run cognis-cli {args:?}: {e}"));

    assert!(
        out.status.success(),
        "cognis-cli {args:?} must not crash on a legacy vec0 DB; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cognis-cli {args:?} must emit parseable JSON: {e}; stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// **Cross-surface consistency (Req 3.4).** `health --json` over a legacy-`vec0`
/// DB does not crash and still emits the pinned report shape with the
/// `db`/`index`/`vector` subchecks (each carrying `status` + `message`).
#[test]
fn health_json_shape_unchanged_on_legacy_vec0_db() {
    let ws = legacy_workspace();

    let json = run_cli_json(&ws.repo_root, &["health", "--json"]);

    assert!(
        json.get("runtime_version").is_some(),
        "health.runtime_version"
    );
    assert!(json.get("overall").is_some(), "health.overall");
    let checks = json.get("checks").expect("health.checks");
    assert!(checks.is_object(), "checks must serialize as a JSON object");
    for key in ["config", "db", "index", "vector"] {
        let check = checks
            .get(key)
            .unwrap_or_else(|| panic!("health report missing `{key}` check"));
        assert!(check.get("status").is_some(), "{key} check has status");
        assert!(check.get("message").is_some(), "{key} check has message");
    }

    // The indexed symbol data survived the heal, so `index` still reports `ok`.
    assert_eq!(
        checks["index"]["status"], "ok",
        "index should stay ok (symbol data preserved) after heal"
    );
    // The legacy vec0 artifact is unreadable-then-healed to empty BLOB, so
    // `vector` degrades to the existing non-crashing `warn` class.
    assert_eq!(
        checks["vector"]["status"], "warn",
        "vector should degrade to warn (empty BLOB), never crash"
    );
}

/// **Cross-surface consistency (Req 3.5).** `doctor` over a legacy-`vec0` DB
/// does not crash and still emits the pinned `PrerequisiteReport` shape
/// (`ready` / `items[]` / `combined_install_target`), including the
/// `semantic_index` item with the full field set.
#[test]
fn doctor_json_shape_unchanged_on_legacy_vec0_db() {
    let ws = legacy_workspace();

    let json = run_cli_json(&ws.repo_root, &["doctor"]);

    assert!(json.get("ready").is_some(), "doctor.ready");
    assert!(
        json.get("combined_install_target").is_some(),
        "doctor.combined_install_target"
    );
    let items = json["items"].as_array().expect("doctor.items array");
    assert!(!items.is_empty(), "doctor should report at least one item");

    for item in items {
        for key in [
            "id",
            "label",
            "description",
            "status",
            "required",
            "install_target",
            "detail",
        ] {
            assert!(item.get(key).is_some(), "doctor item missing `{key}`");
        }
    }

    // The engine + semantic-index prerequisites are still reported (shape pinned).
    let has_item = |id: &str| {
        items
            .iter()
            .any(|it| it["id"] == Value::String(id.to_string()))
    };
    assert!(has_item("engine"), "doctor should report the engine item");
    assert!(
        has_item("semantic_index"),
        "doctor should report the semantic_index item"
    );
}

/// **Heal consistency (Req 2.3).** After both CLI surfaces have opened the
/// legacy DB, re-opening it leaves a queryable plain-BLOB `symbol_vec` — the
/// same post-heal state indexd sees, proving a DB that heals for one path heals
/// for all.
#[test]
fn cli_surfaces_leave_legacy_db_healed_for_all_paths() {
    let ws = legacy_workspace();

    // Exercise both surfaces (each opens the store through the shared path).
    let _ = run_cli_json(&ws.repo_root, &["health", "--json"]);
    let _ = run_cli_json(&ws.repo_root, &["doctor"]);

    // The legacy vec0 artifact is gone: a live symbol_vec query succeeds and
    // the indexed symbol data is intact.
    let db = Database::open(&ws.db_path).expect("re-open after CLI surfaces");
    assert!(
        db.vec_symbol_ids().is_ok(),
        "all open paths must apply the same heal (Req 2.3)"
    );
    assert_eq!(db.count("symbol").expect("count symbol"), 4);
    db.close_thread_connection();
}
