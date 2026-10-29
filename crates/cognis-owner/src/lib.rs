//! `Workspace_Owner` — one heavy process per canonical workspace (task 13.2).
//!
//! Wires [`cognis_core::lease::acquire_or_attach`] under [`LeaseRole::Owner`]
//! (`.cognis/owner.lease`), keyed by [`RepoIdentity`] so symlink / case /
//! trailing-separator spellings collapse to one owner (Requirement 16.5).
//!
//! * [`OwnerSession::Acquired`] — this process holds the lease: open the
//!   database, verify required indexes, start the file watcher, start the
//!   query listener, and drain due embedding-queue rows (Requirements 16.2,
//!   9.7, 11.2).
//! * [`OwnerSession::Attached`] — a live owner already exists: publish that
//!   owner's endpoint and **do not** open the database (Requirement 16.3).
//! * Drop / [`OwnedWorkspace::release`] frees the lease so a later process can
//!   become owner (Requirement 16.4).

pub mod queue;
pub mod run;
pub mod status;

pub use queue::{Observation, ObservationKind, WatcherQueue};
pub use run::{
    ensure_owner_running, heavy_surface_args, is_busybox_stem, run, run_from, BECOME_OWNER_ENV,
};
pub use status::{owner_status_path, write_owner_status, OwnerStatus, OWNER_STATUS_FILE};

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cognis_core::{
    acquire_or_attach, lease_path, AcquireOutcome, Capabilities, CognisError, Config, LeaseGuard,
    LeaseRole, RepoIdentity, Result, SemanticWarmPolicy,
};
use cognis_embed::{failure_cooldown_from_env, EvictOutcome, ModelFingerprint, ModelSlot};
use cognis_indexer::EmbeddingQueueDrainer;
use cognis_store::{Database, GenerationSnapshot};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Published address of a workspace owner that query clients can attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEndpoint {
    pub pid: u32,
    pub lease_path: PathBuf,
    pub query_url: Option<String>,
}

/// Outcome of trying to become the workspace owner.
#[allow(clippy::large_enum_variant)]
pub enum OwnerSession {
    /// This process owns the workspace (lease, database, watcher, listener).
    Acquired(OwnedWorkspace),
    /// A live owner already holds the lease; this process did not open the DB.
    Attached(AttachedWorkspace),
}

impl OwnerSession {
    /// Canonical identity this session resolved.
    pub fn identity(&self) -> &RepoIdentity {
        match self {
            OwnerSession::Acquired(owned) => &owned.identity,
            OwnerSession::Attached(attached) => &attached.identity,
        }
    }

    /// Endpoint a later process should publish / attach to.
    pub fn endpoint(&self) -> OwnerEndpoint {
        match self {
            OwnerSession::Acquired(owned) => owned.endpoint(),
            OwnerSession::Attached(attached) => attached.endpoint.clone(),
        }
    }

    /// Whether this process opened the workspace database.
    pub fn opened_database(&self) -> bool {
        matches!(self, OwnerSession::Acquired(_))
    }
}

/// Live owner: holds the lease and the workspace resources Requirement 16.2
/// assigns to a single process.
pub struct OwnedWorkspace {
    identity: RepoIdentity,
    db: Option<Database>,
    watcher: Option<FileWatcher>,
    queue: Arc<WatcherQueue>,
    query: Option<QueryListener>,
    missing_indexes: Vec<String>,
    pending_vector_count: usize,
    model: ModelSlot,
    config: Config,
    idle_evict_after: Duration,
    in_flight_tools: Arc<AtomicUsize>,
    lease: Option<LeaseGuard>,
}

impl OwnedWorkspace {
    pub fn identity(&self) -> &RepoIdentity {
        &self.identity
    }

    pub fn endpoint(&self) -> OwnerEndpoint {
        OwnerEndpoint {
            pid: std::process::id(),
            lease_path: self
                .lease
                .as_ref()
                .map(LeaseGuard::path)
                .unwrap_or_else(|| owner_lease_path(Path::new(&self.identity.root))),
            query_url: self.query.as_ref().map(|listener| listener.url.clone()),
        }
    }

    pub fn watcher_running(&self) -> bool {
        self.watcher
            .as_ref()
            .is_some_and(|watcher| watcher.running.load(Ordering::SeqCst))
    }

    /// Current watcher-queue depth for the status surface (Requirement 18.5).
    pub fn watcher_queue_depth(&self) -> usize {
        self.queue.depth()
    }

    pub fn query_listener_running(&self) -> bool {
        self.query
            .as_ref()
            .is_some_and(|listener| listener.running.load(Ordering::SeqCst))
    }

    /// Required indexes absent after open. Empty means queries may be served.
    pub fn missing_indexes(&self) -> &[String] {
        &self.missing_indexes
    }

    pub fn pending_vector_count(&self) -> usize {
        self.pending_vector_count
    }

    /// Whether a model session is currently resident (Requirement 20.7).
    pub fn model_resident(&self) -> bool {
        self.model.is_loaded()
    }

    pub fn set_idle_evict_after(&mut self, idle: Duration) {
        self.idle_evict_after = idle;
    }

    /// Mark a tool call as in flight so idle eviction stays refused (20.2).
    pub fn note_tool_call_start(&self) {
        self.in_flight_tools.fetch_add(1, Ordering::SeqCst);
    }

    pub fn note_tool_call_end(&self) {
        self.in_flight_tools.fetch_sub(1, Ordering::SeqCst);
    }

    /// Release the model only when idle, the embedding queue is empty, and no
    /// tool call is in flight (Requirement 20.2). A successful release is
    /// timestamped in the diagnostics log (20.3).
    pub fn try_idle_evict_model(&mut self) -> EvictOutcome {
        let pending = self.live_pending_vector_count();
        if pending > 0 {
            return EvictOutcome::InFlight { count: pending };
        }
        let tools = self.in_flight_tools.load(Ordering::SeqCst);
        if tools > 0 {
            return EvictOutcome::InFlight { count: tools };
        }
        let outcome = self.model.try_idle_evict(self.idle_evict_after);
        if matches!(outcome, EvictOutcome::Evicted) {
            eprintln!(
                "cognis-owner: released model session at unix_ts={:.3} workspace={}",
                OwnerStatus::now_secs(),
                self.identity.wire_key()
            );
            let _ = self.write_status();
        }
        outcome
    }

    /// Rewrite `.cognis/owner-status.json` from the live owner state.
    pub fn write_status(&mut self) -> std::io::Result<()> {
        let path = owner_status_path(Path::new(&self.identity.root));
        let status = self.owner_status();
        self.pending_vector_count = status.pending_vector_count.max(0) as usize;
        write_owner_status(&path, &status)
    }

    fn live_pending_vector_count(&self) -> usize {
        let Some(db) = self.db.as_ref() else {
            return self.pending_vector_count;
        };
        db.connect()
            .ok()
            .and_then(|conn| {
                conn.query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| {
                    row.get::<_, i64>(0)
                })
                .ok()
            })
            .map(|count| count.max(0) as usize)
            .unwrap_or(self.pending_vector_count)
    }

    fn owner_status(&self) -> OwnerStatus {
        let (source_generation, semantic_generation, pending_vector_count) = match self
            .db
            .as_ref()
            .and_then(|db| GenerationSnapshot::open(db).ok())
        {
            Some(snapshot) => (
                snapshot.source_generation(),
                snapshot.semantic_generation(),
                snapshot.pending_vector_count(),
            ),
            None => (0, 0, self.pending_vector_count as i64),
        };
        let last_error = if self.missing_indexes.is_empty() {
            None
        } else {
            Some(format!(
                "missing required index: {}",
                self.missing_indexes.join(", ")
            ))
        };
        OwnerStatus {
            pid: std::process::id(),
            active: true,
            phase: if self.watcher_running() {
                "watching".into()
            } else {
                "idle".into()
            },
            message: if last_error.is_some() {
                "Owner running with missing indexes; lexical-only.".into()
            } else if self.model_resident() {
                "Owner running.".into()
            } else {
                "Owner running; model not resident.".into()
            },
            progress_percent: Some(100.0),
            pending_count: 0,
            pending_files: Vec::new(),
            inflight_count: self.in_flight_tools.load(Ordering::SeqCst),
            inflight_files: Vec::new(),
            recent_files: Vec::new(),
            updated_at: OwnerStatus::now_secs(),
            last_error,
            source_generation,
            semantic_generation,
            pending_vector_count,
            watcher_queue_depth: self.queue.depth(),
            model_resident: self.model_resident(),
            fingerprint_match: fingerprint_match(self.db.as_ref(), &self.config),
            capabilities: Capabilities {
                semantic_backend: self.model_resident() || self.config.embedder.backend == "stub",
                accelerated_vector_search: cognis_store::accelerated_vector_search_available(),
            },
            canonical_workspace: self.identity.wire_key(),
        }
    }

    /// The opened workspace database, if still held.
    pub fn database(&self) -> Option<&Database> {
        self.db.as_ref()
    }

    /// Stop services and release the lease so a later process can acquire it.
    pub fn release(mut self) -> Result<()> {
        self.shutdown_services();
        match self.lease.take() {
            Some(lease) => lease
                .release()
                .map_err(|error| CognisError::Config(format!("owner lease release: {error}"))),
            None => Ok(()),
        }
    }

    fn shutdown_services(&mut self) {
        if let Some(query) = self.query.take() {
            query.stop();
        }
        self.watcher.take();
        if let Some(db) = self.db.take() {
            db.close_thread_connection();
        }
    }
}

impl Drop for OwnedWorkspace {
    fn drop(&mut self) {
        self.shutdown_services();
    }
}

/// Attach-only view of a live owner. No database connection is held.
#[derive(Debug, Clone)]
pub struct AttachedWorkspace {
    identity: RepoIdentity,
    endpoint: OwnerEndpoint,
}

impl AttachedWorkspace {
    pub fn identity(&self) -> &RepoIdentity {
        &self.identity
    }

    pub fn endpoint(&self) -> &OwnerEndpoint {
        &self.endpoint
    }
}

struct FileWatcher {
    _watcher: RecommendedWatcher,
    running: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

struct QueryListener {
    url: String,
    addr: SocketAddr,
    running: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl QueryListener {
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for QueryListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Become the workspace owner, or attach to the live owner, for `repo_root`.
///
/// The lease key is the canonical [`RepoIdentity`] root, so two spellings of
/// the same directory share `.cognis/owner.lease`.
pub fn become_owner(repo_root: impl AsRef<Path>) -> Result<OwnerSession> {
    become_owner_with_policy(repo_root, SemanticWarmPolicy::from_env())
}

/// Become owner with an explicit warm policy (tests + daemon entry points).
pub fn become_owner_with_policy(
    repo_root: impl AsRef<Path>,
    policy: SemanticWarmPolicy,
) -> Result<OwnerSession> {
    let repo_root = repo_root.as_ref();
    let identity = RepoIdentity::from_paths(repo_root, RepoIdentity::default_db_path(repo_root));
    let canonical_root = PathBuf::from(&identity.root);
    let config = Config::load(&canonical_root).unwrap_or_default();
    let ttl = Duration::from_secs(config.owner.lease_ttl_s.max(1));

    match acquire_or_attach(&canonical_root, LeaseRole::Owner, Some(ttl)) {
        Ok(AcquireOutcome::Attached { lease, path }) => {
            Ok(OwnerSession::Attached(AttachedWorkspace {
                identity,
                endpoint: OwnerEndpoint {
                    pid: lease.pid,
                    lease_path: path,
                    query_url: advertised_query_url(&canonical_root),
                },
            }))
        }
        Ok(AcquireOutcome::Acquired(lease)) => {
            acquire_resources(identity, config, lease, policy, QueryListenMode::Dummy)
                .map(OwnerSession::Acquired)
        }
        Err(error) => Err(CognisError::Config(format!("owner lease: {error}"))),
    }
}

/// How the owner exposes its query surface while holding the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryListenMode {
    /// Background TCP accept-and-drop used by unit tests of `become_owner`.
    Dummy,
    /// No listener yet — the daemon entry binds MCP HTTP on the requested port.
    Deferred,
}

/// Become owner without starting the dummy query listener. The daemon entry
/// binds the real MCP HTTP surface afterwards.
pub fn become_owner_deferred(
    repo_root: impl AsRef<Path>,
    policy: SemanticWarmPolicy,
) -> Result<OwnerSession> {
    let repo_root = repo_root.as_ref();
    let identity = RepoIdentity::from_paths(repo_root, RepoIdentity::default_db_path(repo_root));
    let canonical_root = PathBuf::from(&identity.root);
    let config = Config::load(&canonical_root).unwrap_or_default();
    let ttl = Duration::from_secs(config.owner.lease_ttl_s.max(1));

    match acquire_or_attach(&canonical_root, LeaseRole::Owner, Some(ttl)) {
        Ok(AcquireOutcome::Attached { lease, path }) => {
            Ok(OwnerSession::Attached(AttachedWorkspace {
                identity,
                endpoint: OwnerEndpoint {
                    pid: lease.pid,
                    lease_path: path,
                    query_url: advertised_query_url(&canonical_root),
                },
            }))
        }
        Ok(AcquireOutcome::Acquired(lease)) => {
            acquire_resources(identity, config, lease, policy, QueryListenMode::Deferred)
                .map(OwnerSession::Acquired)
        }
        Err(error) => Err(CognisError::Config(format!("owner lease: {error}"))),
    }
}

/// Attach to a live owner without opening the database. Returns `None` when
/// no owner holds the lease (the lease is released immediately if this
/// process won a vacant acquire).
pub fn attach_existing_owner(repo_root: impl AsRef<Path>) -> Result<Option<AttachedWorkspace>> {
    let repo_root = repo_root.as_ref();
    let identity = RepoIdentity::from_paths(repo_root, RepoIdentity::default_db_path(repo_root));
    let canonical_root = PathBuf::from(&identity.root);
    let config = Config::load(&canonical_root).unwrap_or_default();
    let ttl = Duration::from_secs(config.owner.lease_ttl_s.max(1));

    match acquire_or_attach(&canonical_root, LeaseRole::Owner, Some(ttl)) {
        Ok(AcquireOutcome::Attached { lease, path }) => Ok(Some(AttachedWorkspace {
            identity,
            endpoint: OwnerEndpoint {
                pid: lease.pid,
                lease_path: path,
                query_url: advertised_query_url(&canonical_root),
            },
        })),
        Ok(AcquireOutcome::Acquired(guard)) => {
            let _ = guard.release();
            Ok(None)
        }
        Err(error) => Err(CognisError::Config(format!("owner lease: {error}"))),
    }
}

fn advertised_query_url(repo_root: &Path) -> Option<String> {
    let path = repo_root
        .join(cognis_core::config::CONFIG_DIR_NAME)
        .join("mcpd.endpoint");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Atomically write `.cognis/mcpd.endpoint` (URL + optional credential +
/// optional model fingerprint) so thin adapters can attach.
pub fn write_mcpd_endpoint(
    repo_root: impl AsRef<Path>,
    url: &str,
    credential: Option<&str>,
    fingerprint: Option<&str>,
) -> std::io::Result<()> {
    let repo_root = repo_root.as_ref();
    let dir = repo_root.join(cognis_core::config::CONFIG_DIR_NAME);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mcpd.endpoint");
    let mut body = format!("{}\n", url.trim());
    if let Some(token) = credential.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(token);
        body.push('\n');
        if let Some(fp) = fingerprint.map(str::trim).filter(|s| !s.is_empty()) {
            body.push_str(fp);
            body.push('\n');
        }
    }
    let tmp = path.with_extension(format!("endpoint.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body)?;
    let mut last_err = None;
    for _ in 0..5 {
        match std::fs::rename(&tmp, &path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("endpoint rename failed")))
}

fn acquire_resources(
    identity: RepoIdentity,
    config: Config,
    lease: LeaseGuard,
    policy: SemanticWarmPolicy,
    query_mode: QueryListenMode,
) -> Result<OwnedWorkspace> {
    let mut db = Database::open(&identity.db_path)?;
    let missing_indexes = db.missing_required_indexes()?;
    let cooldown = failure_cooldown_from_env();
    let idle_evict_after = Duration::from_secs(config.owner.idle_evict_after_s.max(1));
    let model = ModelSlot::empty();
    if policy.is_eager() {
        let _ = model.warm_with(cooldown, || cognis_embed::build_embedder(&config));
    }
    let pending_vector_count = drain_embedding_queue_in_place(&mut db, &config, &model)?;
    let cap = config.owner.watcher_queue_max as usize;
    let debounce = Duration::from_millis(config.owner.watcher_debounce_ms);
    let queue = Arc::new(WatcherQueue::new(cap));
    let watcher = start_watcher(Path::new(&identity.root), queue.clone(), debounce)?;
    let in_flight_tools = Arc::new(AtomicUsize::new(0));
    let query = if query_mode == QueryListenMode::Dummy && missing_indexes.is_empty() {
        Some(start_query_listener(in_flight_tools.clone())?)
    } else {
        None
    };

    let mut owned = OwnedWorkspace {
        identity,
        db: Some(db),
        watcher: Some(watcher),
        queue,
        query,
        missing_indexes,
        pending_vector_count,
        model,
        config,
        idle_evict_after,
        in_flight_tools,
        lease: Some(lease),
    };
    let _ = owned.write_status();
    Ok(owned)
}

fn drain_embedding_queue_in_place(
    db: &mut Database,
    config: &Config,
    model: &ModelSlot,
) -> Result<usize> {
    let cooldown = failure_cooldown_from_env();
    let mut drainer = EmbeddingQueueDrainer::new(db, model, config, true, cooldown);
    let outcome = drainer.drain(config.owner.embed_batch_budget as usize)?;
    Ok(outcome.pending_vector_count)
}

fn fingerprint_match(db: Option<&Database>, config: &Config) -> bool {
    let Some(db) = db else {
        return true;
    };
    let Ok(snapshot) = GenerationSnapshot::open(db) else {
        return false;
    };
    let Ok(persisted) = snapshot.vector_space_fingerprint() else {
        return false;
    };
    let Some(persisted) = persisted else {
        return true;
    };
    let query = ModelFingerprint::from_env_or_derive(config);
    persisted.digest.eq_ignore_ascii_case(&query.digest)
}

fn start_watcher(
    repo_root: &Path,
    queue: Arc<WatcherQueue>,
    debounce: Duration,
) -> Result<FileWatcher> {
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let offered = queue.clone();
    let watched_root = repo_root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        let Some(kind) = observation_kind(event.kind) else {
            return;
        };
        for path in event.paths {
            if is_owner_internal_path(&watched_root, &path) {
                continue;
            }
            offered.offer(Observation { path, kind });
        }
    })
    .map_err(|error| CognisError::Config(format!("owner watcher: {error}")))?;
    watcher
        .watch(repo_root, RecursiveMode::Recursive)
        .map_err(|error| CognisError::Config(format!("owner watcher: {error}")))?;

    let stop_thread = stop.clone();
    let running_thread = running.clone();
    let join = thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            // Keep the existing debounce window: the notify callback already
            // offered into WatcherQueue without blocking. This thread only
            // paces shutdown / future drains.
            thread::sleep(debounce.max(Duration::from_millis(1)));
        }
        running_thread.store(false, Ordering::SeqCst);
    });

    Ok(FileWatcher {
        _watcher: watcher,
        running,
        stop,
        join: Some(join),
    })
}

fn is_owner_internal_path(repo_root: &Path, path: &Path) -> bool {
    let cognis = repo_root.join(cognis_core::config::CONFIG_DIR_NAME);
    path.starts_with(&cognis)
        || path
            .components()
            .any(|component| component.as_os_str() == cognis_core::config::CONFIG_DIR_NAME)
}

fn observation_kind(kind: EventKind) -> Option<ObservationKind> {
    match kind {
        EventKind::Create(_) => Some(ObservationKind::Create),
        EventKind::Modify(_) => Some(ObservationKind::Modify),
        EventKind::Remove(_) => Some(ObservationKind::Delete),
        _ => None,
    }
}

fn start_query_listener(in_flight_tools: Arc<AtomicUsize>) -> Result<QueryListener> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| CognisError::Config(format!("owner query listener: {error}")))?;
    let addr = listener
        .local_addr()
        .map_err(|error| CognisError::Config(format!("owner query listener: {error}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| CognisError::Config(format!("owner query listener: {error}")))?;

    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let stop_thread = stop.clone();
    let running_thread = running.clone();
    let join = thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    in_flight_tools.fetch_add(1, Ordering::SeqCst);
                    drop(stream);
                    in_flight_tools.fetch_sub(1, Ordering::SeqCst);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        running_thread.store(false, Ordering::SeqCst);
    });

    Ok(QueryListener {
        url: format!("http://{addr}/mcp"),
        addr,
        running,
        stop,
        join: Some(join),
    })
}

/// Lease file path for the owner role under a canonical repository root.
pub fn owner_lease_path(repo_root: impl AsRef<Path>) -> PathBuf {
    lease_path(repo_root, LeaseRole::Owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_core::lease::{new_owner_record, write_lease_atomic, DEFAULT_LEASE_TTL};
    use cognis_core::{ParseStatus, Symbol, SymbolKind};
    use cognis_store::{FileCacheRow, IndexPublisher, PublishBatch, PublishReason, QueuedSymbol};
    use std::fs;

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cognis-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn acquire_opens_db_starts_services_and_release_frees_lease() {
        let repo = temp_repo();
        let session = become_owner(&repo).expect("acquire owner");
        let lease = owner_lease_path(Path::new(&session.identity().root));
        match session {
            OwnerSession::Acquired(owned) => {
                assert!(owned.database().is_some(), "acquired owner opens the DB");
                assert!(owned.watcher_running());
                assert_eq!(owned.watcher_queue_depth(), 0);
                assert!(owned.query_listener_running());
                assert!(owned.missing_indexes().is_empty());
                assert!(lease.exists(), "owner.lease must exist while held");
                let status_path = owner_status_path(Path::new(&owned.identity().root));
                assert!(status_path.exists(), "owner-status.json must be written");
                let raw: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&status_path).unwrap()).unwrap();
                for key in [
                    "pid",
                    "active",
                    "phase",
                    "message",
                    "progressPercent",
                    "pendingCount",
                    "pendingFiles",
                    "inflightCount",
                    "inflightFiles",
                    "recentFiles",
                    "updatedAt",
                    "lastError",
                    "sourceGeneration",
                    "semanticGeneration",
                    "pendingVectorCount",
                    "watcherQueueDepth",
                    "modelResident",
                    "fingerprintMatch",
                    "capabilities",
                    "canonicalWorkspace",
                ] {
                    assert!(raw.get(key).is_some(), "owner-status missing {key}");
                }
                assert_eq!(raw["watcherQueueDepth"], 0);
                let endpoint = owned.endpoint();
                assert_eq!(endpoint.pid, std::process::id());
                assert!(endpoint.query_url.is_some());
                owned.release().expect("release lease");
            }
            OwnerSession::Attached(_) => panic!("empty repo must be acquirable"),
        }
        assert!(!lease.exists(), "release must free the lease");
        cleanup(&repo);
    }

    #[test]
    fn attach_does_not_open_the_database() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        let canonical = PathBuf::from(&identity.root);
        fs::create_dir_all(canonical.join(".cognis")).unwrap();
        let mut foreign = new_owner_record(DEFAULT_LEASE_TTL);
        foreign.pid = 1;
        foreign.process_start_id = "foreign-owner".into();
        write_lease_atomic(owner_lease_path(&canonical), &foreign).unwrap();

        let db_path = PathBuf::from(&identity.db_path);
        let session = become_owner(&repo).expect("attach to live owner");
        match session {
            OwnerSession::Attached(attached) => {
                assert_eq!(attached.endpoint().pid, 1);
                assert!(!attached.endpoint().lease_path.as_os_str().is_empty());
                assert!(attached.endpoint().query_url.is_none());
            }
            OwnerSession::Acquired(_) => panic!("live foreign lease must attach"),
        }
        assert!(
            !db_path.exists(),
            "attach must not open (and therefore not create) the workspace database"
        );
        cleanup(&repo);
    }

    #[test]
    fn attach_reads_advertised_query_url_without_opening_db() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        let canonical = PathBuf::from(&identity.root);
        fs::create_dir_all(canonical.join(".cognis")).unwrap();
        let mut foreign = new_owner_record(DEFAULT_LEASE_TTL);
        foreign.pid = 7;
        foreign.process_start_id = "foreign-owner".into();
        write_lease_atomic(owner_lease_path(&canonical), &foreign).unwrap();
        write_mcpd_endpoint(
            &canonical,
            "http://127.0.0.1:50123/mcp",
            Some("0123456789abcdef0123456789abcdef"),
            Some("abc"),
        )
        .unwrap();

        let db_path = PathBuf::from(&identity.db_path);
        let session = become_owner(&repo).expect("attach");
        match session {
            OwnerSession::Attached(attached) => {
                assert_eq!(
                    attached.endpoint().query_url.as_deref(),
                    Some("http://127.0.0.1:50123/mcp")
                );
            }
            OwnerSession::Acquired(_) => panic!("live foreign lease must attach"),
        }
        assert!(!db_path.exists());
        cleanup(&repo);
    }

    #[test]
    fn attach_existing_owner_on_vacant_lease_does_not_open_db() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        let db_path = PathBuf::from(&identity.db_path);
        let attached = attach_existing_owner(&repo).expect("vacant attach");
        assert!(attached.is_none(), "vacant workspace has no live owner");
        assert!(
            !db_path.exists(),
            "vacant attach must not create the workspace database"
        );
        cleanup(&repo);
    }

    #[test]
    fn busybox_heavy_surface_is_owner() {
        assert_eq!(heavy_surface_args("cognis"), vec!["owner"]);
        assert!(heavy_surface_args("cognis-mcpd").is_empty());
    }

    #[test]
    fn path_spellings_collapse_to_one_lease() {
        let repo = temp_repo();
        let with_sep = repo.join("");
        let id_a = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        let id_b = RepoIdentity::from_paths(&with_sep, RepoIdentity::default_db_path(&with_sep));
        assert_eq!(id_a.wire_key(), id_b.wire_key());
        assert_eq!(
            owner_lease_path(Path::new(&id_a.root)),
            owner_lease_path(Path::new(&id_b.root))
        );
        cleanup(&repo);
    }

    #[test]
    fn missing_required_index_skips_the_query_listener() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        fs::create_dir_all(Path::new(&identity.root).join(".cognis")).unwrap();
        let db = Database::open(&identity.db_path).unwrap();
        db.connect()
            .unwrap()
            .execute("DROP INDEX idx_symbol_name_lower", [])
            .unwrap();
        db.close_thread_connection();
        drop(db);

        let session = become_owner(&repo).expect("owner may acquire a degraded workspace");
        match session {
            OwnerSession::Acquired(owned) => {
                assert_eq!(
                    owned.missing_indexes(),
                    &["idx_symbol_name_lower".to_string()]
                );
                assert!(owned.watcher_running());
                assert!(
                    !owned.query_listener_running(),
                    "missing indexes must refuse to serve queries"
                );
                drop(owned);
            }
            OwnerSession::Attached(_) => panic!("degraded workspace is still acquirable"),
        }
        cleanup(&repo);
    }

    fn queued_symbol(id: &str, path: &str) -> (Symbol, FileCacheRow, QueuedSymbol) {
        let symbol = Symbol {
            id: id.into(),
            kind: SymbolKind::Function,
            name: id.into(),
            qualified_name: format!("mod.{id}"),
            language: "rust".into(),
            module: "mod".into(),
            file_path: path.into(),
            line_start: 1,
            line_end: 2,
            signature: None,
            docstring: None,
            content_hash: format!("hash-{id}"),
            body_excerpt: Some("fn x() {}".into()),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        };
        let file = FileCacheRow {
            path: path.into(),
            language: "rust".into(),
            size_bytes: 8,
            content_hash: format!("hash-{id}"),
            parsed_at: 1,
            parse_status: ParseStatus::Ok,
        };
        let queued = QueuedSymbol {
            symbol_id: id.into(),
            file_path: path.into(),
        };
        (symbol, file, queued)
    }

    #[test]
    fn acquire_drains_due_embedding_queue_rows_without_a_user_command() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        fs::create_dir_all(Path::new(&identity.root).join(".cognis")).unwrap();
        let mut db = Database::open(&identity.db_path).unwrap();
        let (symbol, file, queued) = queued_symbol("drain-me", "src/a.rs");
        let mut batch = PublishBatch::empty(PublishReason::Incremental);
        batch.symbol_upserts.push(symbol);
        batch.file_rows.push(file);
        batch.queue_enqueue.push(queued);
        db.publish(&batch).unwrap();
        db.connect()
            .unwrap()
            .execute("UPDATE embedding_queue SET next_attempt_at = 0", [])
            .unwrap();
        db.close_thread_connection();
        drop(db);

        let session = become_owner(&repo).expect("acquire owner");
        match session {
            OwnerSession::Acquired(owned) => {
                let attempts: i64 = owned
                    .database()
                    .expect("db")
                    .connect()
                    .unwrap()
                    .query_row(
                        "SELECT attempts FROM embedding_queue WHERE symbol_id = 'drain-me'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(
                    attempts >= 1,
                    "owner start must resume the embedding queue; attempts={attempts}"
                );
                drop(owned);
            }
            OwnerSession::Attached(_) => panic!("must acquire"),
        }
        cleanup(&repo);
    }

    fn write_stub_embedder_config(repo: &Path) {
        let cognis = repo.join(".cognis");
        fs::create_dir_all(&cognis).unwrap();
        fs::write(
            cognis.join("config.yaml"),
            "embedder:\n  backend: stub\n  model: stub\n  dim: 8\n  batch_size: 4\n",
        )
        .unwrap();
    }

    #[test]
    fn lazy_policy_does_not_load_the_model_at_start() {
        let repo = temp_repo();
        write_stub_embedder_config(&repo);
        let session = become_owner_with_policy(&repo, SemanticWarmPolicy::Lazy).expect("acquire");
        match session {
            OwnerSession::Acquired(owned) => {
                assert!(
                    !owned.model_resident(),
                    "lazy policy must not load before demand"
                );
                let raw: serde_json::Value = serde_json::from_str(
                    &fs::read_to_string(owner_status_path(Path::new(&owned.identity().root)))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(raw["modelResident"], false);
                drop(owned);
            }
            OwnerSession::Attached(_) => panic!("must acquire"),
        }
        cleanup(&repo);
    }

    #[test]
    fn eager_stub_warms_the_model_and_idle_evict_releases_when_clear() {
        let repo = temp_repo();
        write_stub_embedder_config(&repo);
        let session = become_owner_with_policy(&repo, SemanticWarmPolicy::Eager).expect("acquire");
        match session {
            OwnerSession::Acquired(mut owned) => {
                assert!(owned.model_resident(), "eager stub must be resident");
                owned.note_tool_call_start();
                owned.set_idle_evict_after(Duration::ZERO);
                assert!(
                    matches!(
                        owned.try_idle_evict_model(),
                        EvictOutcome::InFlight { count } if count >= 1
                    ),
                    "must refuse while a tool call is in flight"
                );
                owned.note_tool_call_end();
                assert_eq!(owned.try_idle_evict_model(), EvictOutcome::Evicted);
                assert!(!owned.model_resident());
                let raw: serde_json::Value = serde_json::from_str(
                    &fs::read_to_string(owner_status_path(Path::new(&owned.identity().root)))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(raw["modelResident"], false);
                drop(owned);
            }
            OwnerSession::Attached(_) => panic!("must acquire"),
        }
        cleanup(&repo);
    }

    #[test]
    fn idle_evict_refuses_while_embedding_queue_is_non_empty() {
        let repo = temp_repo();
        let identity = RepoIdentity::from_paths(&repo, RepoIdentity::default_db_path(&repo));
        fs::create_dir_all(Path::new(&identity.root).join(".cognis")).unwrap();
        let mut db = Database::open(&identity.db_path).unwrap();
        let (symbol, file, queued) = queued_symbol("still-pending", "src/a.rs");
        let mut batch = PublishBatch::empty(PublishReason::Incremental);
        batch.symbol_upserts.push(symbol);
        batch.file_rows.push(file);
        batch.queue_enqueue.push(queued);
        db.publish(&batch).unwrap();
        db.connect()
            .unwrap()
            .execute("UPDATE embedding_queue SET next_attempt_at = 0", [])
            .unwrap();
        db.close_thread_connection();
        drop(db);

        let session = become_owner_with_policy(&repo, SemanticWarmPolicy::Lazy).expect("acquire");
        match session {
            OwnerSession::Acquired(mut owned) => {
                owned.set_idle_evict_after(Duration::ZERO);
                assert!(
                    matches!(owned.try_idle_evict_model(), EvictOutcome::InFlight { .. }),
                    "must refuse while the embedding queue is non-empty"
                );
                drop(owned);
            }
            OwnerSession::Attached(_) => panic!("must acquire"),
        }
        cleanup(&repo);
    }
}
