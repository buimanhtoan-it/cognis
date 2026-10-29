//! `cognis owner` daemon entry — acquire the workspace lease and serve MCP HTTP.
//!
//! Spawned by the thin adapter (`resolve_or_spawn_heavy`) as
//! `cognis owner --host 127.0.0.1 --port <p>`. A second invocation attaches
//! without opening the database (Requirement 16.3).

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::lease::resolve_repo_root_from_env;
use cognis_core::SemanticWarmPolicy;
use cognis_embed::ModelFingerprint;
use cognis_mcp::audit::AuditLog;
use cognis_mcp::http::{
    bind, serve_listener_with, HttpServeConfig, RouteCredential, ROUTE_CREDENTIAL_ENV,
};
use cognis_mcp::{McpServer, StoreEngine};

use crate::{attach_existing_owner, become_owner_deferred, OwnerSession};

/// Env var that forces a standalone `cognis-mcpd` / `cognis-indexd` binary to
/// enter the owner serve path (those bins have no `owner` subcommand).
pub const BECOME_OWNER_ENV: &str = "COGNIS_BECOME_OWNER";

/// How long a freshly spawned owner has to advertise a live endpoint.
const OWNER_READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerBind {
    host: String,
    port: Option<u16>,
    repo_root: Option<PathBuf>,
}

fn parse_owner_bind<I: IntoIterator<Item = String>>(args: I) -> OwnerBind {
    let mut host = "127.0.0.1".to_string();
    let mut port: Option<u16> = None;
    let mut repo_root: Option<PathBuf> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => {
                if let Some(v) = it.next() {
                    host = v;
                }
            }
            "--port" => {
                port = it.next().and_then(|v| v.parse::<u16>().ok());
            }
            "--repo-root" => {
                if let Some(v) = it.next() {
                    repo_root = Some(PathBuf::from(v));
                }
            }
            other if other.starts_with("--host=") => {
                host = other["--host=".len()..].to_string();
            }
            other if other.starts_with("--port=") => {
                port = other["--port=".len()..].parse::<u16>().ok();
            }
            other if other.starts_with("--repo-root=") => {
                repo_root = Some(PathBuf::from(&other["--repo-root=".len()..]));
            }
            _ => {}
        }
    }
    OwnerBind {
        host,
        port,
        repo_root,
    }
}

/// Standalone / busybox entry: process argv.
pub fn run() -> ExitCode {
    run_from(std::env::args_os())
}

/// Run the workspace owner. Reused by `cognis owner` and by standalone
/// mcpd/indexd binaries that set [`BECOME_OWNER_ENV`].
pub fn run_from<I: IntoIterator<Item = OsString>>(args: I) -> ExitCode {
    // Owner processes must never be classified as thin adapters.
    std::env::remove_var(thin_proxy_env_name());

    let flags: Vec<String> = args
        .into_iter()
        .skip(1)
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let bind_spec = parse_owner_bind(flags);
    let repo_root = bind_spec
        .repo_root
        .unwrap_or_else(resolve_repo_root_from_env);

    match become_owner_deferred(&repo_root, SemanticWarmPolicy::from_env()) {
        Ok(OwnerSession::Attached(attached)) => {
            eprintln!(
                "cognis-owner: attaching to live workspace owner (pid {}) without \
                 opening the database",
                attached.endpoint().pid
            );
            ExitCode::SUCCESS
        }
        Ok(OwnerSession::Acquired(owned)) => serve_owned(owned, &bind_spec.host, bind_spec.port),
        Err(err) => {
            eprintln!("cognis-owner: failed to become workspace owner: {err}");
            ExitCode::FAILURE
        }
    }
}

fn thin_proxy_env_name() -> &'static str {
    "COGNIS_MCP_PROXY"
}

fn serve_owned(owned: crate::OwnedWorkspace, host: &str, port: Option<u16>) -> ExitCode {
    let Some(db) = owned.database().cloned() else {
        eprintln!("cognis-owner: acquired owner has no database handle");
        return ExitCode::FAILURE;
    };
    let engine = match StoreEngine::from_open_database(db, SemanticWarmPolicy::from_env()) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("cognis-owner: failed to wrap workspace database: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut server = McpServer::new(engine);
    if let Ok(path) = std::env::var("COGNIS_AUDIT_LOG") {
        if !path.trim().is_empty() {
            server = server.with_audit(AuditLog::new(path));
        }
    }

    let credential = match RouteCredential::from_env_or_generate() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cognis-owner: invalid {ROUTE_CREDENTIAL_ENV}: {err}");
            return ExitCode::FAILURE;
        }
    };
    std::env::set_var(ROUTE_CREDENTIAL_ENV, credential.as_str());

    let port = port.unwrap_or(0);
    let listener = match bind(host, port) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("cognis-owner: failed to bind {host}:{port}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let bound = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("cognis-owner: bound listener has no local address: {err}");
            return ExitCode::FAILURE;
        }
    };
    let url = format!("http://{bound}/mcp");
    let repo_root = PathBuf::from(&owned.identity().root);
    let cfg = cognis_core::Config::load(&repo_root).unwrap_or_default();
    let fingerprint = ModelFingerprint::from_env_or_derive(&cfg);
    if let Err(err) =
        advertise_query_endpoint(&repo_root, &url, credential.as_str(), fingerprint.as_str())
    {
        eprintln!("cognis-owner: warning: could not write endpoint file: {err}");
    }

    eprintln!(
        "cognis-owner ready [{url}] — {} tools, contract v{} (route credential required)",
        cognis_core::MCP_TOOLS.len(),
        cognis_core::CONTRACT_VERSION
    );
    let _ = io::stderr().flush();

    let serve_cfg = HttpServeConfig::with_credential(credential)
        .with_repo_identity(owned.identity().clone())
        .with_model_fingerprint(fingerprint);

    let result = serve_listener_with(&server, listener, serve_cfg);
    drop(owned);
    if let Err(err) = result {
        eprintln!("cognis-owner: http serve loop error: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// True when `exe_stem` names the multi-call `cognis` binary (including
/// platform-suffixed release artifacts like `cognis-x86_64-unknown-linux-gnu`)
/// rather than a standalone surface binary (`cognis-mcpd`, `cognis-indexd`,
/// `cognis-owner`, `cognis-cli`).
pub fn is_busybox_stem(exe_stem: &str) -> bool {
    let stem = exe_stem.to_ascii_lowercase();
    !stem.contains("owner")
        && !stem.contains("mcpd")
        && !stem.contains("indexd")
        && !stem.ends_with("-cli")
        && stem != "cognis-cli"
}

/// Prefix argv the thin adapter uses to launch this surface from `current_exe`.
///
/// * Busybox `cognis` (or release artifact `cognis-<target>`) → `owner`
/// * Symlink / stem containing `owner` → no extra token
/// * Standalone mcpd/indexd → `--transport http` (mcpd) or [`BECOME_OWNER_ENV`]
pub fn heavy_surface_args(exe_stem: &str) -> Vec<&'static str> {
    if is_busybox_stem(exe_stem) {
        vec!["owner"]
    } else {
        vec![]
    }
}

/// Spawn a detached `cognis owner` (or equivalent) for `repo_root` and wait
/// until `.cognis/mcpd.endpoint` accepts connections. The caller must not
/// open the workspace database (Requirement 16.3 / 17.1).
pub fn ensure_owner_running(repo_root: &Path) -> io::Result<String> {
    if let Ok(Some(attached)) = attach_existing_owner(repo_root) {
        if let Some(url) = attached.endpoint().query_url.clone() {
            if endpoint_accepts(&url) {
                return Ok(url);
            }
        }
    }
    spawn_owner_detached(repo_root)?;
    wait_for_advertised_endpoint(repo_root)
}

fn spawn_owner_detached(repo_root: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(&exe);
    let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("cognis");
    for arg in heavy_surface_args(stem) {
        cmd.arg(arg);
    }
    if !is_busybox_stem(stem) && !stem.to_ascii_lowercase().contains("owner") {
        cmd.env(BECOME_OWNER_ENV, "1");
    }
    cmd.env_remove(thin_proxy_env_name())
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = cmd.spawn()?;
    std::mem::forget(child);
    Ok(())
}

fn wait_for_advertised_endpoint(repo_root: &Path) -> io::Result<String> {
    let deadline = Instant::now() + OWNER_READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(url) = advertised_endpoint_url(repo_root) {
            if endpoint_accepts(&url) {
                return Ok(url);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "workspace owner did not advertise a live query endpoint in time",
    ))
}

fn advertised_endpoint_url(repo_root: &Path) -> Option<String> {
    let path = repo_root
        .join(cognis_core::config::CONFIG_DIR_NAME)
        .join("mcpd.endpoint");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn endpoint_accepts(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let host_port = rest.split('/').next().unwrap_or(rest);
    let (host, port_str) = match host_port.split_once(':') {
        Some(pair) => pair,
        None => return false,
    };
    let Ok(port) = port_str.parse::<u16>() else {
        return false;
    };
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

pub fn advertise_query_endpoint(
    repo_root: &Path,
    url: &str,
    credential: &str,
    fingerprint: &str,
) -> io::Result<()> {
    crate::write_mcpd_endpoint(repo_root, url, Some(credential), Some(fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_port_and_repo_root() {
        let b = parse_owner_bind(vec![
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            "50123".into(),
            "--repo-root".into(),
            "/tmp/repo".into(),
        ]);
        assert_eq!(b.host, "127.0.0.1");
        assert_eq!(b.port, Some(50123));
        assert_eq!(b.repo_root.as_deref(), Some(Path::new("/tmp/repo")));
    }

    #[test]
    fn busybox_surface_arg_is_owner() {
        assert_eq!(heavy_surface_args("cognis"), vec!["owner"]);
        assert_eq!(
            heavy_surface_args("cognis-x86_64-unknown-linux-gnu"),
            vec!["owner"]
        );
        assert!(heavy_surface_args("cognis-mcpd").is_empty());
        assert!(heavy_surface_args("cognis-indexd").is_empty());
        assert!(heavy_surface_args("cognis-cli").is_empty());
        assert!(heavy_surface_args("cognis-owner").is_empty());
    }
}
