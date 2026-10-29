//! Smoke tests for the frozen `001_initial.sql` and the required index set
//! (cognis-one-toggle-engine task 1.5).
//!
//! Three things are pinned here, each of which is a *silent* failure everywhere
//! else in the system:
//!
//! 1. **`001_initial.sql` is frozen** (Requirement 7.8, 22.5). Every schema
//!    change this feature needs ships as `002_generation_and_cache.sql`. An edit
//!    to 001 would not fail any other test — `CREATE ... IF NOT EXISTS` DDL
//!    quietly diverges old and new databases instead of erroring — so the only
//!    way to catch it is to hash the file against a committed baseline.
//! 2. **`002_generation_and_cache.sql` is actually registered** in the private
//!    `MIGRATIONS` table and `LATEST_SCHEMA_VERSION` was bumped to 2. `MIGRATIONS`
//!    is crate-private, so this is asserted the way it is actually observable:
//!    every object 002's DDL declares exists in a freshly-created database, which
//!    is true if and only if the file is registered and applied.
//! 3. **`missing_required_indexes()` is empty** (Requirement 9.6) for both a
//!    fresh database and a *migrated pre-feature* one — the case that matters,
//!    because three of the seven required indexes only exist after 002 runs.
//!
//! ## Why the hash is over LF-normalised bytes
//!
//! `.gitattributes` does not pin `*.sql`, and this repo is developed on Windows
//! with `core.autocrlf=true`. So the working-tree form of `001_initial.sql` is
//! CRLF on a Windows checkout and LF on a Linux/CI checkout, while the committed
//! blob is LF on both. A raw hash of the bytes `include_bytes!` sees would
//! therefore be a *platform* assertion, not a content assertion, and would fail
//! on CI or on a colleague's machine for reasons that have nothing to do with the
//! freeze.
//!
//! Normalising CRLF → LF before hashing makes the baseline equal the committed
//! git blob hash on every checkout, which is exactly the property the freeze is
//! about: *the content is unchanged*. The bytes still come from `include_bytes!`
//! — compile-time, no runtime file IO, no dependence on the working directory —
//! so the test cannot pass by reading some other file.

use std::fmt::Write as _;

use cognis_store::{Database, LATEST_SCHEMA_VERSION, REQUIRED_INDEXES};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// The frozen migration, exactly as the crate embeds it (`include_str!` at
/// `src/lib.rs`). Bytes, not text, so the hash is over what is really there.
const MIGRATION_001: &[u8] = include_bytes!("../migrations/001_initial.sql");

/// The migration this feature adds. Read here so the test asserts against the
/// real DDL rather than a hand-copied list of object names that could drift.
const MIGRATION_002: &str = include_str!("../migrations/002_generation_and_cache.sql");

/// SHA-256 of `001_initial.sql` with CRLF normalised to LF — i.e. the hash of
/// the committed git blob, stable on every checkout and every platform.
///
/// **Do not "fix" this constant when it fails.** A mismatch means someone edited
/// the frozen migration; the correct response is to move the change into a new
/// numbered migration and restore 001 (Requirement 7.8, 22.5).
const MIGRATION_001_SHA256: &str =
    "890f6578219a9e25067fb73f5269e018d654a6a32a6abfb3fdb42aba5804ea81";

/// Length of the LF-normalised `001_initial.sql`, asserted alongside the hash so
/// a failure says *how much* changed, not just that something did.
const MIGRATION_001_LF_LEN: usize = 7847;

/// Drop every `\r` that is immediately followed by `\n`, leaving lone `\r`
/// bytes (which are content, not line endings) untouched.
fn normalize_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Extract the `(kind, name)` of every object a migration's DDL declares.
///
/// Deliberately structural rather than a hard-coded list: if 002 grows a table
/// or an index, the "is it registered and applied?" assertion covers the new
/// object automatically instead of silently checking a stale set.
fn declared_objects(sql: &str) -> Vec<(&'static str, String)> {
    const PREFIXES: &[(&str, &str)] = &[
        ("CREATE VIRTUAL TABLE IF NOT EXISTS ", "table"),
        ("CREATE TABLE IF NOT EXISTS ", "table"),
        ("CREATE INDEX IF NOT EXISTS ", "index"),
    ];
    let mut objects = Vec::new();
    for line in sql.lines() {
        let line = line.trim_start();
        if line.starts_with("--") {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        for (prefix, kind) in PREFIXES {
            if let Some(rest) = upper.strip_prefix(prefix) {
                // Take the name from the original line so its case survives.
                let name: String = line[line.len() - rest.len()..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    objects.push((if *kind == "table" { "table" } else { "index" }, name));
                }
                break;
            }
        }
    }
    objects
}

fn object_exists(conn: &Connection, kind: &str, name: &str) -> bool {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2")
        .expect("prepare sqlite_master probe")
        .exists(rusqlite::params![kind, name])
        .expect("query sqlite_master")
}

/// Build an authentic **pre-feature** database: apply only `001_initial.sql` and
/// stamp `schema_version = 1`, exactly the state a workspace indexed before this
/// feature is in. Done over a raw connection because `Database::connect` always
/// runs migrations, so the un-migrated state is unreachable through the handle.
///
/// The connection is dropped before returning so Windows releases the file
/// handle and `Database::open` can take it.
fn write_pre_feature_db(path: &std::path::Path) {
    let conn = Connection::open(path).expect("open raw pre-feature DB");
    let sql = String::from_utf8(MIGRATION_001.to_vec()).expect("001 is UTF-8");
    conn.execute_batch(&sql).expect("apply 001_initial.sql");
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('schema_version', '1')",
        [],
    )
    .expect("stamp schema_version = 1");
    drop(conn);
}

// ---------------------------------------------------------------------------
// 1. The frozen migration
// ---------------------------------------------------------------------------

/// **Validates: Requirements 7.8, 22.5**
///
/// `001_initial.sql` is byte-identical to its committed baseline.
#[test]
fn migration_001_is_frozen_at_its_baseline_hash() {
    let normalized = normalize_crlf(MIGRATION_001);

    assert_eq!(
        normalized.len(),
        MIGRATION_001_LF_LEN,
        "001_initial.sql changed length ({} -> {}). It is FROZEN (Requirement 7.8, 22.5): \
         ship the change as a new numbered migration and restore 001.",
        MIGRATION_001_LF_LEN,
        normalized.len()
    );

    assert_eq!(
        sha256_hex(&normalized),
        MIGRATION_001_SHA256,
        "001_initial.sql content changed. It is FROZEN (Requirement 7.8, 22.5): \
         ship the change as a new numbered migration and restore 001. \
         Do not update this baseline constant."
    );
}

// ---------------------------------------------------------------------------
// 2. 002 is registered and the schema version was bumped
// ---------------------------------------------------------------------------

/// **Validates: Requirement 22.5**
///
/// The new migration is registered in `MIGRATIONS` and the schema version was
/// bumped to match. `MIGRATIONS` is crate-private, so registration is asserted
/// through its only observable consequence: a freshly created database is
/// stamped at version 2 and carries every object 002's DDL declares.
#[test]
fn migration_002_is_registered_and_applied_on_a_fresh_database() {
    assert_eq!(
        LATEST_SCHEMA_VERSION, 2,
        "LATEST_SCHEMA_VERSION must be 2 now that 002_generation_and_cache.sql ships"
    );

    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("fresh.db")).expect("open fresh DB");

    assert_eq!(
        db.schema_version().unwrap(),
        LATEST_SCHEMA_VERSION,
        "a fresh database is stamped at the latest schema version"
    );

    let objects = declared_objects(MIGRATION_002);
    assert!(
        objects.iter().any(|(_, n)| n == "generation")
            && objects.iter().any(|(_, n)| n == "embedding_queue")
            && objects.iter().any(|(_, n)| n == "idx_embedding_queue_file"),
        "sanity: the DDL scanner found 002's objects, got {objects:?}"
    );

    let conn = db.connect().unwrap();
    for (kind, name) in &objects {
        assert!(
            object_exists(&conn, kind, name),
            "{kind} `{name}` is declared by 002_generation_and_cache.sql but absent from a \
             freshly created database — is the file registered in MIGRATIONS?"
        );
    }

    db.close_thread_connection();
}

// ---------------------------------------------------------------------------
// 3. The required index set
// ---------------------------------------------------------------------------

/// **Validates: Requirement 9.6**
///
/// A freshly created database has every required index.
#[test]
fn fresh_database_has_every_required_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("fresh_indexes.db")).expect("open fresh DB");

    assert_eq!(
        db.missing_required_indexes().unwrap(),
        Vec::<String>::new(),
        "a freshly created database must satisfy the whole required index set {REQUIRED_INDEXES:?}"
    );

    db.close_thread_connection();
}

/// **Validates: Requirements 9.6, 22.5**
///
/// A database created *before* this feature (schema version 1, only 001 applied)
/// gains the rest of the required index set when it is migrated on open.
///
/// The pre-check matters as much as the post-check: it proves the fixture is
/// genuinely pre-feature, so the passing assertion below is really testing the
/// migration rather than a database that already had the indexes.
#[test]
fn migrated_pre_feature_database_has_every_required_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pre_feature.db");
    write_pre_feature_db(&path);

    // Pre-condition: a schema-1 database is genuinely missing 002's indexes.
    {
        let conn = Connection::open(&path).expect("reopen pre-feature DB");
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .expect("read stamped schema_version");
        assert_eq!(version, "1", "fixture is stamped as a pre-feature database");

        for name in [
            "idx_symbol_name",
            "idx_symbol_name_lower",
            "idx_embedding_queue_file",
        ] {
            assert!(
                !object_exists(&conn, "index", name),
                "fixture is not genuinely pre-feature: `{name}` already exists before 002 runs"
            );
        }
        drop(conn);
    }

    // Opening applies 002 in place.
    let db = Database::open(&path).expect("open (and migrate) the pre-feature DB");
    assert_eq!(
        db.schema_version().unwrap(),
        LATEST_SCHEMA_VERSION,
        "opening a pre-feature database migrates it to the latest schema version"
    );
    assert_eq!(
        db.missing_required_indexes().unwrap(),
        Vec::<String>::new(),
        "a migrated pre-feature database must satisfy the whole required index set"
    );

    db.close_thread_connection();
}

/// **Validates: Requirements 9.6, 22.5**
///
/// The same guarantee against the checked-in frozen compatibility fixture — a
/// database written by the legacy engine at schema version 1, i.e. a real
/// pre-feature database rather than one this test constructed.
#[test]
fn migrated_legacy_oracle_fixture_has_every_required_index() {
    let src =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/uckg_oracle.db");
    assert!(
        src.exists(),
        "missing frozen compatibility fixture {src:?} (see tests/legacy_db_compat.rs)"
    );

    // Copy so the checked-in fixture is never mutated by WAL sidecar files.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uckg_oracle.db");
    std::fs::copy(&src, &path).expect("copy fixture");

    let db = Database::open(&path).expect("open (and migrate) the legacy fixture");
    assert_eq!(db.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
    assert_eq!(
        db.missing_required_indexes().unwrap(),
        Vec::<String>::new(),
        "a migrated legacy database must satisfy the whole required index set"
    );

    // The migration is additive: the fixture's pre-feature content survives
    // (the full data-preservation property is task 1.4's).
    assert_eq!(
        db.count("symbol").unwrap(),
        4,
        "fixture symbols survive 002"
    );
    assert_eq!(db.count("edge").unwrap(), 3, "fixture edges survive 002");

    db.close_thread_connection();
}
