//! Preservation property tests for `indexd-vec0-legacy-crash` (Property 2).
//!
//! These tests capture the BASELINE behaviour of NON-legacy DBs — the cases
//! where `isBugCondition` returns false (plain-BLOB `symbol_vec`, a DB with no
//! `symbol_vec`, and a fresh DB). Per the bug-fix workflow they MUST PASS on the
//! UNFIXED code: they pin the behaviour the module-free self-heal (tasks 3.x)
//! must preserve byte-for-byte. Task 3.6 re-runs *these same tests* after the
//! fix; they must still pass (no regressions).
//!
//! Methodology is observation-first: for a generated non-legacy DB we open it,
//! record the actual open/migrate result and `symbol_vec` contents, then re-open
//! through a fresh handle (a second "open path") and assert the recorded state
//! is unchanged. The `vec_search` BLOB parity and `health`/`doctor` JSON shapes
//! are pinned against the checked-in `vec_parity` golden and the real CLI
//! surfaces, mirroring `crates/cognis-store/tests/vec_parity.rs`.
//!
//! Requirements covered: 3.1 (plain-BLOB unchanged), 3.3 (fresh / no-symbol_vec
//! open+migrate no-op), 3.4 (health JSON), 3.5 (doctor JSON), 3.6 (vec_search
//! BLOB parity). Req 3.2 (vec0-with-extension) is gated behind `sqlite-vec`.

use std::fs;
use std::path::{Path, PathBuf};

use cognis_cli::{build_health_report, HealthStatus};
use cognis_core::{Symbol, SymbolKind};
use cognis_store::{run_migrations, Database, SymbolStore, SymbolWriter, LATEST_SCHEMA_VERSION};
use proptest::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures + helpers
// ---------------------------------------------------------------------------

/// The checked-in plain-BLOB oracle DB (4 symbols / 3 edges / 4 FTS rows /
/// 4 plain-BLOB `symbol_vec` rows, `schema_version = 1`). This is a non-legacy
/// DB — `symbol_vec` is the BLOB fallback, so `isBugCondition` is false.
fn oracle_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/cognis-store/tests/fixtures/uckg_oracle.db")
}

/// The checked-in `vec_search` parity golden (Python cosine-KNN over the same
/// BLOB vectors). Reused here to assert the BLOB top-k is unchanged (Req 3.6).
fn vec_parity_golden() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/cognis-store/tests/fixtures/vec_parity_golden.json")
}

/// Copy the oracle DB into `dst` and ensure it is writable (so `check_db`
/// reports `ok` rather than a spurious read-only `fail` from copied perms).
fn copy_oracle_to(dst: &Path) {
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
}

/// A repo layout `<dir>/.cognis/uckg.db` holding the non-legacy oracle DB.
struct OracleRepo {
    _dir: TempDir,
    repo_root: PathBuf,
}

fn oracle_repo() -> OracleRepo {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().to_path_buf();
    let cognis_dir = repo_root.join(".cognis");
    fs::create_dir_all(&cognis_dir).expect("mkdir .cognis");
    let db_path = cognis_dir.join("uckg.db");
    copy_oracle_to(&db_path);
    OracleRepo {
        _dir: dir,
        repo_root,
    }
}

/// The `CREATE` SQL recorded for `symbol_vec` in `sqlite_master`, or `None` when
/// the table is absent. The plain-BLOB fallback records a `CREATE TABLE ...
/// embedding BLOB ...`; a legacy vec0 table would record `USING vec0`.
fn symbol_vec_sql(db: &Database) -> Option<String> {
    let conn = db.connect().expect("connect");
    conn.query_row(
        "SELECT sql FROM sqlite_master \
         WHERE type IN ('table','view') AND name = 'symbol_vec'",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .unwrap_or_default()
}

/// The raw `(symbol_id, embedding-bytes)` rows of `symbol_vec`, ordered by id.
/// Used to assert re-opening a plain-BLOB DB leaves the stored bytes untouched.
fn read_vec_rows(db: &Database) -> Vec<(String, Vec<u8>)> {
    let conn = db.connect().expect("connect");
    let mut stmt = conn
        .prepare("SELECT symbol_id, embedding FROM symbol_vec ORDER BY symbol_id")
        .expect("prepare symbol_vec read");
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .expect("query symbol_vec")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect symbol_vec rows");
    rows
}

/// Minimal valid [`Symbol`] for a generated id (satisfies the NOT NULL columns
/// and `Symbol::validate`). Only the fields the FK + row-count assertions touch
/// matter here.
fn make_symbol(id: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: id.to_string(),
        qualified_name: format!("m::{id}"),
        language: "rust".to_string(),
        module: "m".to_string(),
        file_path: "src/m.rs".to_string(),
        line_start: 1,
        line_end: 2,
        signature: None,
        docstring: None,
        content_hash: "hash".to_string(),
        body_excerpt: None,
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.1 — plain-BLOB symbol_vec DB opens with form + rows unchanged
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// A DB whose `symbol_vec` is the plain-BLOB fallback (random symbols +
    /// random BLOB vectors) re-opens through a fresh handle with the table
    /// FORM and the stored ROWS byte-for-byte unchanged, and `run_migrations`
    /// stays a no-op at `LATEST_SCHEMA_VERSION`.
    ///
    /// **Validates: Requirements 3.1**
    #[test]
    fn plain_blob_symbol_vec_opens_unchanged(
        raw in prop::collection::vec(
            (0u32..64u32, prop::collection::vec(-5.0f32..5.0f32, 8..=8)),
            0..6,
        )
    ) {
        // Dedupe by id so every row targets a distinct symbol.
        let mut seen = std::collections::BTreeSet::new();
        let mut symbols = Vec::new();
        let mut emb_rows: Vec<(String, Vec<f32>)> = Vec::new();
        for (n, vec) in raw {
            let id = format!("s:{n}");
            if seen.insert(id.clone()) {
                symbols.push(make_symbol(&id));
                emb_rows.push((id, vec));
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("uckg.db");

        // --- Build the plain-BLOB DB and record the baseline. ---
        let (base_form, base_ids, base_rows, base_vec_count, base_sym_count) = {
            let mut db = Database::open(&db_path).expect("open fresh DB");
            db.upsert_symbols(&symbols).expect("upsert symbols");
            db.upsert_embeddings(&emb_rows).expect("upsert embeddings");

            let form = symbol_vec_sql(&db);
            let ids = db.vec_symbol_ids().expect("vec_symbol_ids");
            let rows = read_vec_rows(&db);
            let vec_count = db.vec_row_count().expect("vec_row_count");
            let sym_count = db.count("symbol").expect("count symbol");
            db.close_thread_connection();
            (form, ids, rows, vec_count, sym_count)
        };

        // The baseline really is the plain-BLOB fallback, not a vec0 vtable.
        let form_text = base_form.clone().expect("symbol_vec must exist");
        prop_assert!(
            form_text.to_ascii_uppercase().contains("BLOB"),
            "baseline symbol_vec should be the plain-BLOB fallback: {form_text}"
        );
        prop_assert!(
            !form_text.to_ascii_uppercase().contains("USING VEC0"),
            "baseline symbol_vec must not be a vec0 vtable: {form_text}"
        );

        // --- Re-open through a fresh handle (a second open path). ---
        let (re_migrate, re_version, re_form, re_ids, re_rows, re_vec_count, re_sym_count) = {
            let db2 = Database::open(&db_path).expect("re-open DB");
            let conn = db2.connect().expect("connect re-open");
            let migrate = run_migrations(&conn).expect("run_migrations re-open");
            let version = db2.schema_version().expect("schema_version");
            let form = symbol_vec_sql(&db2);
            let ids = db2.vec_symbol_ids().expect("vec_symbol_ids re-open");
            let rows = read_vec_rows(&db2);
            let vec_count = db2.vec_row_count().expect("vec_row_count re-open");
            let sym_count = db2.count("symbol").expect("count symbol re-open");
            db2.close_thread_connection();
            (migrate, version, form, ids, rows, vec_count, sym_count)
        };

        // Open + migrate is a no-op at the latest schema version.
        prop_assert_eq!(re_migrate, LATEST_SCHEMA_VERSION);
        prop_assert_eq!(re_version, LATEST_SCHEMA_VERSION);
        // Table form + stored rows are byte-for-byte unchanged.
        prop_assert_eq!(re_form, base_form);
        prop_assert_eq!(re_ids, base_ids);
        prop_assert_eq!(re_rows, base_rows);
        prop_assert_eq!(re_vec_count, base_vec_count);
        prop_assert_eq!(re_sym_count, base_sym_count);
    }
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.3 — fresh / no-symbol_vec DBs open + migrate as before
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// A fresh DB (and a DB with its `symbol_vec` removed) opens, migrates once
    /// to `LATEST_SCHEMA_VERSION`, and re-opens with `run_migrations` a no-op —
    /// symbol rows preserved and `symbol_vec` presence unchanged (absent stays
    /// absent; present stays a plain-BLOB table).
    ///
    /// **Validates: Requirements 3.3**
    #[test]
    fn fresh_and_no_symbol_vec_open_and_migrate_unchanged(
        n_symbols in 0usize..6usize,
        drop_vec in any::<bool>(),
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("uckg.db");

        // --- Fresh open (migration runs), optional symbol_vec removal. ---
        let (first_migrate, base_sym_count, base_has_vec, base_form) = {
            let mut db = Database::open(&db_path).expect("open fresh DB");
            let conn = db.connect().expect("connect fresh");
            let migrate = run_migrations(&conn).expect("run_migrations fresh");

            if n_symbols > 0 {
                let symbols: Vec<Symbol> =
                    (0..n_symbols).map(|i| make_symbol(&format!("s:{i}"))).collect();
                db.upsert_symbols(&symbols).expect("upsert symbols");
            }
            if drop_vec {
                db.connect()
                    .expect("connect")
                    .execute_batch("DROP TABLE IF EXISTS symbol_vec")
                    .expect("drop symbol_vec");
            }

            let sym_count = db.count("symbol").expect("count symbol");
            let form = symbol_vec_sql(&db);
            let has_vec = form.is_some();
            db.close_thread_connection();
            (migrate, sym_count, has_vec, form)
        };

        // A fresh DB migrates exactly once to the latest version.
        prop_assert_eq!(first_migrate, LATEST_SCHEMA_VERSION);
        prop_assert_eq!(base_has_vec, !drop_vec);
        if let Some(form) = &base_form {
            prop_assert!(
                form.to_ascii_uppercase().contains("BLOB")
                    && !form.to_ascii_uppercase().contains("USING VEC0"),
                "symbol_vec should stay the plain-BLOB fallback: {form}"
            );
        }

        // --- Re-open through a fresh handle: migrate no-ops, state preserved. ---
        let (re_migrate, re_version, re_sym_count, re_has_vec, re_form) = {
            let db2 = Database::open(&db_path).expect("re-open DB");
            let conn = db2.connect().expect("connect re-open");
            let migrate = run_migrations(&conn).expect("run_migrations re-open");
            let version = db2.schema_version().expect("schema_version");
            let sym_count = db2.count("symbol").expect("count symbol re-open");
            let form = symbol_vec_sql(&db2);
            let has_vec = form.is_some();
            db2.close_thread_connection();
            (migrate, version, sym_count, has_vec, form)
        };

        prop_assert_eq!(re_migrate, LATEST_SCHEMA_VERSION);
        prop_assert_eq!(re_version, LATEST_SCHEMA_VERSION);
        prop_assert_eq!(re_sym_count, base_sym_count);
        prop_assert_eq!(re_has_vec, base_has_vec);
        prop_assert_eq!(re_form, base_form);
    }
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.6 — vec_search BLOB top-k matches the checked-in golden
// ---------------------------------------------------------------------------

/// `vec_search` over the plain-BLOB oracle DB returns the same top-k
/// (ordering + score within tolerance) as the frozen `vec_parity` golden —
/// the BLOB linear-scan behaviour the fix must preserve.
///
/// **Validates: Requirements 3.6**
#[test]
fn vec_search_blob_topk_matches_vec_parity_golden() {
    const TOL: f64 = 1e-9;

    let golden_text = fs::read_to_string(vec_parity_golden()).expect("read vec_parity golden");
    let golden: Value = serde_json::from_str(&golden_text).expect("parse golden json");
    let k = golden["k"].as_u64().expect("golden.k") as usize;
    let cases = golden["cases"].as_array().expect("golden.cases");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("uckg.db");
    copy_oracle_to(&db_path);
    let db = Database::open(&db_path).expect("open oracle copy");

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let q: Vec<f32> = case["query"]
            .as_array()
            .expect("case.query")
            .iter()
            .map(|v| v.as_f64().expect("query component") as f32)
            .collect();
        let expected = case["expected"].as_array().expect("case.expected");

        let hits = db.vec_search(&q, k).expect("vec_search");
        assert_eq!(
            hits.len(),
            expected.len(),
            "hit count diverges from golden for case {name:?}"
        );
        for (i, (hit, exp)) in hits.iter().zip(expected.iter()).enumerate() {
            assert_eq!(
                hit.symbol_id,
                exp["symbol_id"].as_str().expect("expected.symbol_id"),
                "rank {i} symbol id diverges for case {name:?}"
            );
            let exp_score = exp["score"].as_f64().expect("expected.score");
            assert!(
                (hit.score - exp_score).abs() <= TOL,
                "rank {i} score {} vs golden {exp_score} (case {name:?})",
                hit.score
            );
        }
    }

    db.close_thread_connection();
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.4 — health JSON keys + status classes on a non-legacy DB
// ---------------------------------------------------------------------------

/// `build_health_report` over the plain-BLOB oracle DB reports the expected
/// `db`/`index`/`vector` status classes and serializes to the pinned JSON
/// object shape (a `checks` map keyed by check name) — unchanged, non-crashing.
///
/// **Validates: Requirements 3.4**
#[test]
fn health_report_shape_and_status_on_non_legacy_db() {
    // This surface resolves the DB via COGNIS_DB_PATH first; clear any inherited
    // override so we exercise the fixture at `<repo>/.cognis/uckg.db`.
    std::env::remove_var("COGNIS_DB_PATH");
    let fx = oracle_repo();

    let report = build_health_report(&fx.repo_root);
    let status_of = |name: &str| {
        report
            .checks
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c.status)
    };

    // All four subchecks are present (the pinned key set).
    for key in ["config", "db", "index", "vector"] {
        assert!(
            status_of(key).is_some(),
            "health report missing `{key}` check"
        );
    }
    // Non-legacy oracle DB: present+writable, 4 symbols, 4 BLOB vectors.
    assert_eq!(status_of("db"), Some(HealthStatus::Ok), "db should be ok");
    assert_eq!(
        status_of("index"),
        Some(HealthStatus::Ok),
        "index should be ok (4 symbols)"
    );
    assert_eq!(
        status_of("vector"),
        Some(HealthStatus::Ok),
        "vector should be ok (4 BLOB vectors present)"
    );

    // JSON shape: `checks` is an object keyed by name, each with status+message.
    let json = serde_json::to_value(&report).expect("serialize health report");
    assert!(json.get("runtime_version").is_some());
    assert!(json.get("overall").is_some());
    let checks = json.get("checks").expect("checks key");
    assert!(checks.is_object(), "checks must serialize as a JSON object");
    for key in ["config", "db", "index", "vector"] {
        let check = checks.get(key).expect("check present in JSON");
        assert!(check.get("status").is_some(), "{key} check has status");
        assert!(check.get("message").is_some(), "{key} check has message");
    }
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.5 — doctor JSON keys + status classes on a non-legacy DB
// ---------------------------------------------------------------------------

/// `cognis-cli doctor` over the plain-BLOB oracle DB emits the pinned
/// `PrerequisiteReport` JSON shape (`ready` / `items[]` / `combined_install_target`)
/// with the engine item `ok` and the semantic-index item `ok` (vectors present)
/// — unchanged and non-crashing.
///
/// **Validates: Requirements 3.5**
#[test]
fn doctor_report_shape_and_status_on_non_legacy_db() {
    let fx = oracle_repo();

    let exe = env!("CARGO_BIN_EXE_cognis-cli");
    let out = std::process::Command::new(exe)
        .arg("--repo-root")
        .arg(&fx.repo_root)
        .arg("doctor")
        // Ensure the child resolves the fixture DB under the repo, not an
        // inherited override.
        .env_remove("COGNIS_DB_PATH")
        .output()
        .expect("run cognis-cli doctor");

    assert!(
        out.status.success(),
        "doctor must exit 0 on a non-legacy DB; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json: Value = serde_json::from_slice(&out.stdout).expect("parse doctor JSON");
    assert_eq!(
        json["combined_install_target"],
        Value::String(String::new())
    );
    assert_eq!(json["ready"], Value::Bool(true));
    let items = json["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "doctor should report at least one item");

    // Each item carries the pinned contract fields.
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

    let status_of = |id: &str| {
        items
            .iter()
            .find(|it| it["id"] == Value::String(id.to_string()))
            .map(|it| it["status"].as_str().unwrap_or("").to_string())
    };
    assert_eq!(
        status_of("engine"),
        Some("ok".to_string()),
        "engine prerequisite should be ok (binary is running)"
    );
    assert_eq!(
        status_of("semantic_index"),
        Some("ok".to_string()),
        "semantic_index should be ok (oracle has 4 BLOB vectors)"
    );
}

// ---------------------------------------------------------------------------
// Property 2 / Req 3.2 — vec0-with-extension preservation (gated)
// ---------------------------------------------------------------------------

/// When the sqlite-vec extension IS loadable, a `vec0` `symbol_vec` DB keeps its
/// `vec0` form on open (it is NOT healed to BLOB). This case only compiles/runs
/// under the `sqlite-vec` feature — the default offline build has no `vec0`
/// module, so it is gated off and covered by the other preservation cases.
///
/// **Validates: Requirements 3.2**
#[cfg(feature = "sqlite-vec")]
#[test]
fn vec0_with_extension_keeps_vec0_form() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("uckg.db");

    // Reconcile to a vec0 table (requires the loadable extension). If the
    // environment has no extension available, there is nothing to preserve.
    let mut db = Database::open(&db_path).expect("open fresh DB");
    db.reconcile_embedding_dim(384).expect("reconcile");

    let form = symbol_vec_sql(&db);
    if let Some(sql) = form {
        if sql.to_ascii_uppercase().contains("USING VEC0") {
            db.close_thread_connection();
            let db2 = Database::open(&db_path).expect("re-open DB");
            let re_form = symbol_vec_sql(&db2).expect("symbol_vec present");
            assert!(
                re_form.to_ascii_uppercase().contains("USING VEC0"),
                "a vec0 DB on a build with the extension must keep the vec0 form: {re_form}"
            );
            db2.close_thread_connection();
        }
    }
}
