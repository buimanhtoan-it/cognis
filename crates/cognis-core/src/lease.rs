//! Cross-process, repository-scoped ownership lease with heartbeat.
//!
//! Heavy daemons (`indexd`, `mcpd`) historically tracked ownership only in an
//! in-memory map (extension side) plus a best-effort status-file pid. After an
//! extension reload or crash the map is gone, there is no owner nonce, no
//! process-start identity, and no heartbeat/expiry — so a live orphan cannot be
//! reclaimed safely and a second heavy process can be spawned for the same
//! repository (bug facet `repoHasDuplicateHeavyDaemonOrOrphan`).
//!
//! This module is the repository-scoped lease the fixed system writes under
//! `.cognis/` (`indexd.lease`, `mcpd.lease`). The on-disk JSON shape is:
//!
//! ```json
//! {
//!   "owner_nonce": "<string>",
//!   "pid": 12345,
//!   "process_start_id": "<string>",
//!   "heartbeat_at": 1710000000.0,
//!   "expiry": 1710000030.0
//! }
//! ```
//!
//! matching the exploration test schema in
//! `apps/cognis-vscode/src/test/indexd.test.ts` (Requirements 2.7, 2.13;
//! Correctness Property 8; preservation 3.6, 3.9).
//!
//! ## Semantics
//!
//! * **Atomic write.** Lease files are written via temp-file + rename with a
//!   short retry (mirrors `write_status_file` in `cognis-indexd`) so concurrent
//!   readers never observe a truncated JSON body.
//! * **Acquire-or-attach.** On start a daemon tries to acquire. If a
//!   non-expired lease already exists it **attaches** (reports the live owner
//!   and does not become a second heavy owner). An expired heartbeat is treated
//!   as reclaimable.
//! * **Heartbeat.** The owner refreshes `heartbeat_at` and extends `expiry`.
//!   Readers treat `now >= expiry` as reclaimable. Safe stale-orphan cleanup
//!   that verifies PID + process-start identity before kill is Task 6.2; this
//!   module only records the identity fields and decides liveness from expiry.
//! * **Release.** Dropping a [`LeaseGuard`] (or calling [`LeaseGuard::release`])
//!   removes the lease file only when the on-disk `owner_nonce` still matches
//!   ours — never clobbering a newer owner's record (preservation 3.9).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::CONFIG_DIR_NAME;

/// Default time-to-live for a lease heartbeat window.
///
/// The owner refreshes well inside this window; readers treat a lease whose
/// `expiry` is in the past as reclaimable.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);

/// Lease file name for the live indexing daemon.
pub const INDEXD_LEASE_FILE: &str = "indexd.lease";

/// Lease file name for the MCP heavy daemon.
pub const MCPD_LEASE_FILE: &str = "mcpd.lease";

/// Which heavy daemon role a lease belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseRole {
    /// Live indexing daemon → `.cognis/indexd.lease`.
    Indexd,
    /// MCP heavy daemon → `.cognis/mcpd.lease`.
    Mcpd,
}

impl LeaseRole {
    /// On-disk file name for this role (under `.cognis/`).
    pub fn file_name(self) -> &'static str {
        match self {
            LeaseRole::Indexd => INDEXD_LEASE_FILE,
            LeaseRole::Mcpd => MCPD_LEASE_FILE,
        }
    }
}

/// On-disk lease record. Field names are snake_case JSON keys.
///
/// `pid` is a JSON number; `owner_nonce` is a string; `process_start_id` is a
/// string (the exploration test also accepts a number — we always write a
/// string). Timestamps are unix seconds as `f64`, matching the status-file
/// convention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub owner_nonce: String,
    pub pid: u32,
    pub process_start_id: String,
    pub heartbeat_at: f64,
    pub expiry: f64,
}

impl LeaseRecord {
    /// True when `now` is strictly before `expiry` — the heartbeat window is
    /// still open and readers should treat the owner as live.
    pub fn is_live_at(&self, now: f64) -> bool {
        now < self.expiry
    }

    /// True when the heartbeat window has elapsed (reclaimable).
    pub fn is_expired_at(&self, now: f64) -> bool {
        !self.is_live_at(now)
    }
}

/// Outcome of [`acquire_or_attach`].
#[derive(Debug)]
pub enum AcquireOutcome {
    /// This process now owns the lease. Keep the guard alive for the process
    /// lifetime; it heartbeats in the background and releases on drop.
    Acquired(LeaseGuard),
    /// A live, non-expired lease already exists for another owner. The caller
    /// MUST NOT start a duplicate heavy daemon — attach/reuse the existing one.
    Attached { lease: LeaseRecord, path: PathBuf },
}

/// RAII ownership of a repository-scoped lease.
///
/// Heartbeats run on a background thread until [`LeaseGuard::release`] or drop.
/// Release only unlinks the file when the on-disk nonce still matches ours.
#[derive(Debug)]
pub struct LeaseGuard {
    inner: Arc<Mutex<HeldLease>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct HeldLease {
    path: PathBuf,
    record: LeaseRecord,
    ttl: Duration,
}

impl LeaseGuard {
    /// Path of the lease file this guard owns.
    pub fn path(&self) -> PathBuf {
        self.inner
            .lock()
            .map(|h| h.path.clone())
            .unwrap_or_default()
    }

    /// Snapshot of the current on-disk record (as last written by us).
    pub fn record(&self) -> Option<LeaseRecord> {
        self.inner.lock().ok().map(|h| h.record.clone())
    }

    /// Force a heartbeat refresh now (also runs periodically in the background).
    pub fn heartbeat(&self) -> std::io::Result<()> {
        let mut held = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("lease mutex poisoned"))?;
        held.heartbeat()
    }

    /// Stop the heartbeat thread and remove the lease file if we still own it.
    pub fn release(mut self) -> std::io::Result<()> {
        self.stop_and_join();
        let mut held = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("lease mutex poisoned"))?;
        held.release()
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.stop_and_join();
        if let Ok(mut held) = self.inner.lock() {
            let _ = held.release();
        }
    }
}

impl HeldLease {
    fn heartbeat(&mut self) -> std::io::Result<()> {
        let now = unix_now();
        self.record.heartbeat_at = now;
        self.record.expiry = now + duration_secs_f64(self.ttl);
        // Only refresh if we still own the file (nonce match). If someone else
        // reclaimed underneath us, refuse to clobber their record.
        if let Some(existing) = read_lease(&self.path)? {
            if existing.owner_nonce != self.record.owner_nonce {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "lease ownership lost; another owner holds the file",
                ));
            }
        }
        write_lease_atomic(&self.path, &self.record)
    }

    fn release(&mut self) -> std::io::Result<()> {
        match read_lease(&self.path)? {
            Some(existing) if existing.owner_nonce == self.record.owner_nonce => {
                match std::fs::remove_file(&self.path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                }
            }
            // Missing or owned by someone else — safe non-destruction (3.9).
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Public path / IO helpers
// ---------------------------------------------------------------------------

/// Resolve the lease file path for `role` under `<repo_root>/.cognis/`.
pub fn lease_path(repo_root: impl AsRef<Path>, role: LeaseRole) -> PathBuf {
    repo_root
        .as_ref()
        .join(CONFIG_DIR_NAME)
        .join(role.file_name())
}

/// Read a lease file. Returns `Ok(None)` when the file is absent; returns an
/// error for I/O failures or invalid JSON (callers may treat invalid JSON as
/// reclaimable by discarding the error and overwriting).
pub fn read_lease(path: impl AsRef<Path>) -> std::io::Result<Option<LeaseRecord>> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let record = serde_json::from_str::<LeaseRecord>(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid lease JSON at {}: {e}", path.display()),
                )
            })?;
            Ok(Some(record))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write a lease record atomically (temp file + rename with short retry).
pub fn write_lease_atomic(path: impl AsRef<Path>, record: &LeaseRecord) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Sibling temp with a unique suffix so concurrent writers don't collide on
    // the temp name itself.
    let tmp = path.with_extension(format!(
        "lease.tmp.{}-{}",
        std::process::id(),
        next_tmp_seq()
    ));
    std::fs::write(&tmp, text.as_bytes())?;

    let mut last_err = None;
    for attempt in 0..10u32 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                thread::sleep(Duration::from_millis(20 * (attempt as u64 + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("lease rename failed")))
}

/// Acquire the repository-scoped lease for `role`, or attach to a live owner.
///
/// * If no lease exists, or the existing lease's heartbeat has expired, this
///   process becomes the owner and returns [`AcquireOutcome::Acquired`].
/// * If a non-expired lease exists, returns [`AcquireOutcome::Attached`] so the
///   caller can reuse the live owner instead of spawning a duplicate.
///
/// Uses [`DEFAULT_LEASE_TTL`] when `ttl` is `None`.
pub fn acquire_or_attach(
    repo_root: impl AsRef<Path>,
    role: LeaseRole,
    ttl: Option<Duration>,
) -> std::io::Result<AcquireOutcome> {
    let ttl = ttl.unwrap_or(DEFAULT_LEASE_TTL);
    let path = lease_path(repo_root.as_ref(), role);
    let now = unix_now();

    // Fast path: live non-expired lease → attach.
    match read_lease(&path) {
        Ok(Some(existing)) if existing.is_live_at(now) => {
            // If *we* already own it (same pid + start id), re-acquire rather
            // than attach to ourselves — supports re-entrant start in tests.
            if existing.pid == std::process::id()
                && existing.process_start_id == this_process_start_id()
            {
                // Fall through to (re)acquire with a fresh nonce/heartbeat.
            } else {
                return Ok(AcquireOutcome::Attached {
                    lease: existing,
                    path,
                });
            }
        }
        Ok(Some(_)) | Ok(None) => {
            // Expired or missing → reclaimable.
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            // Corrupt lease: treat as reclaimable and overwrite.
        }
        Err(e) => return Err(e),
    }

    let record = new_owner_record(ttl);
    write_lease_atomic(&path, &record)?;

    // Verify we still own it after the write (lost-race detection).
    match read_lease(&path)? {
        Some(on_disk) if on_disk.owner_nonce == record.owner_nonce => {
            Ok(AcquireOutcome::Acquired(spawn_guard(path, record, ttl)))
        }
        Some(on_disk) if on_disk.is_live_at(unix_now()) => Ok(AcquireOutcome::Attached {
            lease: on_disk,
            path,
        }),
        Some(_expired) => {
            // Winner's lease already expired (clock skew / tiny TTL in tests):
            // retry once by overwriting.
            let record = new_owner_record(ttl);
            write_lease_atomic(&path, &record)?;
            // Final verify.
            match read_lease(&path)? {
                Some(v) if v.owner_nonce == record.owner_nonce => {
                    Ok(AcquireOutcome::Acquired(spawn_guard(path, record, ttl)))
                }
                Some(v) => Ok(AcquireOutcome::Attached { lease: v, path }),
                None => {
                    // Vanished between write and read — treat as acquired of our write.
                    let _ = write_lease_atomic(&path, &record);
                    Ok(AcquireOutcome::Acquired(spawn_guard(path, record, ttl)))
                }
            }
        }
        None => {
            // Vanished; re-write and own.
            let record = new_owner_record(ttl);
            write_lease_atomic(&path, &record)?;
            Ok(AcquireOutcome::Acquired(spawn_guard(path, record, ttl)))
        }
    }
}

/// Build a fresh owner record for this process.
pub fn new_owner_record(ttl: Duration) -> LeaseRecord {
    let now = unix_now();
    LeaseRecord {
        owner_nonce: generate_owner_nonce(),
        pid: std::process::id(),
        process_start_id: this_process_start_id(),
        heartbeat_at: now,
        expiry: now + duration_secs_f64(ttl),
    }
}

/// Resolve a repository root for lease placement when only env is available
/// (mcpd). Preference order:
///
/// 1. `COGNIS_REPO_ROOT` when set and non-empty
/// 2. Parent of the `.cognis` directory containing `COGNIS_DB_PATH`
/// 3. Parent directory of `COGNIS_DB_PATH`
/// 4. Current working directory
pub fn resolve_repo_root_from_env() -> PathBuf {
    if let Ok(root) = std::env::var("COGNIS_REPO_ROOT") {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(db) = std::env::var("COGNIS_DB_PATH") {
        let trimmed = db.trim();
        if !trimmed.is_empty() {
            let db_path = PathBuf::from(trimmed);
            if let Some(parent) = db_path.parent() {
                if parent
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(CONFIG_DIR_NAME))
                {
                    if let Some(repo) = parent.parent() {
                        return repo.to_path_buf();
                    }
                }
                return parent.to_path_buf();
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn spawn_guard(path: PathBuf, record: LeaseRecord, ttl: Duration) -> LeaseGuard {
    let held = Arc::new(Mutex::new(HeldLease { path, record, ttl }));
    let stop = Arc::new(AtomicBool::new(false));
    let join = {
        let held = Arc::clone(&held);
        let stop = Arc::clone(&stop);
        // Refresh at roughly TTL/3 so a single missed beat still leaves the
        // lease live. Floor at 100 ms so tiny test TTLs still make progress.
        let interval = std::cmp::max(ttl / 3, Duration::from_millis(100));
        Some(thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(interval);
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(mut h) = held.lock() {
                    // Best-effort: a lost race / permission error ends the loop
                    // so we don't keep hammering a foreign lease.
                    if h.heartbeat().is_err() {
                        break;
                    }
                } else {
                    break;
                }
            }
        }))
    };
    LeaseGuard {
        inner: held,
        stop,
        join,
    }
}

fn generate_owner_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix pid + wall time + a stack address so concurrent acquirers almost
    // never collide without pulling in a RNG dependency.
    let mix = std::ptr::addr_of!(nanos) as usize;
    format!("{:x}-{:x}-{:x}", std::process::id(), nanos, mix)
}

/// Process-start identity recorded once per process. Used as the lease's
/// `process_start_id` so a later reclaim step (Task 6.2) can distinguish a
/// live owner from a PID-reused unrelated process.
fn this_process_start_id() -> String {
    static START_ID: OnceLock<String> = OnceLock::new();
    START_ID
        .get_or_init(|| {
            // Prefer a stable OS-level creation stamp when available; fall back
            // to the first wall-clock observation in this process.
            read_os_process_start_id().unwrap_or_else(|| {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("start-{}", nanos)
            })
        })
        .clone()
}

#[cfg(windows)]
fn read_os_process_start_id() -> Option<String> {
    // Avoid a winapi/windows-sys dependency in cognis-core: shell out is too
    // heavy and racy. Record the first-observed wall clock instead; Task 6.2
    // can strengthen verification with a proper process-start query on the
    // TypeScript side (where `process.pid` + WMI/NtQuery already exist for
    // kill/isAlive). Returning None forces the wall-clock fallback.
    None
}

#[cfg(not(windows))]
fn read_os_process_start_id() -> Option<String> {
    // Best-effort: /proc/self on Linux exposes starttime (clock ticks).
    // Format as a string so the lease schema stays opaque to readers.
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // stat fields: pid (comm) ... starttime is field 22 (1-indexed) after the
    // comm field which may contain spaces inside parentheses. Find the trailing
    // ')' of comm and split the rest.
    let after_comm = stat.rsplit_once(')')?.1;
    let field = after_comm.split_whitespace().nth(19)?; // 22nd overall ⇒ index 19 after comm
    Some(format!("proc-starttime-{field}"))
}

/// Monotonic per-process counter for unique temp-file suffixes, so concurrent
/// atomic writes never collide on the sibling temp name.
fn next_tmp_seq() -> u64 {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::SeqCst)
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn duration_secs_f64(d: Duration) -> f64 {
    d.as_secs_f64()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn temp_repo() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "cognis-lease-test-{}-{}-{}",
            std::process::id(),
            n,
            unix_now().to_bits()
        ));
        std::fs::create_dir_all(dir.join(CONFIG_DIR_NAME)).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lease_path_uses_cognis_dir_and_role_name() {
        let root = Path::new("/repo");
        assert_eq!(
            lease_path(root, LeaseRole::Indexd),
            PathBuf::from("/repo/.cognis/indexd.lease")
        );
        assert_eq!(
            lease_path(root, LeaseRole::Mcpd),
            PathBuf::from("/repo/.cognis/mcpd.lease")
        );
    }

    #[test]
    fn write_read_roundtrip_preserves_schema() {
        let repo = temp_repo();
        let path = lease_path(&repo, LeaseRole::Indexd);
        let record = new_owner_record(DEFAULT_LEASE_TTL);
        write_lease_atomic(&path, &record).unwrap();

        let loaded = read_lease(&path).unwrap().expect("lease should exist");
        assert_eq!(loaded.pid, std::process::id());
        assert!(!loaded.owner_nonce.is_empty());
        assert!(!loaded.process_start_id.is_empty());
        assert!(loaded.expiry > loaded.heartbeat_at);

        // JSON shape: pid is a number, owner_nonce is a string.
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["pid"].is_number());
        assert!(raw["owner_nonce"].is_string());
        assert!(raw["process_start_id"].is_string() || raw["process_start_id"].is_number());
        assert!(raw["heartbeat_at"].is_number());
        assert!(raw["expiry"].is_number());

        cleanup(&repo);
    }

    #[test]
    fn expired_lease_is_reclaimable() {
        let repo = temp_repo();
        let path = lease_path(&repo, LeaseRole::Indexd);
        let mut stale = new_owner_record(Duration::from_secs(1));
        // Force expiry into the past.
        stale.heartbeat_at = 1.0;
        stale.expiry = 2.0;
        stale.pid = 1; // not us
        stale.owner_nonce = "stale-nonce".into();
        stale.process_start_id = "stale-start".into();
        write_lease_atomic(&path, &stale).unwrap();

        assert!(stale.is_expired_at(unix_now()));

        match acquire_or_attach(&repo, LeaseRole::Indexd, Some(Duration::from_secs(5))).unwrap() {
            AcquireOutcome::Acquired(guard) => {
                let rec = guard.record().unwrap();
                assert_eq!(rec.pid, std::process::id());
                assert_ne!(rec.owner_nonce, "stale-nonce");
                // Drop releases.
                drop(guard);
            }
            AcquireOutcome::Attached { .. } => panic!("expired lease must be reclaimable"),
        }
        cleanup(&repo);
    }

    #[test]
    fn live_lease_attaches_instead_of_duplicating() {
        let repo = temp_repo();
        let path = lease_path(&repo, LeaseRole::Mcpd);
        // Simulate a *different* live owner (different pid).
        let mut foreign = new_owner_record(Duration::from_secs(60));
        foreign.pid = std::process::id().wrapping_add(99999).max(1);
        if foreign.pid == std::process::id() {
            foreign.pid = 1;
        }
        foreign.owner_nonce = "foreign-nonce".into();
        foreign.process_start_id = "foreign-start".into();
        write_lease_atomic(&path, &foreign).unwrap();

        match acquire_or_attach(&repo, LeaseRole::Mcpd, Some(Duration::from_secs(60))).unwrap() {
            AcquireOutcome::Attached { lease, path: p } => {
                assert_eq!(lease.owner_nonce, "foreign-nonce");
                assert_eq!(lease.pid, foreign.pid);
                assert_eq!(p, path);
            }
            AcquireOutcome::Acquired(_) => {
                panic!("live foreign lease must attach, not acquire")
            }
        }
        cleanup(&repo);
    }

    #[test]
    fn acquire_when_missing_writes_lease() {
        let repo = temp_repo();
        let path = lease_path(&repo, LeaseRole::Indexd);
        assert!(!path.exists());

        let guard = match acquire_or_attach(&repo, LeaseRole::Indexd, Some(Duration::from_secs(10)))
            .unwrap()
        {
            AcquireOutcome::Acquired(g) => g,
            AcquireOutcome::Attached { .. } => panic!("empty path should acquire"),
        };

        assert!(path.exists());
        let rec = guard.record().unwrap();
        assert_eq!(rec.pid, std::process::id());
        assert!(!rec.owner_nonce.is_empty());
        assert!(!rec.process_start_id.is_empty());

        // Heartbeat advances expiry.
        let before = rec.expiry;
        thread::sleep(Duration::from_millis(20));
        guard.heartbeat().unwrap();
        let after = guard.record().unwrap().expiry;
        assert!(after >= before);

        guard.release().unwrap();
        assert!(!path.exists(), "release must remove our lease file");
        cleanup(&repo);
    }

    #[test]
    fn release_does_not_clobber_foreign_owner() {
        let repo = temp_repo();
        let path = lease_path(&repo, LeaseRole::Indexd);
        let guard = match acquire_or_attach(&repo, LeaseRole::Indexd, Some(Duration::from_secs(10)))
            .unwrap()
        {
            AcquireOutcome::Acquired(g) => g,
            AcquireOutcome::Attached { .. } => panic!("expected acquire"),
        };

        // Overwrite with a foreign owner while we still hold the guard.
        let mut foreign = new_owner_record(Duration::from_secs(60));
        foreign.owner_nonce = "other".into();
        foreign.pid = 42;
        write_lease_atomic(&path, &foreign).unwrap();

        // Release must be a no-op (safe non-destruction).
        guard.release().unwrap();
        let still = read_lease(&path).unwrap().unwrap();
        assert_eq!(still.owner_nonce, "other");
        assert_eq!(still.pid, 42);
        cleanup(&repo);
    }

    #[test]
    fn resolve_repo_root_from_db_path_parent() {
        // SAFETY: tests in this crate run single-threaded per process for env
        // mutation in practice; we restore after.
        let prev_repo = std::env::var_os("COGNIS_REPO_ROOT");
        let prev_db = std::env::var_os("COGNIS_DB_PATH");
        std::env::remove_var("COGNIS_REPO_ROOT");
        std::env::set_var(
            "COGNIS_DB_PATH",
            if cfg!(windows) {
                r"D:\work\myrepo\.cognis\uckg.db"
            } else {
                "/work/myrepo/.cognis/uckg.db"
            },
        );
        let root = resolve_repo_root_from_env();
        if cfg!(windows) {
            assert_eq!(root, PathBuf::from(r"D:\work\myrepo"));
        } else {
            assert_eq!(root, PathBuf::from("/work/myrepo"));
        }
        // restore
        match prev_repo {
            Some(v) => std::env::set_var("COGNIS_REPO_ROOT", v),
            None => std::env::remove_var("COGNIS_REPO_ROOT"),
        }
        match prev_db {
            Some(v) => std::env::set_var("COGNIS_DB_PATH", v),
            None => std::env::remove_var("COGNIS_DB_PATH"),
        }
    }
}
