//! End-to-end test of the watcher → native pipeline wiring (Task 8.3).
//!
//! Drives the real `watch_loop_indexing` loop against a temp repo with a live
//! UCKG database, then asserts a created source file flows through
//! `notify` → debounce → [`IndexerPipeline::index_batch`] and its symbols land
//! in the DB, and that deleting the file removes them again (incremental
//! indexing). This complements `watch_loop.rs` (which proves events reach the
//! batch handler) by proving the batch handler actually indexes.
//!
//! The loop runs on the test's main thread because the pipeline's SQLite
//! connection is per-thread; a helper thread performs the filesystem edits and
//! observes the committed rows through its **own** connection to the same WAL
//! database (so it can stop the loop once indexing is confirmed).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-indexd-incr-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

#[test]
fn created_then_deleted_file_indexes_and_unindexes_through_watcher() {
    let repo = unique_dir("flow");
    let repo_canon = repo.canonicalize().unwrap_or_else(|_| repo.clone());
    std::env::remove_var("COGNIS_INDEXD_STATUS_PATH");
    let cognis_dir = repo_canon.join(".cognis");
    std::fs::create_dir_all(&cognis_dir).unwrap();
    let status_path = cognis_dir.join("indexd-status.json");
    let db_path = cognis_dir.join("uckg.db");

    // Pipeline opens (and connects) on this thread; the watch loop and its
    // per-batch handler also run on this thread, sharing that connection.
    let mut pipeline = IndexerPipeline::open(&db_path, Config::default()).expect("open pipeline");

    let running = Arc::new(AtomicBool::new(true));

    let helper = {
        let running = running.clone();
        let repo = repo_canon.clone();
        let status_path = status_path.clone();
        let db_path = db_path.clone();
        std::thread::spawn(move || {
            // Wait for the watcher to be live.
            wait_until(Duration::from_secs(10), || {
                std::fs::read_to_string(&status_path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .map(|v| v["phase"] == "watching")
                    .unwrap_or(false)
            });

            // The helper observes committed rows through its own connection.
            let probe = Database::open(&db_path).expect("probe db");

            // Create a source file — it must get indexed.
            std::fs::write(repo.join("alpha.py"), b"def alpha():\n    return 1\n").unwrap();
            let indexed = wait_until(Duration::from_secs(15), || {
                probe.count("symbol").unwrap_or(0) >= 1
            });
            assert!(indexed, "created file never produced a symbol in the DB");

            // Delete it — its symbols must be removed.
            std::fs::remove_file(repo.join("alpha.py")).unwrap();
            let unindexed = wait_until(Duration::from_secs(15), || {
                probe.count("symbol").unwrap_or(-1) == 0
            });
            assert!(unindexed, "deleted file's symbols were not removed");

            running.store(false, Ordering::SeqCst);
        })
    };

    cognis_indexd::watch_loop_indexing(&repo_canon, &Config::default(), running, &mut pipeline)
        .expect("watch loop ok");
    helper.join().expect("helper thread");

    // Final state on the loop's own connection: no symbols remain.
    assert_eq!(pipeline.database().count("symbol").unwrap(), 0);

    std::fs::remove_dir_all(&repo).ok();
}
