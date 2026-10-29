//! End-to-end test of the `notify` watch loop (Task 7.3).
//!
//! Drives the real daemon loop in a thread against a temp repo, writes a source
//! file, and asserts the change flows through `notify` → debounce → batch
//! handler, then that a clean stop publishes a `stopped` status snapshot. This
//! exercises the actual filesystem-watch integration, not just the pure
//! helpers covered by the crate's unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cognis_core::Config;

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-indexd-e2e-{tag}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn file_change_flows_through_notify_to_a_batch() {
    let repo = unique_dir("flow");
    let repo_canon = repo.canonicalize().unwrap_or_else(|_| repo.clone());
    let status_path = repo.join(".cognis").join("indexd-status.json");
    // Point the daemon's status file at this repo's .cognis (default), and make
    // sure no global override leaks in from the host env.
    std::env::remove_var("COGNIS_INDEXD_STATUS_PATH");

    let config = Config::default();
    let running = Arc::new(AtomicBool::new(true));
    let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));

    let handle = {
        let repo = repo_canon.clone();
        let running = running.clone();
        let batches = batches.clone();
        std::thread::spawn(move || {
            cognis_indexd::watch_loop_with(&repo, &config, running, |root, paths, status| {
                let rel: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        p.strip_prefix(root)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .replace('\\', "/")
                    })
                    .collect();
                batches.lock().unwrap().push(rel);
                // Still update status so the snapshot reflects the batch.
                status.pending_count = paths.len();
            })
        })
    };

    // Wait until the watcher is up (status flips to "watching").
    wait_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| v["phase"] == "watching")
            .unwrap_or(false)
    });

    // Create a source file under the repo; this must reach the batch handler.
    std::fs::write(repo_canon.join("alpha.rs"), b"fn main() {}\n").unwrap();

    let saw_change = wait_until(Duration::from_secs(15), || {
        batches
            .lock()
            .unwrap()
            .iter()
            .any(|b| b.iter().any(|p| p.ends_with("alpha.rs")))
    });
    assert!(
        saw_change,
        "watch loop never reported the created file; batches={:?}",
        batches.lock().unwrap()
    );

    // Clean shutdown publishes a final "stopped" snapshot.
    running.store(false, Ordering::SeqCst);
    handle.join().unwrap().expect("watch loop ok");

    let final_status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
    assert_eq!(final_status["phase"], "stopped");
    assert_eq!(final_status["active"], false);

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn ignored_paths_do_not_trigger_a_batch() {
    let repo = unique_dir("ignore");
    let repo_canon = repo.canonicalize().unwrap_or_else(|_| repo.clone());
    let status_path = repo.join(".cognis").join("indexd-status.json");
    std::env::remove_var("COGNIS_INDEXD_STATUS_PATH");
    std::fs::create_dir_all(repo_canon.join("target")).unwrap();

    let config = Config::default(); // repo.ignore includes "target", ".git", …
    let running = Arc::new(AtomicBool::new(true));
    let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));

    let handle = {
        let repo = repo_canon.clone();
        let running = running.clone();
        let batches = batches.clone();
        std::thread::spawn(move || {
            cognis_indexd::watch_loop_with(&repo, &config, running, |_root, paths, _status| {
                batches
                    .lock()
                    .unwrap()
                    .push(paths.iter().map(|p| p.display().to_string()).collect());
            })
        })
    };

    wait_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| v["phase"] == "watching")
            .unwrap_or(false)
    });

    // Write only into an ignored directory.
    std::fs::write(repo_canon.join("target").join("out.bin"), b"x").unwrap();
    // Give the loop a couple of batch windows to (not) react.
    std::thread::sleep(Duration::from_millis(1500));

    assert!(
        batches.lock().unwrap().is_empty(),
        "ignored-dir change should not produce a batch, got {:?}",
        batches.lock().unwrap()
    );

    running.store(false, Ordering::SeqCst);
    handle.join().unwrap().expect("watch loop ok");
    std::fs::remove_dir_all(&repo).ok();
}

/// Poll `cond` until it returns true or `timeout` elapses.
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
