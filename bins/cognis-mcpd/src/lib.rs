//! cognis-mcpd — MCP server entry point (Task 7).
//!
//! Serves the read-only [`McpServer`] over the 8-tool contract (Requirement 3)
//! on one of three transports:
//!
//! * **stdio** (default) — newline-delimited JSON-RPC on stdin/stdout, the
//!   transport an editor spawns and owns. A full heavy process that may open
//!   the UCKG and (lazily) load ONNX.
//! * **http** — `--transport http --host <h> --port <p>`, a standalone
//!   localhost server the panel-managed "Start MCP server" flow launches and
//!   the editor connects to by URL. Same JSON-RPC surface, same contract.
//! * **thin proxy** — `--proxy` / `--transport proxy` / `COGNIS_MCP_PROXY=1`:
//!   a model-free, DB-free stdio process that forwards JSON-RPC to a single
//!   heavy repository daemon over loopback HTTP. So `host × repository`
//!   connections cost a thin proxy, not a heavy process (Requirements 2.8,
//!   2.11; preservation 3.8). See [`proxy`].
//!
//! The backing [`StoreEngine`] is chosen from the environment (heavy modes
//! only):
//!
//! * `COGNIS_DB_PATH=<path>` — serve a real UCKG at that path (and wire the
//!   configured embedder for semantic search).
//! * otherwise (or `COGNIS_MCP_FIXTURE=1`) — serve a deterministic in-memory
//!   fixture UCKG. This is the backing the contract e2e drives a *live* server
//!   against without depending on the indexer (Task 8).
//!
//! The server is read-only and applies hard caps + a hashed-argument audit log
//! before each tool runs. The audit log path honours `COGNIS_AUDIT_LOG`. The
//! [`run`] / [`run_from`] entry points are reused both by the standalone
//! `cognis-mcpd` binary and the single multi-call `cognis` binary.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use cognis_core::lease::{
    acquire_or_attach, resolve_repo_root_from_env, AcquireOutcome, LeaseGuard, LeaseRole,
};
use cognis_core::SemanticWarmPolicy;
use cognis_mcp::audit::AuditLog;
use cognis_mcp::{McpServer, StoreEngine};

pub mod proxy;

/// Which transport the server should run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Transport {
    Stdio,
    Http { host: String, port: u16 },
}

/// Parse the transport from a flat argv (`--transport`, `--host`, `--port`).
/// Unknown flags are ignored (env still drives engine selection). Defaults to
/// stdio; `--transport http` without a port falls back to the config default
/// SSE port (7464).
fn parse_transport<I: IntoIterator<Item = String>>(args: I) -> Transport {
    let mut transport = "stdio".to_string();
    let mut host = "127.0.0.1".to_string();
    let mut port: Option<u16> = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--transport" => {
                if let Some(v) = it.next() {
                    transport = v;
                }
            }
            "--host" => {
                if let Some(v) = it.next() {
                    host = v;
                }
            }
            "--port" => {
                port = it.next().and_then(|v| v.parse::<u16>().ok());
            }
            // `--transport=http` / `--port=1234` long-opt forms.
            other if other.starts_with("--transport=") => {
                transport = other["--transport=".len()..].to_string();
            }
            other if other.starts_with("--host=") => {
                host = other["--host=".len()..].to_string();
            }
            other if other.starts_with("--port=") => {
                port = other["--port=".len()..].parse::<u16>().ok();
            }
            _ => {}
        }
    }

    if transport.eq_ignore_ascii_case("http") {
        Transport::Http {
            host,
            port: port.unwrap_or(cognis_core::Config::default().mcp.sse_port),
        }
    } else {
        Transport::Stdio
    }
}

/// Build the engine from the environment (fixture vs. real UCKG).
fn build_engine() -> std::result::Result<StoreEngine, ExitCode> {
    let use_fixture = std::env::var("COGNIS_MCP_FIXTURE").as_deref() == Ok("1");
    let db_path = std::env::var("COGNIS_DB_PATH").unwrap_or_default();

    if !use_fixture && !db_path.trim().is_empty() {
        // Resolve the semantic warm policy at the daemon entry point so the
        // extension's `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP` signal is actually
        // consumed (Requirement 2.4; bug facet
        // `semanticWarmPolicyIsIgnoredOrInconsistent`). Eager builds the
        // embedder up front (legacy behavior / direct launch); Lazy defers it
        // to first demand so zero ONNX is resident at startup.
        let policy = SemanticWarmPolicy::from_env();
        StoreEngine::open_with_policy(&db_path, policy).map_err(|err| {
            eprintln!("cognis-mcpd: failed to open UCKG at {db_path}: {err}");
            ExitCode::FAILURE
        })
    } else {
        StoreEngine::in_memory_fixture().map_err(|err| {
            eprintln!("cognis-mcpd: failed to build fixture UCKG: {err}");
            ExitCode::FAILURE
        })
    }
}

/// Build the server over `engine`, wiring the audit log from `COGNIS_AUDIT_LOG`
/// when set (else the default `.cognis/audit.log`).
fn build_server(engine: StoreEngine) -> McpServer<StoreEngine> {
    let mut server = McpServer::new(engine);
    if let Ok(path) = std::env::var("COGNIS_AUDIT_LOG") {
        if !path.trim().is_empty() {
            server = server.with_audit(AuditLog::new(path));
        }
    }
    server
}

/// Best-effort repository-scoped MCP lease acquisition.
///
/// Returns `Some(guard)` when this process becomes the owner (heartbeat runs
/// until drop). Returns `None` when a live foreign lease already exists (attach
/// path — we do not steal ownership) or when lease I/O fails. Under the default
/// gate-OFF topology multiple editor-owned stdio processes may still run for
/// the same repo (preservation 3.8); only one of them holds the lease record
/// that enables safe orphan reclaim after reload/crash.
fn acquire_mcpd_lease() -> Option<LeaseGuard> {
    let repo_root = resolve_repo_root_from_env();
    match acquire_or_attach(&repo_root, LeaseRole::Mcpd, None) {
        Ok(AcquireOutcome::Acquired(guard)) => Some(guard),
        Ok(AcquireOutcome::Attached { lease, path }) => {
            eprintln!(
                "cognis-mcpd: a live MCP owner already holds this repository \
                 (pid {}, lease {}); attaching without claiming ownership",
                lease.pid,
                path.display()
            );
            None
        }
        Err(err) => {
            eprintln!(
                "cognis-mcpd: warning: could not acquire repository lease: {err}; \
                 continuing without cross-process ownership"
            );
            None
        }
    }
}

/// Standalone-binary entry: run with the process argv.
pub fn run() -> ExitCode {
    run_from(std::env::args_os())
}

/// Run the server, selecting the transport from `args` and the engine from the
/// environment. Reused by the `cognis-mcpd` binary and the `cognis` multi-call
/// binary (which forwards its post-subcommand argv here).
pub fn run_from<I: IntoIterator<Item = OsString>>(args: I) -> ExitCode {
    // argv[0] is the program name; the flags follow.
    let flags: Vec<String> = args
        .into_iter()
        .skip(1)
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

    // Thin-proxy mode short-circuits before any engine / ONNX / DB construction
    // (Requirements 2.8, 2.11). Selected via `--proxy`, `--transport proxy`, or
    // `COGNIS_MCP_PROXY=1`. The compatible stdio path is preserved — the editor
    // still spawns a command-form server block; the block is just a thin proxy
    // (preservation 3.8).
    if proxy::is_proxy_mode(&flags) {
        return proxy::run_proxy(&flags);
    }

    let transport = parse_transport(flags);

    // Acquire (or attach to) the repository-scoped MCP ownership lease so a
    // reloaded extension / next owner can reclaim a live orphan safely instead
    // of guessing from a bare pid (bug facet `repoHasDuplicateHeavyDaemonOrOrphan`;
    // Requirements 2.7, 2.13). Ownership is recorded under `.cognis/mcpd.lease`
    // with an owner nonce + pid + process-start identity + heartbeat/expiry.
    //
    // The one-heavy-daemon-per-repository *sharing* topology is gated OFF by
    // default (Task 7), so we do NOT refuse to serve when a live lease already
    // exists — that would regress the editor-owned stdio path (preservation
    // 3.8). We record ownership best-effort and hold the guard for the process
    // lifetime; when we are the fresh owner it heartbeats in the background and
    // is removed on clean exit. A pre-existing live lease is logged for
    // visibility. Bind the guard so it is not dropped before `serve` returns.
    let _mcpd_lease: Option<LeaseGuard> = acquire_mcpd_lease();

    let engine = match build_engine() {
        Ok(e) => e,
        Err(code) => return code,
    };
    let server = build_server(engine);

    let use_fixture = std::env::var("COGNIS_MCP_FIXTURE").as_deref() == Ok("1")
        || std::env::var("COGNIS_DB_PATH")
            .unwrap_or_default()
            .trim()
            .is_empty();
    let fixture_tag = if use_fixture { " [fixture]" } else { "" };

    match transport {
        Transport::Stdio => {
            eprintln!(
                "cognis-mcpd (Rust) ready [stdio] — {} tools, contract v{}{}",
                cognis_core::MCP_TOOLS.len(),
                cognis_core::CONTRACT_VERSION,
                fixture_tag
            );
            let _ = io::stderr().flush();

            let stdin = io::stdin();
            let stdout = io::stdout();
            let mut reader = stdin.lock();
            let mut writer = stdout.lock();
            if let Err(err) = server.serve(&mut reader, &mut writer) {
                eprintln!("cognis-mcpd: serve loop error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Transport::Http { host, port } => {
            // Isolation (Requirement 2.12 / Tasks 8.1 + 8.2):
            // * Bind is loopback-only by default (`cognis_mcp::http::bind` rejects
            //   non-loopback hosts unless `COGNIS_MCP_ALLOW_REMOTE=1`).
            // * Every HTTP route requires an unguessable scoped credential.
            //   Resolve from env or mint a fresh one so thin proxies / the
            //   endpoint file can present it.
            // * Repository identity + model fingerprint are verified on every
            //   attachment; cross-repo access and fingerprint mismatch refuse
            //   the session.
            let credential = match cognis_mcp::http::RouteCredential::from_env_or_generate() {
                Ok(c) => c,
                Err(err) => {
                    eprintln!(
                        "cognis-mcpd: invalid {}: {err}",
                        cognis_mcp::http::ROUTE_CREDENTIAL_ENV
                    );
                    return ExitCode::FAILURE;
                }
            };
            // Propagate the token into the process env so any child inspection
            // and the endpoint advertisement share the same secret.
            std::env::set_var(cognis_mcp::http::ROUTE_CREDENTIAL_ENV, credential.as_str());

            // Bind first so a port-in-use / policy error is reported before we
            // announce readiness (the extension's readiness check is a TCP
            // connect).
            let listener = match cognis_mcp::http::bind(&host, port) {
                Ok(l) => l,
                Err(err) => {
                    eprintln!("cognis-mcpd: failed to bind {host}:{port}: {err}");
                    return ExitCode::FAILURE;
                }
            };
            // Advertise the loopback endpoint + scoped credential + model
            // fingerprint only after a successful bind so thin proxies can
            // attach without spawning a second heavy and can refuse session
            // reuse across differing fingerprints (Task 7.1 / 8.1 / 8.2).
            // Best-effort: a write failure never blocks serve.
            {
                let repo_root = resolve_repo_root_from_env();
                let url = format!("http://{host}:{port}/mcp");
                let cfg_for_fp = cognis_core::Config::load(&repo_root).unwrap_or_default();
                let fingerprint = cognis_embed::ModelFingerprint::from_env_or_derive(&cfg_for_fp);
                if let Err(err) = proxy::write_endpoint_file_with_fingerprint(
                    &repo_root,
                    &url,
                    Some(credential.as_str()),
                    Some(fingerprint.as_str()),
                ) {
                    eprintln!("cognis-mcpd: warning: could not write endpoint file: {err}");
                }
            }
            eprintln!(
                "cognis-mcpd (Rust) ready [http://{host}:{port}/mcp] — {} tools, contract v{}{} \
                 (route credential required)",
                cognis_core::MCP_TOOLS.len(),
                cognis_core::CONTRACT_VERSION,
                fixture_tag
            );
            let _ = io::stderr().flush();

            let serve_cfg = {
                // Isolation checks (repo identity + model fingerprint) apply to
                // real repository daemons. Fixture/e2e servers skip them so the
                // contract surface stays reachable without a workspace identity
                // (preservation 3.7 / 3.8; Task 8.2 still enforces on real
                // attachments via proxy + real heavy path).
                let mut cfg = cognis_mcp::http::HttpServeConfig::with_credential(credential);
                if !use_fixture {
                    let owner_identity = cognis_core::RepoIdentity::from_env();
                    let cfg_for_fp =
                        cognis_core::Config::load(cognis_core::resolve_repo_root_from_env())
                            .unwrap_or_default();
                    let fingerprint =
                        cognis_embed::ModelFingerprint::from_env_or_derive(&cfg_for_fp);
                    cfg = cfg
                        .with_repo_identity(owner_identity)
                        .with_model_fingerprint(fingerprint);
                }
                cfg
            };
            if let Err(err) = cognis_mcp::http::serve_listener_with(&server, listener, serve_cfg) {
                eprintln!("cognis-mcpd: http serve loop error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_defaults_to_stdio() {
        assert_eq!(parse_transport(Vec::<String>::new()), Transport::Stdio);
        assert_eq!(
            parse_transport(vec!["--transport".into(), "stdio".into()]),
            Transport::Stdio
        );
    }

    #[test]
    fn parses_http_host_and_port() {
        let t = parse_transport(vec![
            "--transport".into(),
            "http".into(),
            "--host".into(),
            "0.0.0.0".into(),
            "--port".into(),
            "8123".into(),
        ]);
        assert_eq!(
            t,
            Transport::Http {
                host: "0.0.0.0".into(),
                port: 8123
            }
        );
    }

    #[test]
    fn parses_long_opt_forms() {
        let t = parse_transport(vec!["--transport=http".into(), "--port=9000".into()]);
        assert_eq!(
            t,
            Transport::Http {
                host: "127.0.0.1".into(),
                port: 9000
            }
        );
    }

    #[test]
    fn http_without_port_uses_config_default() {
        let t = parse_transport(vec!["--transport".into(), "http".into()]);
        assert_eq!(
            t,
            Transport::Http {
                host: "127.0.0.1".into(),
                port: cognis_core::Config::default().mcp.sse_port
            }
        );
    }
}
