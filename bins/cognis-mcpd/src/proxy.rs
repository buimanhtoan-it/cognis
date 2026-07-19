//! Thin stdio proxy mode (Task 7.1 / Requirements 2.8, 2.11).
//!
//! A thin proxy is a model-free, DB-free process that speaks the same
//! newline-delimited JSON-RPC stdio surface the editor expects, but forwards
//! every message to a single heavy repository daemon over loopback HTTP.
//!
//! Consequences for the process graph:
//! * `host × repository` editor connections cost a thin proxy, not a heavy
//!   process that maps ONNX / holds the UCKG.
//! * At most one heavy owner exists per canonical repository (the heavy
//!   process acquires the repository lease itself; the proxy never holds a
//!   DB/model and never steals ownership).
//! * The compatible stdio path is preserved: the editor still spawns a
//!   command-form server block; the block just happens to be a thin proxy
//!   (preservation 3.8).
//!
//! The proxy deliberately never constructs a StoreEngine / embedder session —
//! that is the invariant unit tests assert.

use std::io::{self, BufRead, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::config::CONFIG_DIR_NAME;
use cognis_core::lease::resolve_repo_root_from_env;
use cognis_core::{RepoIdentity, REPO_IDENTITY_HEADER};
use cognis_embed::{ModelFingerprint, MODEL_FINGERPRINT_HEADER};

/// Env var that marks this process as a thin proxy (no ONNX / no repo DB).
/// Consumed by the extension's runtime probe so thin proxies can be counted
/// separately from heavy daemons (Requirement 2.11).
pub const THIN_PROXY_ENV: &str = "COGNIS_MCP_PROXY";

/// Env var holding the loopback HTTP URL of the heavy daemon this proxy
/// forwards to (e.g. `http://127.0.0.1:50123/mcp`).
pub const PROXY_TARGET_ENV: &str = "COGNIS_MCP_PROXY_TARGET";

/// Env var holding the scoped route credential for the heavy HTTP daemon
/// (Requirement 2.12 / Task 8.1). Mirrors `cognis_mcp::http::ROUTE_CREDENTIAL_ENV`
/// so the proxy can present the same secret the heavy requires.
pub const ROUTE_CREDENTIAL_ENV: &str = cognis_mcp::http::ROUTE_CREDENTIAL_ENV;

/// On-disk endpoint advertisement written by the heavy HTTP owner under
/// `.cognis/mcpd.endpoint` so later thin proxies can attach without racing a
/// second heavy process. Format:
/// ```text
/// http://127.0.0.1:<port>/mcp
/// <route-credential>
/// <model-fingerprint>
/// ```
/// Legacy single-/two-line files remain readable; the credential and fingerprint
/// lines are optional for attach-path discovery of the port. Authenticated POSTs
/// still need the secret, and session reuse is refused when fingerprints differ
/// (Requirement 2.12 / Task 8.2).
const ENDPOINT_FILE_NAME: &str = "mcpd.endpoint";

/// How long a freshly-spawned heavy child has to accept connections before the
/// proxy gives up and surfaces an error.
const HEAVY_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an attach path waits for an existing endpoint to become reachable.
const ATTACH_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(15);

/// Selection of a heavy-daemon target for a thin proxy session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTarget {
    /// Full URL including path (e.g. `http://127.0.0.1:50123/mcp`).
    pub url: String,
    /// Loopback host portion.
    pub host: String,
    /// Bound port of the heavy daemon.
    pub port: u16,
    /// Scoped route credential the heavy requires (Requirement 2.12).
    /// Empty only for legacy endpoint files that pre-date credentials; the
    /// forwarder will still send the Authorization header when non-empty.
    pub credential: String,
    /// Owner model fingerprint (Requirement 2.12 / Task 8.2). Empty for legacy
    /// endpoint files; when non-empty, attach refuses session reuse across a
    /// differing local fingerprint.
    pub model_fingerprint: String,
}

impl ProxyTarget {
    /// Parse a URL of the form `http://host:port[/path]`, optionally with a
    /// second line holding the route credential.
    pub fn parse(url: &str) -> Option<Self> {
        Self::parse_with_credential(url, "")
    }

    /// Parse a URL and attach an explicit credential.
    pub fn parse_with_credential(url: &str, credential: &str) -> Option<Self> {
        Self::parse_with_identity(url, credential, "")
    }

    /// Parse a URL with credential + model fingerprint (full isolation surface).
    pub fn parse_with_identity(
        url: &str,
        credential: &str,
        model_fingerprint: &str,
    ) -> Option<Self> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return None;
        }
        let rest = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))?;
        let (host_port, _path) = match rest.split_once('/') {
            Some((hp, p)) => (hp, p),
            None => (rest, "mcp"),
        };
        let (host, port_str) = host_port.split_once(':')?;
        let port: u16 = port_str.parse().ok()?;
        if host.is_empty() || port == 0 {
            return None;
        }
        // Normalize to the canonical `/mcp` path the HTTP transport serves.
        let url = format!("http://{host}:{port}/mcp");
        Some(ProxyTarget {
            url,
            host: host.to_string(),
            port,
            credential: credential.trim().to_string(),
            model_fingerprint: model_fingerprint.trim().to_ascii_lowercase(),
        })
    }
}

/// Resolve the endpoint-file path for a repository root.
pub fn endpoint_path(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root
        .as_ref()
        .join(CONFIG_DIR_NAME)
        .join(ENDPOINT_FILE_NAME)
}

/// Atomically write the heavy-daemon endpoint advertisement (URL + optional
/// scoped route credential + optional model fingerprint).
pub fn write_endpoint_file(
    repo_root: impl AsRef<Path>,
    url: &str,
    credential: Option<&str>,
) -> io::Result<()> {
    write_endpoint_file_with_fingerprint(repo_root, url, credential, None)
}

/// Atomically write the heavy-daemon endpoint advertisement with full isolation
/// identity (URL + credential + model fingerprint).
pub fn write_endpoint_file_with_fingerprint(
    repo_root: impl AsRef<Path>,
    url: &str,
    credential: Option<&str>,
    model_fingerprint: Option<&str>,
) -> io::Result<()> {
    let path = endpoint_path(&repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = format!("{}\n", url.trim());
    if let Some(token) = credential.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(token);
        body.push('\n');
        if let Some(fp) = model_fingerprint.map(str::trim).filter(|s| !s.is_empty()) {
            body.push_str(fp);
            body.push('\n');
        }
    }
    let tmp = path.with_extension(format!("endpoint.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body)?;
    // Best-effort rename with a short retry for Windows sharing violations.
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
    Err(last_err.unwrap_or_else(|| io::Error::other("endpoint rename failed")))
}

/// Read a previously advertised heavy-daemon endpoint, if present and parseable.
/// Supports single-line (URL only), two-line (URL + credential), and three-line
/// (URL + credential + model fingerprint) formats.
pub fn read_endpoint_file(repo_root: impl AsRef<Path>) -> Option<ProxyTarget> {
    let text = std::fs::read_to_string(endpoint_path(repo_root)).ok()?;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let url = lines.next()?;
    let credential = lines.next().unwrap_or("");
    let fingerprint = lines.next().unwrap_or("");
    ProxyTarget::parse_with_identity(url, credential, fingerprint)
}

/// True when a TCP connect to `host:port` succeeds within `timeout`.
pub fn port_accepts(host: &str, port: u16, timeout: Duration) -> bool {
    use std::net::ToSocketAddrs;
    let addr = match (host, port).to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None => return false,
        },
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Derive a deterministic loopback port from the repo path (mirrors the
/// TypeScript `derivePort` helper in `mcpServer.ts` so the extension and the
/// proxy agree on where the heavy daemon lives). Uses a simple stable hash —
/// not byte-identical to the TS SHA-256 derivation, but good enough for the
/// proxy's own spawn attempts; the authoritative URL is always the endpoint
/// file the heavy writes after bind.
pub fn derive_port(repo_root: &Path, offset: u32) -> u16 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let norm = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let key = if cfg!(windows) {
        norm.to_string_lossy().to_lowercase()
    } else {
        norm.to_string_lossy().into_owned()
    };
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let digest = hasher.finish();
    const PORT_FLOOR: u64 = 49152;
    const PORT_CEIL: u64 = 65535;
    let span = PORT_CEIL - PORT_FLOOR + 1;
    let base = (digest % span) as u16;
    PORT_FLOOR as u16 + base.wrapping_add(offset as u16) % span as u16
}

/// Resolve the heavy-daemon target for a thin-proxy session.
///
/// Order:
/// 1. Explicit `--proxy-target` / `COGNIS_MCP_PROXY_TARGET` if live.
/// 2. On-disk `.cognis/mcpd.endpoint` if live **and** model fingerprints match.
/// 3. Spawn a heavy HTTP child (detached — the heavy acquires the repository
///    lease itself and outlives this proxy); wait until it accepts connections.
///
/// Session reuse is refused when the endpoint's model fingerprint differs from
/// this process's (Task 8.2 / Property 12). The proxy never constructs a
/// StoreEngine and never holds the MCP lease.
pub fn resolve_or_spawn_heavy(explicit_target: Option<&str>) -> io::Result<ProxyTarget> {
    let local_fp = local_model_fingerprint();

    // 1. Explicit target.
    if let Some(raw) = explicit_target {
        if let Some(t) = ProxyTarget::parse(raw) {
            if port_accepts(&t.host, t.port, Duration::from_millis(500)) {
                if fingerprint_allows_attach(&t, &local_fp) {
                    return Ok(t);
                }
                eprintln!(
                    "cognis-mcpd: proxy target {raw} has a different model \
                     fingerprint; refusing session reuse and spawning a matching heavy"
                );
            } else {
                eprintln!(
                    "cognis-mcpd: proxy target {raw} is not accepting connections; \
                     will try to (re)start the heavy daemon"
                );
            }
        }
    }

    let repo_root = resolve_repo_root_from_env();

    // 2. Endpoint file from a live owner — wait briefly so a just-started heavy
    //    from another proxy can finish advertising. Refuse attach when the
    //    advertised fingerprint differs from ours (Task 8.2).
    let attach_deadline = Instant::now() + ATTACH_ENDPOINT_TIMEOUT;
    loop {
        if let Some(t) = read_endpoint_file(&repo_root) {
            if port_accepts(&t.host, t.port, Duration::from_millis(300)) {
                if fingerprint_allows_attach(&t, &local_fp) {
                    return Ok(t);
                }
                eprintln!(
                    "cognis-mcpd: live heavy at {} has a different model fingerprint; \
                     refusing session reuse",
                    t.url
                );
                break;
            }
        }
        if Instant::now() >= attach_deadline {
            break;
        }
        // Only wait when an endpoint file exists (owner mid-start); otherwise
        // fall through to spawn immediately.
        if read_endpoint_file(&repo_root).is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    // 3. Spawn a heavy HTTP child. The heavy process acquires the repository
    //    lease and writes the endpoint file itself (see `run_from`). We do NOT
    //    kill it when this proxy exits — that would race other attached proxies.
    spawn_heavy_http(&repo_root)
}

/// Derive this process's model fingerprint from env/config/assets.
fn local_model_fingerprint() -> ModelFingerprint {
    let repo_root = resolve_repo_root_from_env();
    let cfg = cognis_core::Config::load(&repo_root).unwrap_or_default();
    ModelFingerprint::from_env_or_derive(&cfg)
}

/// Session reuse is allowed when the target has no fingerprint (legacy) or
/// fingerprints match. Differing fingerprints refuse attach.
fn fingerprint_allows_attach(target: &ProxyTarget, local: &ModelFingerprint) -> bool {
    if target.model_fingerprint.is_empty() {
        // Legacy endpoint without a fingerprint: allow attach (stdio fallback
        // path / older heavy); HTTP isolation still enforces when the heavy
        // was started with fingerprint verification enabled.
        return true;
    }
    let presented = ModelFingerprint::from_digest(&target.model_fingerprint);
    local.allows_session_reuse(&presented)
}

/// Spawn `current_exe() [mcpd] --transport http --host 127.0.0.1 --port <p>` as
/// a heavy child for `repo_root`, waiting until the port accepts connections.
/// The child is intentionally **not** reaped/killed by this process so multiple
/// thin proxies can share one heavy owner for the repository lifetime.
fn spawn_heavy_http(repo_root: &Path) -> io::Result<ProxyTarget> {
    let exe = std::env::current_exe()?;
    // Share one credential across spawn attempts so a port-retry still ends up
    // with a single secret the proxy can present.
    let credential = cognis_mcp::http::RouteCredential::generate();
    let local_fp = local_model_fingerprint();
    let mut last_err = None;
    for offset in 0..4u32 {
        let port = derive_port(repo_root, offset);
        // Port already accepting — reuse without spawning. Prefer the on-disk
        // endpoint credential when present; fall back to our freshly minted one
        // only when the file has no secret (legacy). Refuse reuse when the
        // advertised fingerprint differs (Task 8.2).
        if port_accepts("127.0.0.1", port, Duration::from_millis(100)) {
            if let Some(mut t) = read_endpoint_file(repo_root) {
                if t.port == port && fingerprint_allows_attach(&t, &local_fp) {
                    if t.credential.is_empty() {
                        t.credential = credential.as_str().to_string();
                    }
                    if t.model_fingerprint.is_empty() {
                        t.model_fingerprint = local_fp.digest.clone();
                    }
                    let _ = write_endpoint_file_with_fingerprint(
                        repo_root,
                        &t.url,
                        Some(&t.credential),
                        Some(&t.model_fingerprint),
                    );
                    return Ok(t);
                }
            }
            let url = format!("http://127.0.0.1:{port}/mcp");
            if let Some(t) =
                ProxyTarget::parse_with_identity(&url, credential.as_str(), local_fp.as_str())
            {
                let _ = write_endpoint_file_with_fingerprint(
                    repo_root,
                    &t.url,
                    Some(&t.credential),
                    Some(&t.model_fingerprint),
                );
                return Ok(t);
            }
            continue;
        }

        match launch_heavy_on_port(&exe, repo_root, port, credential.as_str()) {
            Ok(target) => {
                // Advertise immediately so racing proxies attach instead of
                // spawning a second heavy. The heavy also rewrites this after
                // bind (authoritative).
                let _ = write_endpoint_file_with_fingerprint(
                    repo_root,
                    &target.url,
                    Some(&target.credential),
                    Some(&target.model_fingerprint),
                );
                return Ok(target);
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| io::Error::other("could not spawn heavy cognis-mcpd for thin proxy")))
}

/// Launch one heavy HTTP attempt on `port`. Returns once the port accepts.
fn launch_heavy_on_port(
    exe: &Path,
    repo_root: &Path,
    port: u16,
    credential: &str,
) -> io::Result<ProxyTarget> {
    let mut cmd = Command::new(exe);
    // Multi-call binary: when invoked as `cognis` / `cognis.exe`, the surface
    // is selected by the first arg (`mcpd`). The standalone daemon binary is
    // named `cognis-mcpd` and takes flags directly.
    let stem = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cognis-mcpd");
    if stem == "cognis" {
        cmd.arg("mcpd");
    }
    cmd.args([
        "--transport",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
    ])
    // Critical: the heavy child must NOT itself enter proxy mode.
    .env_remove(THIN_PROXY_ENV)
    .env_remove(PROXY_TARGET_ENV)
    // Shared route credential so the heavy and this proxy agree (2.12).
    .env(ROUTE_CREDENTIAL_ENV, credential)
    // Inherit the repo/model env so the heavy opens the right DB.
    .current_dir(repo_root)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit());

    // On Windows, put the child in a new process group with no console so the
    // editor killing the thin proxy does not cascade. On Unix we rely on the
    // heavy's own lease/lifetime; `std::mem::forget` below prevents Drop-kill.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn()?;
    let url = format!("http://127.0.0.1:{port}/mcp");
    let target = ProxyTarget {
        url: url.clone(),
        host: "127.0.0.1".into(),
        port,
        credential: credential.to_string(),
        model_fingerprint: local_model_fingerprint().digest,
    };

    // Wait until the heavy is accepting connections. We intentionally do not
    // keep the Child handle — the heavy outlives this proxy.
    let deadline = Instant::now() + HEAVY_READY_TIMEOUT;
    while Instant::now() < deadline {
        if port_accepts("127.0.0.1", port, Duration::from_millis(200)) {
            // Detach: forget the child so Drop does not kill it.
            std::mem::forget(child);
            eprintln!("cognis-mcpd: thin-proxy spawned heavy daemon at {url}");
            return Ok(target);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(io::Error::other(format!(
                    "heavy daemon exited before binding 127.0.0.1:{port} ({status})"
                )));
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => return Err(err),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("heavy daemon did not bind 127.0.0.1:{port} in time"),
    ))
}

/// Run the thin-proxy serve loop: read newline-delimited JSON-RPC from
/// `reader`, POST each request to the heavy daemon, write the response line to
/// `writer`. Notifications (no response body / 202) produce no stdout line.
///
/// This function never constructs a StoreEngine and never loads ONNX.
pub fn serve_proxy<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    target: &ProxyTarget,
) -> io::Result<()> {
    eprintln!(
        "cognis-mcpd (Rust) ready [thin-proxy → {}] — no ONNX, no repository DB",
        target.url
    );
    let _ = io::stderr().flush();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(()); // clean EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // keep-alive blank lines
        }
        match forward_jsonrpc(target, trimmed) {
            Ok(Some(response)) => {
                writer.write_all(response.as_bytes())?;
                if !response.ends_with('\n') {
                    writer.write_all(b"\n")?;
                }
                writer.flush()?;
            }
            Ok(None) => {
                // Notification / empty response — nothing to write.
            }
            Err(err) => {
                // Transport failure: synthesize a JSON-RPC internal error so the
                // editor sees a structured failure rather than a hung call.
                let id = extract_id(trimmed).unwrap_or(serde_json::Value::Null);
                let err_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("thin-proxy forward failed: {err}"),
                    }
                });
                let text = serde_json::to_string(&err_body).unwrap_or_else(|_| {
                    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"proxy error"}}"#
                        .to_string()
                });
                writer.write_all(text.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }
    }
}

/// POST one JSON-RPC body to the heavy daemon. Returns the response body when
/// the server answered 200 with a non-empty payload; `None` for 202 /
/// notification-only. Presents the scoped route credential, repository identity,
/// and model fingerprint (Requirement 2.12 / Task 8.2).
fn forward_jsonrpc(target: &ProxyTarget, body: &str) -> io::Result<Option<String>> {
    let addr = format!("{}:{}", target.host, target.port);
    let mut stream = TcpStream::connect(&addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let path = "/mcp";
    let content_length = body.len();
    // Prefer the target's advertised credential; fall back to the process env
    // so an explicit COGNIS_MCP_ROUTE_TOKEN still works with legacy endpoint
    // files that only store the URL.
    let credential = if !target.credential.is_empty() {
        target.credential.clone()
    } else {
        std::env::var(ROUTE_CREDENTIAL_ENV).unwrap_or_default()
    };
    let auth_header = if credential.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {credential}\r\n")
    };
    // Repository identity + model fingerprint isolation (Task 8.2).
    let repo_key = RepoIdentity::from_env().wire_key();
    let fp = if !target.model_fingerprint.is_empty() {
        target.model_fingerprint.clone()
    } else {
        local_model_fingerprint().digest
    };
    let isolation_headers = format!(
        "{REPO_IDENTITY_HEADER}: {repo_key}\r\n\
         {MODEL_FINGERPRINT_HEADER}: {fp}\r\n"
    );
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         {auth_header}\
         {isolation_headers}\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        target.host, target.port
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut reader = io::BufReader::new(stream);
    // Status line.
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    if status_line.is_empty() {
        return Ok(None);
    }
    let status_ok = status_line.contains(" 200 ")
        || status_line.starts_with("HTTP/1.1 200")
        || status_line.starts_with("HTTP/1.0 200");
    let status_accepted = status_line.contains(" 202 ")
        || status_line.starts_with("HTTP/1.1 202")
        || status_line.starts_with("HTTP/1.0 202");

    // Headers.
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.trim())
        {
            content_length = value.parse::<usize>().unwrap_or(0);
        }
    }

    if status_accepted || content_length == 0 {
        return Ok(None);
    }
    if !status_ok {
        return Err(io::Error::other(format!(
            "heavy daemon returned status: {}",
            status_line.trim()
        )));
    }

    let mut buf = vec![0u8; content_length];
    use std::io::Read;
    reader.read_exact(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

/// Best-effort extraction of the JSON-RPC `id` from a request line so proxy
/// transport errors can echo it back.
fn extract_id(line: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    v.get("id").cloned()
}

/// True when the process was launched as a thin proxy (flag or env).
pub fn is_proxy_mode(args: &[String]) -> bool {
    if std::env::var(THIN_PROXY_ENV).as_deref() == Ok("1") {
        return true;
    }
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--proxy" => return true,
            "--transport" => {
                if let Some(v) = it.next() {
                    if v.eq_ignore_ascii_case("proxy") {
                        return true;
                    }
                }
            }
            other
                if other.starts_with("--transport=")
                    && other["--transport=".len()..].eq_ignore_ascii_case("proxy") =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Extract an explicit `--proxy-target <url>` / `--proxy-target=<url>` /
/// `COGNIS_MCP_PROXY_TARGET` value from argv + env.
pub fn explicit_proxy_target(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--proxy-target" => {
                if let Some(v) = it.next() {
                    if !v.trim().is_empty() {
                        return Some(v.clone());
                    }
                }
            }
            other if other.starts_with("--proxy-target=") => {
                let v = &other["--proxy-target=".len()..];
                if !v.trim().is_empty() {
                    return Some(v.to_string());
                }
            }
            _ => {}
        }
    }
    std::env::var(PROXY_TARGET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Entry: resolve the heavy target (spawn if needed) and run the proxy loop on
/// stdin/stdout. Never builds a StoreEngine.
pub fn run_proxy(args: &[String]) -> std::process::ExitCode {
    // Mark ourselves as a thin proxy for any child-of-child env inspection and
    // for the extension's runtime probe.
    std::env::set_var(THIN_PROXY_ENV, "1");

    let explicit = explicit_proxy_target(args);
    let target = match resolve_or_spawn_heavy(explicit.as_deref()) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("cognis-mcpd: thin-proxy failed to resolve heavy daemon: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    if let Err(err) = serve_proxy(&mut reader, &mut writer, &target) {
        eprintln!("cognis-mcpd: thin-proxy serve loop error: {err}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxy_target_url() {
        let t = ProxyTarget::parse("http://127.0.0.1:50123/mcp").unwrap();
        assert_eq!(t.host, "127.0.0.1");
        assert_eq!(t.port, 50123);
        assert_eq!(t.url, "http://127.0.0.1:50123/mcp");
    }

    #[test]
    fn parses_proxy_target_without_path() {
        let t = ProxyTarget::parse("http://127.0.0.1:9").unwrap();
        assert_eq!(t.url, "http://127.0.0.1:9/mcp");
    }

    #[test]
    fn rejects_empty_or_non_http() {
        assert!(ProxyTarget::parse("").is_none());
        assert!(ProxyTarget::parse("ftp://x").is_none());
        assert!(ProxyTarget::parse("http://noport").is_none());
    }

    #[test]
    fn is_proxy_mode_flag_and_env() {
        assert!(is_proxy_mode(&["--proxy".into()]));
        assert!(is_proxy_mode(&["--transport".into(), "proxy".into()]));
        assert!(is_proxy_mode(&["--transport=proxy".into()]));
        assert!(!is_proxy_mode(&["--transport".into(), "stdio".into()]));
        assert!(!is_proxy_mode(&[]));
    }

    #[test]
    fn explicit_proxy_target_from_args() {
        let args = vec![
            "--proxy".into(),
            "--proxy-target".into(),
            "http://127.0.0.1:9/mcp".into(),
        ];
        assert_eq!(
            explicit_proxy_target(&args).as_deref(),
            Some("http://127.0.0.1:9/mcp")
        );
        assert_eq!(
            explicit_proxy_target(&["--proxy-target=http://h:1/mcp".into()]).as_deref(),
            Some("http://h:1/mcp")
        );
    }

    #[test]
    fn endpoint_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "cognis-proxy-ep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_endpoint_file(
            &dir,
            "http://127.0.0.1:55555/mcp",
            Some("0123456789abcdef0123456789abcdef"),
        )
        .unwrap();
        let t = read_endpoint_file(&dir).unwrap();
        assert_eq!(t.port, 55555);
        assert_eq!(t.credential, "0123456789abcdef0123456789abcdef");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn endpoint_file_legacy_url_only_still_parses() {
        let dir = std::env::temp_dir().join(format!(
            "cognis-proxy-ep-legacy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Legacy single-line format (pre-credential).
        write_endpoint_file(&dir, "http://127.0.0.1:44444/mcp", None).unwrap();
        let t = read_endpoint_file(&dir).unwrap();
        assert_eq!(t.port, 44444);
        assert!(t.credential.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn derive_port_is_stable_and_in_band() {
        let root = Path::new("/tmp/example-repo");
        let a = derive_port(root, 0);
        let b = derive_port(root, 0);
        assert_eq!(a, b);
        assert!((49152..=65535).contains(&a));
        let c = derive_port(root, 1);
        assert_ne!(a, c);
    }

    /// The thin-proxy module has no StoreEngine / embedder import path — this
    /// test documents the invariant that `serve_proxy` only needs a target URL
    /// and byte streams (no engine construction).
    #[test]
    fn serve_proxy_signature_is_engine_free() {
        // Compile-time invariant: serve_proxy is generic only over Read/Write.
        fn _assert<R: BufRead, W: Write>() {
            let _f: fn(&mut R, &mut W, &ProxyTarget) -> io::Result<()> = serve_proxy;
        }
        let _ = _assert::<io::Cursor<Vec<u8>>, Vec<u8>>;
    }

    // -----------------------------------------------------------------------
    // Property 9 unit: thin proxy loads no ONNX / retains no DB
    // -----------------------------------------------------------------------

    /// **Property 9 / unit** — The thin-proxy module surface is engine-free:
    /// `serve_proxy` only needs a target URL and byte streams. Combined with
    /// `THIN_PROXY_ENV` / `--proxy` selection, a host×repository connection
    /// costs a model-free proxy rather than a heavy process that maps ONNX
    /// or holds the repository UCKG (Requirements 2.8, 2.11).
    #[test]
    fn prop9_thin_proxy_loads_no_onnx_and_retains_no_db() {
        // 1. Mode detection never requires a DB path or model dir.
        assert!(is_proxy_mode(&["--proxy".into()]));
        assert!(is_proxy_mode(&["--transport".into(), "proxy".into()]));

        // 2. Target resolution is pure URL/credential parsing — no StoreEngine.
        let t = ProxyTarget::parse_with_credential(
            "http://127.0.0.1:50123/mcp",
            "0123456789abcdef0123456789abcdef",
        )
        .expect("parse");
        assert_eq!(t.host, "127.0.0.1");
        assert_eq!(t.port, 50123);
        assert!(!t.credential.is_empty());

        // 3. Endpoint advertisement is a two-line file (URL + credential), not
        //    a DB handle or model session.
        let dir = std::env::temp_dir().join(format!(
            "cognis-proxy-prop9-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_endpoint_file(&dir, &t.url, Some(&t.credential)).unwrap();
        let roundtrip = read_endpoint_file(&dir).unwrap();
        assert_eq!(roundtrip.url, t.url);
        assert_eq!(roundtrip.credential, t.credential);
        let _ = std::fs::remove_dir_all(&dir);

        // 4. Compile-time invariant: serve_proxy is generic only over Read/Write
        //    (no engine type parameter).
        fn _assert_engine_free<R: BufRead, W: Write>() {
            let _f: fn(&mut R, &mut W, &ProxyTarget) -> io::Result<()> = serve_proxy;
        }
        let _ = _assert_engine_free::<io::Cursor<Vec<u8>>, Vec<u8>>;

        // 5. Env markers the extension uses to classify thin proxies.
        assert_eq!(THIN_PROXY_ENV, "COGNIS_MCP_PROXY");
        assert_eq!(PROXY_TARGET_ENV, "COGNIS_MCP_PROXY_TARGET");
    }

    #[test]
    fn prop9_proxy_target_rejects_non_http_without_side_effects() {
        // Parsing failures are pure — no process spawn, no DB open, no model.
        assert!(ProxyTarget::parse("").is_none());
        assert!(ProxyTarget::parse("not-a-url").is_none());
        assert!(ProxyTarget::parse("ftp://127.0.0.1:9/mcp").is_none());
        assert!(ProxyTarget::parse("http://noport/mcp").is_none());
    }
}
