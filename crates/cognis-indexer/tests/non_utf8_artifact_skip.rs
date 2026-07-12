//! Unit/integration test for Requirement 1.7 (Task 1.6): a non-UTF-8 admitted
//! artifact file emits no symbols, the walk does not abort, and the remaining
//! valid files in the batch are still indexed.
//!
//! Feature: non-code-artifact-coverage
//!
//! ## What this pins
//!
//! Requirement 1.7: "IF an admitted Artifact_File cannot be decoded as UTF-8,
//! THEN THE Artifact_Walker SHALL emit no symbols for that file, index the
//! remaining admitted files, and continue the walk without aborting the batch."
//!
//! The relevant seam is `IndexerPipeline::parse_and_enrich` in
//! `crates/cognis-indexer/src/pipeline.rs`, which reads the file bytes and does
//! `String::from_utf8(bytes)` → returns `Ok(None)` on failure (skip, no
//! symbols) so the batch continues. This test drives the public
//! `IndexerPipeline::index_repo` API against a temp repo containing a non-UTF-8
//! artifact (`.yaml`) alongside a valid code file (`.py`) and a valid artifact-
//! adjacent code file, and asserts the skip/continue contract holds.

use std::path::PathBuf;

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;

/// A fresh, process-and-time unique temp directory so concurrent test binaries
/// never collide on the same repo root.
fn unique_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-non-utf8-artifact-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn non_utf8_artifact_is_skipped_and_batch_continues() {
    let repo = unique_repo("skip");

    // (a) A non-UTF-8 admitted artifact file. `.yaml` is a recognized artifact
    // extension (admitted by the second admission path, artifacts enabled by
    // default). The leading 0xFF/0xFE bytes are never valid UTF-8, so
    // `String::from_utf8` must fail for this file.
    let bad_yaml = repo.join("config.yaml");
    let invalid_utf8: Vec<u8> = vec![0xFF, 0xFE, 0x9D, 0xC3, 0x28, 0x80, 0x00, 0xA0];
    std::fs::write(&bad_yaml, &invalid_utf8).unwrap();
    // Sanity: the bytes we wrote really are undecodable as UTF-8.
    assert!(
        String::from_utf8(invalid_utf8.clone()).is_err(),
        "test fixture must be invalid UTF-8 for the skip path to be exercised"
    );

    // (b) A valid code file that must still be indexed after the bad file.
    std::fs::write(
        repo.join("app.py"),
        "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
    )
    .unwrap();

    // (c) A second valid code file, to prove the walk keeps going and indexes
    // *every* remaining admitted file, not just one.
    std::fs::write(repo.join("other.py"), "def standalone():\n    return 2\n").unwrap();

    // Index the whole repo. `index_repo` returning `Ok` proves the batch did
    // not abort on the undecodable artifact.
    let db = Database::open(":memory:").expect("open in-memory uckg");
    let mut pipeline = IndexerPipeline::new(db.clone(), Config::default());
    let stats = pipeline
        .index_repo(&repo, true)
        .expect("index_repo must not abort on a non-UTF-8 artifact");

    let symbols = db.list_symbols().expect("read symbols back");

    // 1. The non-UTF-8 artifact produced zero symbols.
    let bad_symbols = symbols
        .iter()
        .filter(|s| s.file_path == "config.yaml")
        .count();
    assert_eq!(
        bad_symbols, 0,
        "non-UTF-8 artifact must emit no symbols, got {bad_symbols}"
    );

    // 2. The batch continued and the remaining valid files were indexed.
    let app_symbols = symbols.iter().filter(|s| s.file_path == "app.py").count();
    let other_symbols = symbols.iter().filter(|s| s.file_path == "other.py").count();
    assert!(
        app_symbols > 0,
        "valid code file app.py must be indexed after the skipped artifact"
    );
    assert!(
        other_symbols > 0,
        "valid code file other.py must be indexed after the skipped artifact"
    );

    // 3. Stats corroborate: the two valid files were processed, and the skip was
    //    silent (no fatal per-file error recorded for the undecodable artifact).
    assert_eq!(
        stats.files_processed, 2,
        "exactly the two valid files should be processed (the artifact is skipped)"
    );
    assert!(
        !stats.errors.iter().any(|e| e.contains("config.yaml")),
        "the non-UTF-8 artifact is skipped silently, not recorded as an error: {:?}",
        stats.errors
    );

    let _ = std::fs::remove_dir_all(&repo);
}
