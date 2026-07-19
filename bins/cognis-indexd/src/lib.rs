//! cognis-indexd — incremental indexer daemon (Task 7.3 loop, Task 8.3 wiring).
//!
//! Lands the `notify`-based watch loop the live daemon runs: it watches the
//! repo root, coalesces filesystem events into debounced batches, publishes a
//! status snapshot IDE integrations poll (`.cognis/indexd-status.json`, same
//! shape as the Python daemon), and shuts down cleanly on Ctrl-C / SIGTERM.
//!
//! Task 8.3 connects the native `cognis-indexer` pipeline to the watcher: the
//! production entry point ([`run`] / [`run_from`]) opens the UCKG database,
//! optionally does a full cold rebuild, then drives the loop with
//! [`watch_loop_indexing`], whose per-batch handler calls
//! [`IndexerPipeline::index_batch`] — changed source files are re-parsed /
//! re-resolved / re-written and deleted files have their symbols removed
//! (incremental indexing). The status-only [`process_batch`] /
//! [`watch_loop`] remain as the lib's pipeline-free fallback (used by the
//! filesystem-flow tests and any caller that just wants the watch plumbing).
//!
//! [`run`] / [`run_from`] are the entry points the standalone `cognis-indexd`
//! binary and the single multi-call `cognis` binary dispatch into.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;

use cognis_core::lease::{acquire_or_attach, AcquireOutcome, LeaseGuard, LeaseRole};
use cognis_core::{Config, SemanticWarmPolicy};
use cognis_indexer::IndexerPipeline;

/// How long to collect more events before dispatching a batch (matches the
/// Python daemon's 500 ms batch window — keeps writer transactions low under
/// burst-edit "save all" workloads).
const BATCH_WINDOW: Duration = Duration::from_millis(500);
const STATUS_FILE_NAME: &str = "indexd-status.json";

/// Long-running incremental indexer daemon for cognis.
#[derive(Debug, Parser)]
#[command(name = "cognis-indexd", version, about = "cognis live indexing daemon")]
pub struct DaemonArgs {
    /// Repository root to watch (default: current working directory).
    #[arg(long, value_name = "DIR")]
    pub repo_root: Option<PathBuf>,
    /// UCKG database path (default: COGNIS_DB_PATH or <repo>/.cognis/uckg.db).
    #[arg(long, value_name = "FILE")]
    pub db_path: Option<PathBuf>,
    /// Force a full index rebuild before starting the watcher.
    #[arg(long)]
    pub full_rebuild: bool,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Parse `std::env::args_os()` and run the daemon. Used by the standalone bin.
pub fn run() -> ExitCode {
    match DaemonArgs::try_parse() {
        Ok(args) => run_args(args),
        Err(err) => clap_exit(err),
    }
}

/// Parse an explicit argv (`args[0]` is the program name) and run the daemon.
/// Used by the multi-call `cognis` dispatcher.
pub fn run_from<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match DaemonArgs::try_parse_from(args) {
        Ok(args) => run_args(args),
        Err(err) => clap_exit(err),
    }
}

fn clap_exit(err: clap::Error) -> ExitCode {
    let _ = err.print();
    if err.use_stderr() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_args(args: DaemonArgs) -> ExitCode {
    let repo_root = args
        .repo_root
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    if !repo_root.is_dir() {
        eprintln!(
            "cognis-indexd: repo root does not exist or is not a directory: {}",
            repo_root.display()
        );
        return ExitCode::FAILURE;
    }

    let config = Config::load(&repo_root).unwrap_or_default();
    let db_path = resolve_db_path(args.db_path.as_deref(), &repo_root);

    // Acquire-or-attach the repository-scoped lease BEFORE opening the DB /
    // spawning any watch work (Task 6.1; Requirements 2.7, 2.13). If a live,
    // non-expired `indexd.lease` already records another owner, this repository
    // already has a live indexing daemon — attach/reuse instead of starting a
    // duplicate heavy process (bug facet `repoHasDuplicateHeavyDaemonOrOrphan`).
    // The guard heartbeats on a background thread and releases on drop; hold it
    // for the whole daemon lifetime by binding it to `_lease_guard`, which lives
    // to the end of `run_args`. A lease I/O failure must not take down indexing,
    // so we log and continue lease-free (degrades to the prior behavior).
    let _lease_guard: Option<LeaseGuard> =
        match acquire_or_attach(&repo_root, LeaseRole::Indexd, None) {
            Ok(AcquireOutcome::Acquired(guard)) => Some(guard),
            Ok(AcquireOutcome::Attached { lease, path }) => {
                eprintln!(
                    "cognis-indexd: a live indexing daemon already owns this \
                     repository (pid {}, lease {}); attaching instead of starting \
                     a duplicate",
                    lease.pid,
                    path.display()
                );
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                eprintln!(
                    "cognis-indexd: warning: could not acquire repository lease: {err}; \
                     continuing without cross-process ownership"
                );
                None
            }
        };

    // Ctrl-C / SIGTERM → flip the run flag so the loop exits and publishes a
    // final "stopped" snapshot. `set_handler` may already be installed (e.g.
    // when embedded); treat that as non-fatal.
    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));
    }

    // Open the native indexer pipeline (Task 8.3). Opening the DB eagerly here
    // surfaces a bad path / unreadable DB immediately rather than on first edit.
    //
    // Resolve the semantic warm policy at the daemon entry point so the
    // extension's `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP` signal is actually
    // consumed (Requirement 2.4; bug facet
    // `semanticWarmPolicyIsIgnoredOrInconsistent`). Eager builds the embedder
    // up front (legacy behavior / direct launch); Lazy defers it to first
    // demand so zero ONNX is resident at startup — non-semantic indexing does
    // not wait on the model (preservation 3.3/3.4).
    let warm_policy = SemanticWarmPolicy::from_env();
    let mut pipeline =
        match IndexerPipeline::open_with_policy(&db_path, config.clone(), warm_policy) {
            Ok(p) => p,
            Err(err) => {
                eprintln!(
                    "cognis-indexd: cannot open index database {}: {err}",
                    db_path.display()
                );
                return ExitCode::FAILURE;
            }
        };

    // Optional cold/full rebuild before watching (mirrors the Python daemon's
    // `--full-rebuild`): index the whole repo once so the watcher only has to
    // keep an already-warm index fresh.
    if args.full_rebuild {
        match pipeline.index_repo(&repo_root, true) {
            Ok(stats) => eprintln!(
                "cognis-indexd: full rebuild indexed {} file(s) / {} symbols / {} edges",
                stats.files_processed, stats.symbols_indexed, stats.edges_resolved
            ),
            Err(err) => eprintln!("cognis-indexd: full rebuild failed: {err}"),
        }
    }

    match watch_loop_indexing(&repo_root, &config, running, &mut pipeline) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cognis-indexd: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the UCKG database path: explicit `--db-path`, else `COGNIS_DB_PATH`,
/// else `<repo>/.cognis/uckg.db`. Ensures the parent directory exists so a
/// fresh repo (no prior index) can be opened.
fn resolve_db_path(explicit: Option<&Path>, repo_root: &Path) -> PathBuf {
    let path = if let Some(p) = explicit {
        p.to_path_buf()
    } else if let Ok(env) = std::env::var("COGNIS_DB_PATH") {
        if env.trim().is_empty() {
            default_db_path(repo_root)
        } else {
            PathBuf::from(env)
        }
    } else {
        default_db_path(repo_root)
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

fn default_db_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(cognis_core::config::CONFIG_DIR_NAME)
        .join("uckg.db")
}

// ---------------------------------------------------------------------------
// Watch loop
// ---------------------------------------------------------------------------

/// Run the watch loop until `running` is cleared. Pure of process-global state
/// (signal handling lives in [`run_args`]) so it is exercisable from tests with
/// a controllable stop flag.
pub fn watch_loop(
    repo_root: &Path,
    config: &Config,
    running: Arc<AtomicBool>,
) -> std::io::Result<()> {
    watch_loop_with(repo_root, config, running, |root, paths, status| {
        process_batch(root, paths, status)
    })
}

/// [`watch_loop`] with an injectable per-batch handler — the seam tests drive
/// to observe that real filesystem events flow through `notify` into a batch.
/// `on_batch` runs for every debounced, ignore-filtered, non-empty batch.
pub fn watch_loop_with<F>(
    repo_root: &Path,
    config: &Config,
    running: Arc<AtomicBool>,
    mut on_batch: F,
) -> std::io::Result<()>
where
    F: FnMut(&Path, &[PathBuf], &mut DaemonStatus),
{
    let status_path = resolve_status_path(repo_root);

    let mut status = DaemonStatus::starting();
    write_status_file(&status_path, &status)?;

    // notify watcher → mpsc channel of raw events.
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .map_err(to_io)?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .map_err(to_io)?;

    status = DaemonStatus::watching();
    write_status_file(&status_path, &status)?;
    eprintln!(
        "cognis-indexd (Rust) watching {} — Ctrl-C to stop",
        repo_root.display()
    );

    let ignore = &config.repo.ignore;
    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(first) => {
                // Drain the batch window, collecting repo-relevant changes.
                let mut changes: BTreeSet<PathBuf> = BTreeSet::new();
                collect_event(first, repo_root, ignore, &mut changes);
                let deadline = std::time::Instant::now() + BATCH_WINDOW;
                loop {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(ev) => collect_event(ev, repo_root, ignore, &mut changes),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                if !changes.is_empty() {
                    let paths: Vec<PathBuf> = changes.into_iter().collect();
                    on_batch(repo_root, &paths, &mut status);
                    write_status_file(&status_path, &status)?;
                    // Return to the idle "watching" snapshot.
                    status = DaemonStatus::watching();
                    write_status_file(&status_path, &status)?;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    status = DaemonStatus::stopped();
    write_status_file(&status_path, &status)?;
    eprintln!("cognis-indexd: stopped");
    Ok(())
}

/// Drive the watch loop with the native `cognis-indexer` pipeline (Task 8.3).
///
/// Each debounced, ignore-filtered batch is handed to
/// [`IndexerPipeline::index_batch`], which re-indexes the changed source files
/// (one cross-file edge-resolution pass) and removes the deleted ones; the
/// status snapshot then reflects the run's counters. This is the production
/// entry the daemon binary uses; the pipeline-free [`watch_loop`] remains for
/// callers that only want the watch plumbing.
pub fn watch_loop_indexing(
    repo_root: &Path,
    config: &Config,
    running: Arc<AtomicBool>,
    pipeline: &mut IndexerPipeline,
) -> std::io::Result<()> {
    watch_loop_with(repo_root, config, running, |root, paths, status| {
        index_batch_into_status(pipeline, root, paths, status);
    })
}

/// Run one batch through the pipeline and record the outcome in `status`.
fn index_batch_into_status(
    pipeline: &mut IndexerPipeline,
    repo_root: &Path,
    paths: &[PathBuf],
    status: &mut DaemonStatus,
) {
    let rel: Vec<String> = paths
        .iter()
        .take(8)
        .map(|p| relative_to(p, repo_root))
        .collect();
    status.phase = "incremental".to_string();
    status.pending_count = paths.len();
    status.pending_files = rel.clone();
    status.inflight_count = paths.len();
    status.inflight_files = rel.clone();
    status.recent_files = rel;
    status.progress_percent = Some(65.0);
    status.touch();

    match pipeline.index_batch(repo_root, paths) {
        Ok(stats) => {
            let pending_note = if stats.vectors_pending > 0 {
                format!(
                    "; {} vector group{} pending retry",
                    stats.vectors_pending,
                    if stats.vectors_pending == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            status.message = format!(
                "Indexed {} file{} ({} symbols, {} edges); removed {} file{}{}",
                stats.files_processed,
                if stats.files_processed == 1 { "" } else { "s" },
                stats.symbols_indexed,
                stats.edges_resolved,
                stats.files_removed,
                if stats.files_removed == 1 { "" } else { "s" },
                pending_note,
            );
            status.last_error = if stats.errors.is_empty() {
                None
            } else {
                Some(stats.errors.join("; "))
            };
            // Never claim semantic completion while vectors are still pending
            // (Requirement 2.6; preservation 3.5).
            if stats.vectors_pending > 0 {
                status.progress_percent = Some(90.0);
            } else {
                status.progress_percent = Some(100.0);
            }
        }
        Err(err) => {
            status.message = format!("Indexing failed: {err}");
            status.last_error = Some(err.to_string());
            status.progress_percent = Some(100.0);
        }
    }
    // Overlay retained / in-flight work so completeness is observable.
    status.apply_pipeline_work(&pipeline.work_snapshot());
    // After a successful batch with no pending work, clear inflight.
    if status.inflight_count == 0 && status.pending_count == 0 {
        status.inflight_files.clear();
        status.pending_files.clear();
    }
    status.touch();

    // Idle eviction: only after a measured idle interval with no pending /
    // in-flight work. Best-effort; reload re-initializes via single-flight.
    let _ = pipeline.try_idle_evict_model();
}

/// Fold one watcher event into the pending-change set, filtering ignored paths.
fn collect_event(
    event: notify::Result<notify::Event>,
    repo_root: &Path,
    ignore: &[String],
    changes: &mut BTreeSet<PathBuf>,
) {
    let Ok(event) = event else { return };
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return;
    }
    for path in event.paths {
        if !should_ignore(&path, repo_root, ignore) {
            changes.insert(path);
        }
    }
}

/// Process one debounced batch of changed paths — **status-only fallback**.
///
/// The production daemon path drives indexing through
/// [`watch_loop_indexing`] / [`index_batch_into_status`], which call the native
/// `cognis-indexer` pipeline. This pipeline-free handler (reachable via
/// [`watch_loop`]) just records the batch in the status snapshot and logs the
/// paths, so the watch plumbing stays exercisable without a database.
pub fn process_batch(repo_root: &Path, paths: &[PathBuf], status: &mut DaemonStatus) {
    let rel: Vec<String> = paths
        .iter()
        .take(8)
        .map(|p| relative_to(p, repo_root))
        .collect();
    eprintln!("cognis-indexd: {} changed file(s): {:?}", paths.len(), rel);
    status.phase = "incremental".to_string();
    status.message = format!(
        "Indexing {} changed file{}…",
        paths.len(),
        if paths.len() == 1 { "" } else { "s" }
    );
    status.progress_percent = Some(65.0);
    status.pending_count = paths.len();
    status.recent_files = rel;
    status.touch();
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// True when `path` is under an ignored directory (config `repo.ignore`) or the
/// `.cognis/` runtime dir itself (never re-index our own DB / status writes).
pub fn should_ignore(path: &Path, repo_root: &Path, ignore: &[String]) -> bool {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    let mut ignored: Vec<&str> = ignore.iter().map(String::as_str).collect();
    ignored.push(cognis_core::config::CONFIG_DIR_NAME);
    rel.components().any(|c| {
        let seg = c.as_os_str().to_string_lossy();
        ignored.iter().any(|ig| seg == *ig)
    })
}

fn relative_to(path: &Path, repo_root: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Resolve the status JSON path (honours `COGNIS_INDEXD_STATUS_PATH`).
pub fn resolve_status_path(repo_root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("COGNIS_INDEXD_STATUS_PATH") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    repo_root
        .join(cognis_core::config::CONFIG_DIR_NAME)
        .join(STATUS_FILE_NAME)
}

// ---------------------------------------------------------------------------
// Status snapshot
// ---------------------------------------------------------------------------

/// Daemon status snapshot polled by IDE integrations. Field set mirrors the
/// Python `_compose_status_payload` so the existing extension reader is happy.
///
/// Task 5.2 adds `pending_files` / `inflight_count` / `inflight_files` so the
/// extension (`apps/cognis-vscode/src/indexd.ts`) can observe retained work and
/// never treat omitted vectors as complete (Requirement 2.6).
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub active: bool,
    pub phase: String,
    pub message: String,
    pub progress_percent: Option<f64>,
    pub pending_count: usize,
    /// Repo-relative paths still waiting to be processed / whose vectors are
    /// explicitly pending retry (capped for the status surface).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_files: Vec<String>,
    /// Files currently mid index (in-flight work).
    pub inflight_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight_files: Vec<String>,
    pub recent_files: Vec<String>,
    pub last_error: Option<String>,
    pub updated_at: f64,
}

impl DaemonStatus {
    fn base(phase: &str, message: &str, active: bool, progress: Option<f64>) -> Self {
        let mut s = DaemonStatus {
            pid: std::process::id(),
            active,
            phase: phase.to_string(),
            message: message.to_string(),
            progress_percent: progress,
            pending_count: 0,
            pending_files: Vec::new(),
            inflight_count: 0,
            inflight_files: Vec::new(),
            recent_files: Vec::new(),
            last_error: None,
            updated_at: 0.0,
        };
        s.touch();
        s
    }

    pub fn starting() -> Self {
        Self::base(
            "starting",
            "Starting live indexing daemon…",
            true,
            Some(5.0),
        )
    }
    pub fn watching() -> Self {
        Self::base("watching", "Watching for file changes.", true, Some(100.0))
    }
    pub fn stopped() -> Self {
        Self::base("stopped", "Live indexing stopped.", false, Some(0.0))
    }

    fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
    }

    /// Overlay retained / in-flight work from the pipeline onto this snapshot
    /// so completeness is observable (Requirement 2.6).
    pub fn apply_pipeline_work(&mut self, work: &cognis_indexer::PipelineWorkSnapshot) {
        // Prefer the pipeline's retained pending over a stale pre-batch count
        // when the pipeline reports pending vector work.
        if work.pending_count > 0 {
            self.pending_count = work.pending_count;
            self.pending_files = work.pending_files.clone();
        }
        self.inflight_count = work.inflight_count;
        self.inflight_files = work.inflight_files.clone();
    }
}

/// Write the status snapshot atomically (tmp file + rename), retrying the
/// rename a few times — a concurrent IDE reader can momentarily hold the
/// destination open on Windows (a transient sharing violation, not a failure).
pub fn write_status_file(path: &Path, status: &DaemonStatus) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(status)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text.as_bytes())?;

    let mut last_err = None;
    for attempt in 0..10 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("status rename failed")))
}

fn to_io(e: notify::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_cognis_and_configured_dirs() {
        let root = Path::new("/repo");
        let ignore: Vec<String> = vec!["node_modules".into(), "target".into()];
        assert!(should_ignore(&root.join(".cognis/uckg.db"), root, &ignore));
        assert!(should_ignore(
            &root.join("node_modules/pkg/index.js"),
            root,
            &ignore
        ));
        assert!(should_ignore(&root.join("target/debug/x"), root, &ignore));
        assert!(!should_ignore(&root.join("src/main.rs"), root, &ignore));
    }

    #[test]
    fn status_snapshots_serialize_with_expected_keys() {
        let s = DaemonStatus::watching();
        let v = serde_json::to_value(&s).unwrap();
        for key in [
            "pid",
            "active",
            "phase",
            "message",
            "progress_percent",
            "pending_count",
            "inflight_count",
            "recent_files",
            "last_error",
            "updated_at",
        ] {
            assert!(v.get(key).is_some(), "missing status key {key}");
        }
        assert_eq!(v["phase"], "watching");
        assert_eq!(v["active"], true);
        assert_eq!(v["inflight_count"], 0);
    }

    #[test]
    fn process_batch_updates_status() {
        let root = Path::new("/repo");
        let mut status = DaemonStatus::watching();
        let paths = vec![root.join("src/a.rs"), root.join("src/b.rs")];
        process_batch(root, &paths, &mut status);
        assert_eq!(status.phase, "incremental");
        assert_eq!(status.pending_count, 2);
        assert_eq!(status.recent_files.len(), 2);
        assert!(status.recent_files.iter().all(|p| p.starts_with("src/")));
    }

    #[test]
    fn write_status_file_roundtrips() {
        let dir = std::env::temp_dir().join(format!("cognis-indexd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("indexd-status.json");
        let s = DaemonStatus::starting();
        write_status_file(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["phase"], "starting");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_daemon_args() {
        for argv in [
            vec!["cognis-indexd"],
            vec!["cognis-indexd", "--repo-root", "."],
            vec!["cognis-indexd", "--full-rebuild"],
            vec!["cognis-indexd", "--db-path", "x.db"],
        ] {
            assert!(
                DaemonArgs::try_parse_from(argv.clone()).is_ok(),
                "parse {argv:?}"
            );
        }
    }
}
