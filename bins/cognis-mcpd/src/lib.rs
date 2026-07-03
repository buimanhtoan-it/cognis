//! cognis-mcpd — MCP server entry point (Task 7).
//!
//! Serves the read-only [`McpServer`] over the 8-tool contract (Requirement 3)
//! on one of two transports:
//!
//! * **stdio** (default) — newline-delimited JSON-RPC on stdin/stdout, the
//!   transport an editor spawns and owns.
//! * **http** — `--transport http --host <h> --port <p>`, a standalone
//!   localhost server the panel-managed "Start MCP server" flow launches and
//!   the editor connects to by URL. Same JSON-RPC surface, same contract.
//!
//! The backing [`StoreEngine`] is chosen from the environment:
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

use cognis_mcp::audit::AuditLog;
use cognis_mcp::{McpServer, StoreEngine};

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
        StoreEngine::open(&db_path).map_err(|err| {
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
    let transport = parse_transport(flags);

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
            // Bind first so a port-in-use error is reported before we announce
            // readiness (the extension's readiness check is a TCP connect).
            let listener = match cognis_mcp::http::bind(&host, port) {
                Ok(l) => l,
                Err(err) => {
                    eprintln!("cognis-mcpd: failed to bind {host}:{port}: {err}");
                    return ExitCode::FAILURE;
                }
            };
            eprintln!(
                "cognis-mcpd (Rust) ready [http://{host}:{port}/mcp] — {} tools, contract v{}{}",
                cognis_core::MCP_TOOLS.len(),
                cognis_core::CONTRACT_VERSION,
                fixture_tag
            );
            let _ = io::stderr().flush();

            if let Err(err) = cognis_mcp::http::serve_listener(&server, listener) {
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
