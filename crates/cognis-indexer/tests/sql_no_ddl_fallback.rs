//! Unit/integration test for Requirement 3.6 (Task 4.3): a SQL artifact file
//! that yields no parseable `CREATE TABLE (...)` DDL falls back to **exactly
//! one** whole-file textual `Module` symbol, and the batch continues.
//!
//! Feature: non-code-artifact-coverage
//!
//! ## What this pins
//!
//! Requirement 3.6: "IF a SQL Artifact_File yields no parseable DDL, THEN THE
//! SQL_Extractor SHALL fall back to exactly one textual symbol spanning the
//! file so the file remains searchable and the batch continues."
//!
//! The relevant seam is `cognis_indexer::parser::artifact::extract_artifact`
//! (dispatching `ArtifactKind::Sql` to `sql::extract`), which routes non-DDL SQL
//! text to the shared whole-file `textual_fallback`. The fallback produces one
//! `SymbolKind::Module` symbol spanning line 1..last, with `fell_back == true`,
//! that passes `Symbol::validate`.
//!
//! This test drives the public extractor API directly for the core Req-3.6
//! assertion, and additionally drives the public `IndexerPipeline::index_repo`
//! API against a temp repo containing a non-DDL `.sql` file alongside a valid
//! code file to prove the batch continues (both files indexed).

use std::path::PathBuf;

use cognis_core::{Config, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::{ArtifactKind, IndexerPipeline};
use cognis_store::Database;

/// Assert that `source` (non-DDL SQL) produces exactly one whole-file textual
/// `Module` symbol spanning line 1..last, with `fell_back == true`, and that the
/// symbol passes `Symbol::validate`.
fn assert_single_whole_file_fallback(source: &str) {
    let out = extract_artifact(ArtifactKind::Sql, "db/query.sql", source);

    // Exactly one symbol …
    assert_eq!(
        out.symbols.len(),
        1,
        "non-DDL SQL must yield exactly one fallback symbol, got {}: {:?}",
        out.symbols.len(),
        out.symbols
    );
    // … and it is the textual fallback.
    assert!(
        out.fell_back,
        "non-DDL SQL must be reported as a textual fallback (fell_back == true)"
    );

    let sym = &out.symbols[0];

    // Whole-file textual symbol is a `Module` (Req 3.6 fallback shape).
    assert_eq!(
        sym.kind,
        SymbolKind::Module,
        "the fallback symbol must be a Module symbol"
    );

    // Spans the whole file: line 1 to the last line.
    let last_line = source.lines().count().max(1) as u32;
    assert_eq!(sym.line_start, 1, "fallback must start at line 1");
    assert_eq!(
        sym.line_end, last_line,
        "fallback must span to the last line ({last_line})"
    );

    // Language label is the SQL artifact tag, and the source text remains
    // searchable through the fallback's body excerpt.
    assert_eq!(sym.language, "sql");

    // The emitted symbol honors every `Symbol::validate` invariant, in
    // particular `line_end >= line_start >= 1`.
    sym.validate().expect("fallback symbol must be valid");
    assert!(sym.line_end >= sym.line_start && sym.line_start >= 1);
}

/// A `SELECT` statement is not DDL → single whole-file textual fallback.
#[test]
fn select_only_falls_back_to_single_whole_file_symbol() {
    assert_single_whole_file_fallback("SELECT * FROM users;\n");
}

/// An `UPDATE` statement is not DDL → single whole-file textual fallback.
#[test]
fn update_only_falls_back_to_single_whole_file_symbol() {
    assert_single_whole_file_fallback("UPDATE users SET active = 1 WHERE id = 42;\n");
}

/// `CREATE INDEX` is a DDL statement but declares no table with columns, so it
/// must not be mistaken for a `CREATE TABLE` → single whole-file fallback.
#[test]
fn create_index_falls_back_to_single_whole_file_symbol() {
    assert_single_whole_file_fallback("CREATE INDEX idx_users_email ON users (email);\n");
}

/// A mixed non-DDL script (several statements, none a `CREATE TABLE (...)`)
/// still yields exactly one whole-file fallback spanning every line.
#[test]
fn mixed_non_ddl_script_yields_one_whole_file_symbol() {
    let src = "-- migration rollback\n\
               DELETE FROM sessions WHERE expired = 1;\n\
               INSERT INTO audit (msg) VALUES ('cleanup');\n\
               SELECT count(*) FROM audit;\n";
    assert_single_whole_file_fallback(src);
}

/// A fresh, process-and-time unique temp directory so concurrent test binaries
/// never collide on the same repo root.
fn unique_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-sql-no-ddl-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// End-to-end: a non-DDL `.sql` artifact alongside a valid code file. The batch
/// must continue — both files are indexed, and the non-DDL SQL contributes
/// exactly one whole-file `Module` fallback symbol (Req 3.6).
#[test]
fn non_ddl_sql_indexes_and_batch_continues() {
    let repo = unique_repo("e2e");

    // (a) A non-DDL SQL file (no CREATE TABLE) — must fall back, not abort.
    std::fs::write(
        repo.join("query.sql"),
        "SELECT id, email FROM users WHERE active = 1;\n",
    )
    .unwrap();

    // (b) A valid code file that must still be indexed after the SQL file.
    std::fs::write(
        repo.join("app.py"),
        "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
    )
    .unwrap();

    let db = Database::open(":memory:").expect("open in-memory uckg");
    let mut pipeline = IndexerPipeline::new(db.clone(), Config::default());
    pipeline
        .index_repo(&repo, true)
        .expect("index_repo must not abort on a non-DDL SQL artifact");

    let symbols = db.list_symbols().expect("read symbols back");

    // The non-DDL SQL file contributes exactly one whole-file Module fallback.
    let sql_symbols: Vec<_> = symbols
        .iter()
        .filter(|s| s.file_path == "query.sql")
        .collect();
    assert_eq!(
        sql_symbols.len(),
        1,
        "non-DDL SQL must contribute exactly one fallback symbol, got {:?}",
        sql_symbols
    );
    assert_eq!(sql_symbols[0].kind, SymbolKind::Module);

    // The batch continued: the valid code file was indexed too.
    let app_symbols = symbols.iter().filter(|s| s.file_path == "app.py").count();
    assert!(
        app_symbols > 0,
        "valid code file app.py must be indexed alongside the non-DDL SQL artifact"
    );

    let _ = std::fs::remove_dir_all(&repo);
}
