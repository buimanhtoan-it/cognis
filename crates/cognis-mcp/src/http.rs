//! HTTP transport for the MCP server (Requirement 3, `mcp.http`).
//!
//! `cognis-mcpd --transport http --host <h> --port <p>` serves the same
//! read-only 8-tool JSON-RPC surface as the stdio transport, but over
//! HTTP/1.1 on a localhost port an editor connects to (the panel-managed
//! "Start MCP server" flow, `apps/cognis-vscode/src/mcpServer.ts`). The stdio
//! transport remains the default and the one the editor spawns/owns; this is
//! the standalone-server option.
//!
//! The endpoint is intentionally small and dependency-free (std `TcpListener`,
//! no async runtime): a client POSTs a JSON-RPC request (or a batch array) to
//! `/mcp` and receives the JSON-RPC response as `application/json`. It reuses
//! [`McpServer::handle`] for dispatch, so the wire contract (tool set, output
//! shapes, error envelope) is identical to stdio by construction — there is no
//! second implementation of the protocol to drift.
//!
//! Framing notes:
//! * One request/response per connection (`Connection: close`); MCP HTTP
//!   clients open a fresh POST per call, so this is sufficient and avoids
//!   keep-alive edge cases.
//! * Connections are accepted on the listener thread and handed to a
//!   **bounded worker pool** (explicit worker count + queue capacity +
//!   request timeouts). Concurrent POSTs overlap instead of serializing
//!   head-of-line on a single thread (Requirement 2.8). When the queue is
//!   full the acceptor answers immediately with a retryable `503` overload
//!   response rather than unbounded buffering.
//! * `GET /mcp` (the streamable-http SSE upgrade) is answered `405` with an
//!   `Allow: POST` header — this server does not push server-initiated events;
//!   request/response tool calls (the traffic that matters) work.
//!
//! Isolation (Requirement 2.12 / Tasks 8.1 + 8.2):
//! * Bind is **loopback-only by default**. Non-loopback hosts (`0.0.0.0`, LAN
//!   IPs, public interfaces) are rejected unless the operator explicitly opts
//!   in via [`BindOptions::allow_non_loopback`] or `COGNIS_MCP_ALLOW_REMOTE=1`.
//! * Every shared HTTP route requires an **unguessable scoped credential**
//!   ([`RouteCredential`]), presented as `Authorization: Bearer <token>` or
//!   `X-Cognis-Route-Token: <token>`. Missing/wrong credentials yield `401`.
//! * Every attachment presents repository identity (`X-Cognis-Repo-Key`) and a
//!   model fingerprint (`X-Cognis-Model-Fingerprint`); cross-repository access
//!   and fingerprint mismatch refuse the session (Task 8.2).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use cognis_core::{verify_repo_wire_key, AttachmentDecision, RepoIdentity, REPO_IDENTITY_HEADER};
use cognis_embed::{session_reuse_allowed, ModelFingerprint, MODEL_FINGERPRINT_HEADER};

use crate::caps::DEFAULT_MAX_CONCURRENCY;
use crate::engine::RetrievalEngine;
use crate::jsonrpc::{Request, Response, RpcError, INVALID_REQUEST};
use crate::server::McpServer;

/// Env var that opts into binding non-loopback interfaces (explicit remote
/// exposure). Absent or any value other than `1` keeps the loopback-only
/// default (Requirement 2.12; CHANGELOG documents this flag).
pub const ALLOW_REMOTE_ENV: &str = "COGNIS_MCP_ALLOW_REMOTE";

/// Env var carrying the unguessable scoped HTTP route credential. Set by the
/// heavy daemon (or the thin-proxy spawner) and presented by clients as
/// `Authorization: Bearer …` / `X-Cognis-Route-Token`.
pub const ROUTE_CREDENTIAL_ENV: &str = "COGNIS_MCP_ROUTE_TOKEN";

/// Header name for the scoped route credential (alternative to Bearer).
pub const ROUTE_CREDENTIAL_HEADER: &str = "X-Cognis-Route-Token";

/// Minimum accepted length for a caller-supplied route credential (bytes).
const MIN_CREDENTIAL_LEN: usize = 16;

/// Cap on the request body we will read, so a bogus `Content-Length` can't make
/// the server allocate unbounded memory.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Default worker-pool size for the HTTP transport. Aligned with the tool-call
/// concurrency cap so the accept pool does not admit more concurrent work than
/// the server's in-process limiter already budgets for.
pub const DEFAULT_HTTP_WORKERS: usize = DEFAULT_MAX_CONCURRENCY;

/// Default bounded queue depth in front of the worker pool. Slightly larger
/// than the worker count so short bursts can wait without immediate overload,
/// while still applying backpressure under sustained saturation.
pub const DEFAULT_HTTP_QUEUE_CAPACITY: usize = DEFAULT_HTTP_WORKERS * 2;

/// Default per-connection read/write timeout. Covers framing + tool execution
/// and prevents a stalled client from holding a worker forever.
pub const DEFAULT_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default `Retry-After` (seconds) advertised on overload responses.
pub const DEFAULT_HTTP_OVERLOAD_RETRY_AFTER_SECS: u64 = 1;

// ---------------------------------------------------------------------------
// Loopback bind policy (Requirement 2.12)
// ---------------------------------------------------------------------------

/// Options controlling whether a non-loopback bind is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BindOptions {
    /// When `true`, allow binding non-loopback interfaces (explicit opt-in).
    /// Defaults to `false`; also set by `COGNIS_MCP_ALLOW_REMOTE=1`.
    pub allow_non_loopback: bool,
}

impl BindOptions {
    /// Resolve from the process environment: `COGNIS_MCP_ALLOW_REMOTE=1` opts
    /// into non-loopback binds; any other value (or absence) keeps the default.
    pub fn from_env() -> Self {
        BindOptions {
            allow_non_loopback: std::env::var(ALLOW_REMOTE_ENV).as_deref() == Ok("1"),
        }
    }
}

/// True when `host` resolves exclusively to loopback addresses (or is a known
/// loopback name such as `localhost` / `127.0.0.1` / `::1`).
///
/// Wildcards (`0.0.0.0`, `::`) and any non-loopback IP are **not** loopback.
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Fast-path common loopback names without DNS.
    if trimmed.eq_ignore_ascii_case("localhost")
        || trimmed == "127.0.0.1"
        || trimmed == "::1"
        || trimmed.eq_ignore_ascii_case("ip6-localhost")
    {
        return true;
    }
    // Literal IP: reject wildcards and non-loopback.
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    // Resolve hostnames; every resolved address must be loopback.
    match (trimmed, 0u16).to_socket_addrs() {
        Ok(addrs) => {
            let mut any = false;
            for addr in addrs {
                any = true;
                if !addr.ip().is_loopback() {
                    return false;
                }
            }
            any
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Scoped route credential (Requirement 2.12)
// ---------------------------------------------------------------------------

/// Unguessable credential that authorizes a single HTTP MCP route.
///
/// Generated per heavy-daemon start (or loaded from `COGNIS_MCP_ROUTE_TOKEN`)
/// and required on every POST. Constant-time compared so timing side channels
/// do not leak the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCredential {
    token: String,
}

impl RouteCredential {
    /// Mint a fresh unguessable credential (≥ 128 bits of mixed entropy).
    pub fn generate() -> Self {
        // Mix process id, wall-clock nanos, a stack address, and a SHA-256 of
        // the mix so concurrent generators almost never collide and the output
        // is not a short guessable string — without a RNG dependency.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mix_ptr = std::ptr::addr_of!(nanos) as usize;
        let pid = std::process::id();
        let mut hasher = Sha256::new();
        hasher.update(pid.to_le_bytes());
        hasher.update(nanos.to_le_bytes());
        hasher.update(mix_ptr.to_le_bytes());
        // A second time sample after hashing the first reduces correlation
        // between successive calls in the same process.
        let nanos2 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        hasher.update(nanos2.to_le_bytes());
        let digest = hasher.finalize();
        // 32-byte digest → 64 hex chars (256 bits of mixed material).
        let token = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        RouteCredential { token }
    }

    /// Load from an explicit token string. Rejects empty/short values so a
    /// misconfigured env var cannot silently disable authentication.
    pub fn from_token(token: impl Into<String>) -> std::io::Result<Self> {
        let token = token.into();
        let trimmed = token.trim();
        if trimmed.len() < MIN_CREDENTIAL_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "route credential must be at least {MIN_CREDENTIAL_LEN} characters \
                     (got {}); generate one with RouteCredential::generate() or set \
                     {ROUTE_CREDENTIAL_ENV}",
                    trimmed.len()
                ),
            ));
        }
        Ok(RouteCredential {
            token: trimmed.to_string(),
        })
    }

    /// Resolve from `COGNIS_MCP_ROUTE_TOKEN`, or mint a fresh credential when
    /// the env var is absent/empty. Invalid (too-short) env values error so the
    /// operator notices instead of falling back to an unauthenticated surface.
    pub fn from_env_or_generate() -> std::io::Result<Self> {
        match std::env::var(ROUTE_CREDENTIAL_ENV) {
            Ok(v) if !v.trim().is_empty() => Self::from_token(v),
            _ => Ok(Self::generate()),
        }
    }

    /// The secret token string (present as a Bearer token / header value).
    pub fn as_str(&self) -> &str {
        &self.token
    }

    /// Constant-time equality against a presented candidate.
    pub fn matches(&self, presented: &str) -> bool {
        constant_time_eq(self.token.as_bytes(), presented.as_bytes())
    }
}

/// Constant-time byte equality (length mismatch still walks both sides).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Always iterate over the longer length so short candidates don't short-
    // circuit early. Mismatched lengths force a final `false`.
    let len = a.len().max(b.len());
    let mut diff: u8 = if a.len() == b.len() { 0 } else { 1 };
    for i in 0..len {
        let ai = *a.get(i).unwrap_or(&0);
        let bi = *b.get(i).unwrap_or(&0);
        diff |= ai ^ bi;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Serve configuration
// ---------------------------------------------------------------------------

/// Explicit limits for the bounded-concurrent HTTP transport (Requirement 2.8)
/// plus the isolation credential and attachment identity required on every
/// route (Requirement 2.12 / Tasks 8.1 + 8.2).
///
/// Shared HTTP is only safe once concurrency is bounded with worker/queue/time
/// limits, backpressure, and retryable overload responses **and** each route
/// is authenticated with an unguessable scoped credential, repository identity,
/// and model fingerprint. Defaults match the tool-call concurrency budget;
/// callers (and later the sharing gate) can tighten or loosen them without
/// changing the wire contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpServeConfig {
    /// Number of worker threads that handle accepted connections concurrently.
    pub worker_count: usize,
    /// Bounded queue capacity between the acceptor and the workers. When full,
    /// new connections receive an immediate retryable overload response.
    pub queue_capacity: usize,
    /// Per-connection read and write timeout applied before handling.
    pub request_timeout: Duration,
    /// `Retry-After` value (seconds) on `503` overload responses.
    pub overload_retry_after_secs: u64,
    /// Required scoped credential for every POST. `None` is only valid before
    /// [`HttpServeConfig::normalized`] runs; normalization mints one so the
    /// serve loop always authenticates.
    pub route_credential: Option<RouteCredential>,
    /// Owner repository identity. When set, every attachment must present a
    /// matching `X-Cognis-Repo-Key` (cross-repository access is rejected).
    /// `None` disables the check (legacy/fixture/tests that do not share).
    pub repo_identity: Option<RepoIdentity>,
    /// Owner model fingerprint. When set, every attachment must present a
    /// matching `X-Cognis-Model-Fingerprint` (session reuse refused on mismatch).
    /// `None` disables the check (legacy/fixture/tests that do not share).
    pub model_fingerprint: Option<ModelFingerprint>,
}

impl Default for HttpServeConfig {
    fn default() -> Self {
        HttpServeConfig {
            worker_count: DEFAULT_HTTP_WORKERS,
            queue_capacity: DEFAULT_HTTP_QUEUE_CAPACITY,
            request_timeout: DEFAULT_HTTP_REQUEST_TIMEOUT,
            overload_retry_after_secs: DEFAULT_HTTP_OVERLOAD_RETRY_AFTER_SECS,
            // Mint at construction so `Default` already carries a secret; tests
            // that need a known token override this field.
            route_credential: Some(RouteCredential::generate()),
            repo_identity: None,
            model_fingerprint: None,
        }
    }
}

impl HttpServeConfig {
    /// Build a config that requires the given credential (used by the daemon
    /// entry so the advertised token matches the serve loop).
    pub fn with_credential(credential: RouteCredential) -> Self {
        HttpServeConfig {
            route_credential: Some(credential),
            ..HttpServeConfig::default_limits()
        }
    }

    /// Attach the owner repository identity enforced on every connection.
    pub fn with_repo_identity(mut self, identity: RepoIdentity) -> Self {
        self.repo_identity = Some(identity);
        self
    }

    /// Attach the owner model fingerprint enforced on every connection.
    pub fn with_model_fingerprint(mut self, fingerprint: ModelFingerprint) -> Self {
        self.model_fingerprint = Some(fingerprint);
        self
    }

    /// Limits-only default (no credential yet). Prefer [`Default`] or
    /// [`with_credential`] in production paths.
    fn default_limits() -> Self {
        HttpServeConfig {
            worker_count: DEFAULT_HTTP_WORKERS,
            queue_capacity: DEFAULT_HTTP_QUEUE_CAPACITY,
            request_timeout: DEFAULT_HTTP_REQUEST_TIMEOUT,
            overload_retry_after_secs: DEFAULT_HTTP_OVERLOAD_RETRY_AFTER_SECS,
            route_credential: None,
            repo_identity: None,
            model_fingerprint: None,
        }
    }

    /// Clamp zero/empty limits to safe minima and ensure a route credential is
    /// always present so a misconfigured caller cannot disable auth.
    fn normalized(self) -> Self {
        HttpServeConfig {
            worker_count: self.worker_count.max(1),
            queue_capacity: self.queue_capacity.max(1),
            request_timeout: if self.request_timeout.is_zero() {
                DEFAULT_HTTP_REQUEST_TIMEOUT
            } else {
                self.request_timeout
            },
            overload_retry_after_secs: self.overload_retry_after_secs.max(1),
            route_credential: Some(
                self.route_credential
                    .unwrap_or_else(RouteCredential::generate),
            ),
            repo_identity: self.repo_identity,
            model_fingerprint: self.model_fingerprint,
        }
    }

    /// The credential the serve loop will require (after normalization).
    pub fn credential(&self) -> Option<&RouteCredential> {
        self.route_credential.as_ref()
    }
}

/// Bind `host:port` and serve the MCP JSON-RPC surface over HTTP until the
/// listener errors unrecoverably. Blocks the calling thread (the daemon's main
/// loop). Per-connection failures are logged-and-skipped, never fatal.
///
/// Uses the default loopback-only bind policy and a freshly generated route
/// credential. Prefer [`serve_http_with`] when the caller needs to advertise
/// the credential to clients (thin proxy / endpoint file).
pub fn serve_http<E: RetrievalEngine + Send + Sync>(
    server: &McpServer<E>,
    host: &str,
    port: u16,
) -> std::io::Result<()> {
    let config = HttpServeConfig::default();
    let listener = bind(host, port)?;
    serve_listener_with(server, listener, config)
}

/// Bind + serve with an explicit [`HttpServeConfig`] (credential + pool limits)
/// and the default loopback-only bind policy.
pub fn serve_http_with<E: RetrievalEngine + Send + Sync>(
    server: &McpServer<E>,
    host: &str,
    port: u16,
    config: HttpServeConfig,
) -> std::io::Result<()> {
    let listener = bind(host, port)?;
    serve_listener_with(server, listener, config)
}

/// Serve the MCP JSON-RPC surface on an already-bound [`TcpListener`] with the
/// default bounded-concurrency configuration (and a generated credential).
///
/// Split from [`serve_http`] so a caller can bind first (reporting a port-in-use
/// error) and only then announce readiness before entering the serve loop.
pub fn serve_listener<E: RetrievalEngine + Send + Sync>(
    server: &McpServer<E>,
    listener: TcpListener,
) -> std::io::Result<()> {
    serve_listener_with(server, listener, HttpServeConfig::default())
}

/// Serve the MCP JSON-RPC surface with an explicit [`HttpServeConfig`].
///
/// Architecture (Requirement 2.8):
/// * **Acceptor** — the calling thread accepts TCP connections and
///   `try_send`s them into a bounded queue (backpressure).
/// * **Worker pool** — `worker_count` threads pull connections and handle them
///   concurrently, so a slow request cannot head-of-line-block the next.
/// * **Overload** — when the queue is full the acceptor answers immediately
///   with a retryable `503 Service Unavailable` + `Retry-After` and closes the
///   connection (`Connection: close` is preserved on every response).
/// * **Timeouts** — each accepted stream gets read/write deadlines before a
///   worker begins framing so a stalled peer cannot pin a worker forever.
/// * **Clean shutdown** — when the accept loop ends, the queue sender is
///   dropped and workers exit; scoped threads join before this function
///   returns.
///
/// Isolation (Requirement 2.12):
/// * Every POST must present the config's [`RouteCredential`] (Bearer or
///   `X-Cognis-Route-Token`); missing/wrong credentials yield `401`.
/// * When `repo_identity` / `model_fingerprint` are set, every attachment must
///   present matching `X-Cognis-Repo-Key` / `X-Cognis-Model-Fingerprint` headers
///   or the connection is refused (`403`).
pub fn serve_listener_with<E: RetrievalEngine + Send + Sync>(
    server: &McpServer<E>,
    listener: TcpListener,
    config: HttpServeConfig,
) -> std::io::Result<()> {
    let config = config.normalized();
    let credential = config
        .route_credential
        .clone()
        .expect("normalized config always has a route credential");
    let repo_identity = config.repo_identity.clone();
    let model_fingerprint = config.model_fingerprint.clone();
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(config.queue_capacity);
    // std mpsc is single-consumer; share the receiver under a mutex so the
    // worker pool can pull jobs. Handling always happens *outside* the lock so
    // concurrent requests still overlap.
    let rx = Mutex::new(rx);

    std::thread::scope(|scope| {
        for _ in 0..config.worker_count {
            let rx = &rx;
            let credential = &credential;
            let repo_identity = &repo_identity;
            let model_fingerprint = &model_fingerprint;
            scope.spawn(move || {
                worker_loop(
                    server,
                    rx,
                    config.request_timeout,
                    credential,
                    repo_identity,
                    model_fingerprint,
                );
            });
        }

        // Accept loop — lives on this thread for the lifetime of the daemon.
        // Dropping `tx` at the end of this block wakes workers so they exit
        // cleanly when the listener stops producing connections.
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => match tx.try_send(stream) {
                    Ok(()) => {}
                    Err(TrySendError::Full(stream)) => {
                        // Backpressure: refuse to buffer unboundedly. The
                        // client gets a retryable overload response immediately.
                        // Apply write deadlines so a stuck peer cannot pin the
                        // acceptor thread either.
                        apply_stream_timeouts(&stream, config.request_timeout);
                        let _ = write_overload_response(stream, config.overload_retry_after_secs);
                    }
                    // Workers gone — answer one last overload-style close and stop.
                    Err(TrySendError::Disconnected(stream)) => {
                        apply_stream_timeouts(&stream, config.request_timeout);
                        let _ = write_overload_response(stream, config.overload_retry_after_secs);
                        break;
                    }
                },
                // Transient accept errors must not take the server down.
                Err(_) => continue,
            }
        }
        drop(tx);
    });

    Ok(())
}

/// Worker body: pull accepted streams from the shared queue and handle them
/// until the acceptor drops the sender (clean shutdown).
fn worker_loop<E: RetrievalEngine>(
    server: &McpServer<E>,
    rx: &Mutex<mpsc::Receiver<TcpStream>>,
    request_timeout: Duration,
    credential: &RouteCredential,
    repo_identity: &Option<RepoIdentity>,
    model_fingerprint: &Option<ModelFingerprint>,
) {
    // Bounded wait so a worker can observe disconnect promptly even if no new
    // work arrives; on timeout we re-check the channel.
    let idle_poll = Duration::from_millis(250);
    loop {
        let stream = {
            let guard = match rx.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            match guard.recv_timeout(idle_poll) {
                Ok(stream) => stream,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        apply_stream_timeouts(&stream, request_timeout);
        let _ = handle_connection(
            server,
            stream,
            credential,
            repo_identity.as_ref(),
            model_fingerprint.as_ref(),
        );
    }
}

/// Apply read/write deadlines so a stalled peer cannot pin a worker forever.
fn apply_stream_timeouts(stream: &TcpStream, timeout: Duration) {
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
}

/// Bind a `TcpListener` on `host:port` under the **default loopback-only**
/// policy (Requirement 2.12). Non-loopback hosts are rejected unless
/// `COGNIS_MCP_ALLOW_REMOTE=1` is set in the environment.
///
/// Split out so [`serve_http`] can report a bind failure (port in use / policy
/// rejection) distinctly from serve-loop errors.
pub fn bind(host: &str, port: u16) -> std::io::Result<TcpListener> {
    bind_with(host, port, BindOptions::from_env())
}

/// Bind a `TcpListener` with explicit [`BindOptions`].
///
/// Rejects non-loopback hosts unless `options.allow_non_loopback` is true. This
/// is the policy gate the exploration test
/// `non_loopback_bind_is_rejected_by_default` encodes.
pub fn bind_with(host: &str, port: u16, options: BindOptions) -> std::io::Result<TcpListener> {
    if !is_loopback_host(host) && !options.allow_non_loopback {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to bind non-loopback host '{host}': MCP HTTP binds \
                 loopback-only by default (Requirement 2.12). Set \
                 {ALLOW_REMOTE_ENV}=1 or pass BindOptions {{ allow_non_loopback: true }} \
                 to opt in explicitly."
            ),
        ));
    }
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no socket address")
    })?;
    // Defense in depth: even if the host string looked loopback-ish, refuse a
    // resolved non-loopback address unless explicitly opted in.
    if !addr.ip().is_loopback() && !options.allow_non_loopback {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to bind non-loopback address {addr}: MCP HTTP binds \
                 loopback-only by default (Requirement 2.12). Set \
                 {ALLOW_REMOTE_ENV}=1 to opt in explicitly."
            ),
        ));
    }
    TcpListener::bind(addr)
}

/// Handle one HTTP request on `stream`: parse the request line + headers, check
/// the scoped route credential + repository/model isolation, read the body for
/// a POST, dispatch it, and write the response.
fn handle_connection<E: RetrievalEngine>(
    server: &McpServer<E>,
    stream: TcpStream,
    credential: &RouteCredential,
    owner_repo: Option<&RepoIdentity>,
    owner_fingerprint: Option<&ModelFingerprint>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // Request line: "<METHOD> <PATH> HTTP/1.1".
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(()); // client closed before sending anything
    }
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    // Headers until a blank line; we need Content-Length + isolation headers.
    let mut content_length = 0usize;
    let mut presented_credential: Option<String> = None;
    let mut presented_repo_key: Option<String> = None;
    let mut presented_fingerprint: Option<String> = None;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse::<usize>().unwrap_or(0).min(MAX_BODY_BYTES);
            } else if name.eq_ignore_ascii_case("authorization") {
                // Accept `Bearer <token>` (case-insensitive scheme).
                if let Some(token) = value
                    .strip_prefix("Bearer ")
                    .or_else(|| value.strip_prefix("bearer "))
                {
                    presented_credential = Some(token.trim().to_string());
                }
            } else if name.eq_ignore_ascii_case(ROUTE_CREDENTIAL_HEADER) {
                presented_credential = Some(value.to_string());
            } else if name.eq_ignore_ascii_case(REPO_IDENTITY_HEADER) {
                presented_repo_key = Some(value.to_string());
            } else if name.eq_ignore_ascii_case(MODEL_FINGERPRINT_HEADER) {
                presented_fingerprint = Some(value.to_string());
            }
        }
    }

    // Credential check before we spend any engine time (Requirement 2.12).
    // Apply to every method that reaches this far — a missing token on GET
    // still yields 401 so scanners learn the surface is authenticated; the
    // 405 path only runs after a valid credential so the Allow header is not
    // free reconnaissance.
    let authorized = presented_credential
        .as_deref()
        .map(|t| credential.matches(t))
        .unwrap_or(false);
    if !authorized {
        return write_http(
            &mut writer,
            "401 Unauthorized",
            "application/json",
            br#"{"error":{"code":"UNAUTHORIZED","message":"missing or invalid MCP route credential; present Authorization: Bearer <token> or X-Cognis-Route-Token","retryable":false}}"#,
            &[
                ("WWW-Authenticate", "Bearer"),
                ("X-Cognis-Auth", "route-credential-required"),
            ],
        );
    }

    // Repository-identity verification on every attachment (Task 8.2).
    // When the owner configured an identity, the client must present a matching
    // wire key — cross-repository access is rejected with 403.
    if let Some(owner) = owner_repo {
        let presented = presented_repo_key.as_deref().unwrap_or("");
        match verify_repo_wire_key(owner, presented) {
            AttachmentDecision::Allow => {}
            AttachmentDecision::RejectCrossRepository { .. } => {
                return write_http(
                    &mut writer,
                    "403 Forbidden",
                    "application/json",
                    br#"{"error":{"code":"CROSS_REPOSITORY","message":"repository identity mismatch; cross-repository access is rejected (present matching X-Cognis-Repo-Key)","retryable":false}}"#,
                    &[("X-Cognis-Isolation", "repo-identity-rejected")],
                );
            }
        }
    }

    // Model-fingerprint verification: refuse session reuse across differing
    // fingerprints (Task 8.2 / Property 12).
    if let Some(owner_fp) = owner_fingerprint {
        let presented = presented_fingerprint
            .as_deref()
            .map(ModelFingerprint::from_digest)
            .unwrap_or_else(|| ModelFingerprint::from_digest(""));
        if !session_reuse_allowed(owner_fp, &presented) {
            return write_http(
                &mut writer,
                "403 Forbidden",
                "application/json",
                br#"{"error":{"code":"MODEL_FINGERPRINT_MISMATCH","message":"model fingerprint mismatch; session reuse refused (present matching X-Cognis-Model-Fingerprint)","retryable":false}}"#,
                &[("X-Cognis-Isolation", "model-fingerprint-rejected")],
            );
        }
    }

    if !method.eq_ignore_ascii_case("POST") {
        // Only POST carries JSON-RPC. GET (SSE upgrade) and others are declined.
        return write_http(
            &mut writer,
            "405 Method Not Allowed",
            "text/plain",
            b"MCP over HTTP: POST JSON-RPC to /mcp",
            &[("Allow", "POST")],
        );
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body_text = String::from_utf8_lossy(&body);

    match dispatch(server, &body_text) {
        Some(json) => write_http(
            &mut writer,
            "200 OK",
            "application/json",
            json.as_bytes(),
            &[],
        ),
        // A body of only notifications produces no response payload — 202.
        None => write_http(&mut writer, "202 Accepted", "application/json", b"", &[]),
    }
}

/// Dispatch a JSON-RPC request body (single object or a batch array) through the
/// server. Returns the serialized JSON response (object for a single request,
/// array for a batch), or `None` when there is nothing to send (all
/// notifications / empty batch).
fn dispatch<E: RetrievalEngine>(server: &McpServer<E>, body: &str) -> Option<String> {
    let value: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            // Not JSON at all: a single parse-error response with a null id.
            let resp = Response::error(
                Value::Null,
                RpcError::new(-32700, format!("parse error: {e}")),
            );
            return Some(serde_json::to_string(&resp).unwrap_or_default());
        }
    };

    match value {
        Value::Array(items) => {
            let mut responses: Vec<Response> = Vec::new();
            for item in items {
                if let Some(resp) = dispatch_one(server, item) {
                    responses.push(resp);
                }
            }
            if responses.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&responses).unwrap_or_default())
            }
        }
        other => dispatch_one(server, other).map(|r| serde_json::to_string(&r).unwrap_or_default()),
    }
}

/// Dispatch a single JSON value as a JSON-RPC request. `None` for a
/// notification (no id ⇒ no response).
fn dispatch_one<E: RetrievalEngine>(server: &McpServer<E>, value: Value) -> Option<Response> {
    match serde_json::from_value::<Request>(value) {
        Ok(req) => server.handle(req),
        Err(e) => Some(Response::error(
            Value::Null,
            RpcError::new(INVALID_REQUEST, format!("invalid Request object: {e}")),
        )),
    }
}

/// Write a minimal HTTP/1.1 response with `Connection: close`.
fn write_http(
    writer: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    // One write prevents a Windows client from observing a status-only
    // response if the peer closes between separate header and body writes.
    let mut response = Vec::with_capacity(head.len() + body.len());
    response.extend_from_slice(head.as_bytes());
    response.extend_from_slice(body);
    writer.write_all(&response)?;
    writer.flush()
}

/// Retryable overload response used when the bounded queue is full
/// (Requirement 2.8 backpressure). Uses the same stable error envelope shape
/// the tool surface already exposes so clients can treat it as retryable.
fn write_overload_response(mut stream: TcpStream, retry_after_secs: u64) -> std::io::Result<()> {
    // Keep the body small and contract-shaped: `{error:{code,message,retryable}}`.
    let body = r#"{"error":{"code":"TIMEOUT","message":"MCP HTTP server is at capacity (bounded queue full); retry shortly.","retryable":true}}"#;
    let retry_after = retry_after_secs.to_string();
    write_http(
        &mut stream,
        "503 Service Unavailable",
        "application/json",
        body.as_bytes(),
        &[("Retry-After", retry_after.as_str())],
    )?;
    // The acceptor has not consumed the request body. A plain drop can send
    // RST on Windows and truncate the just-written response, so half-close the
    // write side and briefly drain already-buffered client bytes first.
    stream.shutdown(Shutdown::Write)?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut scratch = [0_u8; 1024];
    loop {
        match stream.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServer;
    use cognis_core::{Hit, Result, Symbol};
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Engine that sleeps so concurrency is observable in wall-clock time.
    struct SlowEngine {
        delay: Duration,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl RetrievalEngine for SlowEngine {
        fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(cur, Ordering::SeqCst);
            thread::sleep(self.delay);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn semantic_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
        fn diffuse(
            &self,
            _seeds: &[Vec<Hit>],
            _k: usize,
            _alpha: f64,
            _eps: f64,
        ) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
        fn hydrate(&self, _ids: &[String]) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }
        fn lookup(&self, _name_or_id: &str, _kind: Option<&str>) -> Result<Option<Symbol>> {
            Ok(None)
        }
        fn dependency_trace(
            &self,
            _symbol_id: &str,
            _direction: &str,
            _depth: u8,
        ) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
    }

    /// Engine that blocks inside `fts_search` until the gate is opened. Used to
    /// deterministically occupy workers without relying on wall-clock races.
    struct GatedEngine {
        /// Pair of (released, condvar). Search waits while `released` is false.
        gate: Arc<(Mutex<bool>, Condvar)>,
        /// Number of calls currently blocked inside the gate.
        blocked: Arc<AtomicUsize>,
    }

    impl RetrievalEngine for GatedEngine {
        fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
            self.blocked.fetch_add(1, Ordering::SeqCst);
            let (lock, cvar) = &*self.gate;
            let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
            while !*released {
                released = cvar.wait(released).unwrap_or_else(|p| p.into_inner());
            }
            self.blocked.fetch_sub(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn semantic_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
        fn diffuse(
            &self,
            _seeds: &[Vec<Hit>],
            _k: usize,
            _alpha: f64,
            _eps: f64,
        ) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
        fn hydrate(&self, _ids: &[String]) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }
        fn lookup(&self, _name_or_id: &str, _kind: Option<&str>) -> Result<Option<Symbol>> {
            Ok(None)
        }
        fn dependency_trace(
            &self,
            _symbol_id: &str,
            _direction: &str,
            _depth: u8,
        ) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }
    }

    fn post_search(port: u16, id: u32, token: &str) -> (u16, String) {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"symbol_search","arguments":{{"query":"x","k":1}}}}}}"#
        );
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Authorization: Bearer {token}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        // Bound the client side so a hung server cannot stall the test forever.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        stream.write_all(request.as_bytes()).expect("write");
        stream.flush().ok();
        let mut buf = Vec::new();
        // Tolerate ConnectionReset and empty partial reads. An overloaded
        // Windows socket can be reset before the test client receives bytes;
        // production clients retry, and this test only asserts complete 503s.
        let _ = stream.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).into_owned();
        let status = text
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, text)
    }

    fn test_config(credential: RouteCredential) -> HttpServeConfig {
        HttpServeConfig {
            worker_count: 4,
            queue_capacity: 8,
            request_timeout: Duration::from_secs(5),
            overload_retry_after_secs: 1,
            route_credential: Some(credential),
            repo_identity: None,
            model_fingerprint: None,
        }
    }

    #[test]
    fn config_normalized_rejects_zero_limits() {
        let cfg = HttpServeConfig {
            worker_count: 0,
            queue_capacity: 0,
            request_timeout: Duration::from_secs(0),
            overload_retry_after_secs: 0,
            route_credential: None,
            repo_identity: None,
            model_fingerprint: None,
        }
        .normalized();
        assert_eq!(cfg.worker_count, 1);
        assert_eq!(cfg.queue_capacity, 1);
        assert_eq!(cfg.request_timeout, DEFAULT_HTTP_REQUEST_TIMEOUT);
        assert_eq!(cfg.overload_retry_after_secs, 1);
        assert!(
            cfg.route_credential.is_some(),
            "normalization must mint a route credential"
        );
    }

    #[test]
    fn loopback_hosts_are_accepted() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn non_loopback_hosts_are_rejected() {
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("::"));
        assert!(!is_loopback_host("8.8.8.8"));
        assert!(!is_loopback_host("192.168.1.1"));
    }

    #[test]
    fn bind_rejects_non_loopback_by_default() {
        let err = bind_with("0.0.0.0", 0, BindOptions::default()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        // Public bind() uses env; with ALLOW_REMOTE unset it must also reject.
        // (Do not assert on bind() directly if a parent test set the env.)
    }

    #[test]
    fn bind_allows_non_loopback_when_opted_in() {
        let listener = bind_with(
            "0.0.0.0",
            0,
            BindOptions {
                allow_non_loopback: true,
            },
        )
        .expect("opt-in non-loopback bind");
        let addr = listener.local_addr().unwrap();
        assert!(!addr.ip().is_loopback() || addr.ip().is_unspecified());
    }

    #[test]
    fn route_credential_rejects_short_tokens() {
        assert!(RouteCredential::from_token("short").is_err());
        assert!(RouteCredential::from_token("0123456789abcdef").is_ok());
    }

    #[test]
    fn route_credential_matches_constant_time() {
        let cred = RouteCredential::from_token("0123456789abcdef0123456789abcdef").unwrap();
        assert!(cred.matches("0123456789abcdef0123456789abcdef"));
        assert!(!cred.matches("0123456789abcdef0123456789abcdee"));
        assert!(!cred.matches(""));
    }

    #[test]
    fn concurrent_requests_overlap_under_worker_pool() {
        let delay = Duration::from_millis(400);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let cred = RouteCredential::generate();
        let token = cred.as_str().to_string();
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = McpServer::new(SlowEngine {
            delay,
            in_flight: Arc::clone(&in_flight),
            peak: Arc::clone(&peak),
        });
        let cfg = test_config(cred);
        thread::spawn(move || {
            let _ = serve_listener_with(&server, listener, cfg);
        });
        // Give the pool a moment to start.
        thread::sleep(Duration::from_millis(50));

        let start = Instant::now();
        let mut handles = Vec::new();
        for id in 0..2u32 {
            let token = token.clone();
            handles.push(thread::spawn(move || post_search(port, id, &token)));
        }
        for h in handles {
            let (status, _) = h.join().unwrap();
            assert_eq!(status, 200, "expected successful tool response");
        }
        let total = start.elapsed();
        assert!(
            total < delay + delay / 2,
            "requests serialized ({total:?}) instead of overlapping under the worker pool"
        );
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "expected concurrent in-flight tool calls, peak={}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn full_queue_returns_retryable_overload() {
        // Saturate a 1-worker / 1-slot pool: hold the worker inside a gate, then
        // flood more concurrent clients than (workers + queue). At least one
        // must be refused with 503 + Retry-After (bounded backpressure).
        //
        // Important: release the gate *before* joining every client — one flood
        // client will be sitting in the queue and only completes once a worker
        // is free, which requires the gate to open.
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let blocked = Arc::new(AtomicUsize::new(0));
        let cred = RouteCredential::generate();
        let token = cred.as_str().to_string();
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = McpServer::new(GatedEngine {
            gate: Arc::clone(&gate),
            blocked: Arc::clone(&blocked),
        });
        let cfg = HttpServeConfig {
            worker_count: 1,
            queue_capacity: 1,
            request_timeout: Duration::from_secs(5),
            overload_retry_after_secs: 2,
            route_credential: Some(cred),
            repo_identity: None,
            model_fingerprint: None,
        };
        thread::spawn(move || {
            let _ = serve_listener_with(&server, listener, cfg);
        });
        thread::sleep(Duration::from_millis(50));

        // Occupy the single worker (blocks inside the gate).
        let token_blocker = token.clone();
        let blocker = thread::spawn(move || post_search(port, 1, &token_blocker));
        let deadline = Instant::now() + Duration::from_secs(2);
        while blocked.load(Ordering::SeqCst) < 1 {
            assert!(
                Instant::now() < deadline,
                "worker never entered the gated handler"
            );
            thread::sleep(Duration::from_millis(10));
        }

        // Flood well past workers+queue while the worker is still gated.
        // Capacity is 1 worker (busy) + 1 queue slot ⇒ at most 1 of these can
        // be buffered; the rest must get the retryable overload response.
        let flood = 8u32;
        let mut handles = Vec::new();
        for id in 0..flood {
            let token = token.clone();
            handles.push(thread::spawn(move || post_search(port, 100 + id, &token)));
        }

        // Poll until we observe an overload response, then open the gate so
        // queued work can drain. Do not join all flood threads first — one of
        // them is parked in the queue and needs the gate open to finish.
        let mut overload_body = String::new();
        let poll_deadline = Instant::now() + Duration::from_secs(3);
        let mut remaining = handles;
        while Instant::now() < poll_deadline && overload_body.is_empty() {
            let mut still = Vec::new();
            for h in remaining {
                if h.is_finished() {
                    let (status, body) = h.join().expect("client thread");
                    if status == 503 {
                        overload_body = body;
                    }
                } else {
                    still.push(h);
                }
            }
            remaining = still;
            if overload_body.is_empty() {
                thread::sleep(Duration::from_millis(20));
            }
        }

        // Always release the gate so blocker + queued flood clients can finish.
        {
            let (lock, cvar) = &*gate;
            let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
            *released = true;
            cvar.notify_all();
        }

        assert!(
            !overload_body.is_empty(),
            "expected at least one 503 overload under a full bounded queue"
        );
        assert!(
            overload_body.to_ascii_lowercase().contains("retry-after"),
            "missing Retry-After header: {overload_body}"
        );
        assert!(
            overload_body.contains("\"retryable\":true"),
            "overload body must mark retryable: {overload_body}"
        );

        let _ = blocker.join();
        for h in remaining {
            let _ = h.join();
        }
    }

    #[test]
    fn responses_still_use_connection_close() {
        let cred = RouteCredential::generate();
        let token = cred.as_str().to_string();
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = McpServer::new(SlowEngine {
            delay: Duration::from_millis(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        });
        let cfg = HttpServeConfig {
            route_credential: Some(cred),
            ..HttpServeConfig::default_limits()
        };
        thread::spawn(move || {
            let _ = serve_listener_with(&server, listener, cfg);
        });
        thread::sleep(Duration::from_millis(50));
        let (status, body) = post_search(port, 1, &token);
        assert_eq!(status, 200);
        assert!(
            body.to_ascii_lowercase().contains("connection: close"),
            "Connection: close semantics must be preserved: {body}"
        );
    }

    #[test]
    fn missing_credential_returns_401() {
        let cred = RouteCredential::generate();
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = McpServer::new(SlowEngine {
            delay: Duration::from_millis(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        });
        let cfg = HttpServeConfig {
            route_credential: Some(cred),
            ..HttpServeConfig::default_limits()
        };
        thread::spawn(move || {
            let _ = serve_listener_with(&server, listener, cfg);
        });
        thread::sleep(Duration::from_millis(50));

        // No Authorization header at all.
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 401"), "body={text}");
        assert!(text.contains("UNAUTHORIZED") || text.contains("route credential"));
    }

    #[test]
    fn wrong_credential_returns_401() {
        let cred = RouteCredential::generate();
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = McpServer::new(SlowEngine {
            delay: Duration::from_millis(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        });
        let cfg = HttpServeConfig {
            route_credential: Some(cred),
            ..HttpServeConfig::default_limits()
        };
        thread::spawn(move || {
            let _ = serve_listener_with(&server, listener, cfg);
        });
        thread::sleep(Duration::from_millis(50));
        let (status, body) = post_search(port, 1, "definitely-not-the-right-token!!");
        assert_eq!(status, 401, "body={body}");
    }

    #[test]
    fn overload_response_shape_is_retryable() {
        // Unit-level: the overload writer itself produces the contract shape
        // and Connection: close regardless of the accept-loop wiring. Pair the
        // client/server through a connected TCP pair so there is no race on
        // accept readiness.
        let listener = bind("127.0.0.1", 0).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        apply_stream_timeouts(&stream, Duration::from_secs(2));
        write_overload_response(stream, 3).expect("write overload");
        let _ = client.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).expect("read");
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 503"), "body={text}");
        assert!(text.to_ascii_lowercase().contains("connection: close"));
        assert!(text.to_ascii_lowercase().contains("retry-after: 3"));
        assert!(text.contains("\"retryable\":true"));
    }
}
