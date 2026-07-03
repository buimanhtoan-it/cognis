//! HTTP-transport e2e — against a **live Rust server** over a real socket.
//!
//! Proves the `--transport http` path the panel-managed "Start MCP server" flow
//! uses actually binds and serves the JSON-RPC contract: the daemon is spawned
//! as a separate process bound to a localhost port, and driven with raw HTTP
//! POSTs (no HTTP-client dependency) exactly as an MCP-over-HTTP host would.
//!
//! This is the regression guard for the go-live gap where `--transport http`
//! was ignored (the server ran stdio and never bound, so the editor's HTTP
//! connect timed out).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A live `cognis-mcpd` HTTP server bound to an ephemeral localhost port.
struct HttpServer {
    child: Child,
    port: u16,
}

impl HttpServer {
    fn start() -> Self {
        let port = free_port();
        let audit = std::env::temp_dir().join(format!(
            "cognis-mcp-http-{}-{}.log",
            std::process::id(),
            port
        ));
        let child = Command::new(env!("CARGO_BIN_EXE_cognis-mcpd"))
            .args([
                "--transport",
                "http",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("COGNIS_MCP_FIXTURE", "1")
            .env("COGNIS_AUDIT_LOG", &audit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cognis-mcpd --transport http");
        let server = HttpServer { child, port };
        server.wait_until_ready();
        server
    }

    /// Poll the port until it accepts a connection (server bound) or we give up.
    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("cognis-mcpd http server never bound on port {}", self.port);
    }

    /// POST a JSON-RPC body to /mcp and return the decoded JSON response.
    fn post(&self, body: Value) -> Value {
        let payload = body.to_string();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to http server");
        stream.write_all(request.as_bytes()).expect("write request");
        stream.flush().expect("flush");

        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read response");
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .expect("response has header/body separator");
        assert!(
            head.starts_with("HTTP/1.1 200"),
            "expected 200 OK, got head: {head}"
        );
        serde_json::from_str(body.trim()).expect("parse json body")
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Grab a free localhost port by binding to :0 and immediately releasing it.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

#[test]
fn http_transport_serves_initialize_and_tools_over_a_real_socket() {
    let server = HttpServer::start();

    // initialize → contract version lockstep, exactly like stdio.
    let init = server.post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
    assert_eq!(
        init["result"]["contractVersion"],
        cognis_core::CONTRACT_VERSION
    );

    // tools/list → exactly the 8 contract tools.
    let list = server.post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 8, "expected 8 tools, got {names:?}");
    for tool in cognis_core::MCP_TOOLS {
        assert!(names.iter().any(|n| n == tool), "missing tool {tool}");
    }

    // tools/call over HTTP returns the same wrapped result shape as stdio.
    let call = server.post(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "symbol_search", "arguments": { "query": "authenticate", "k": 5 } }
    }));
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    let hits: Value = serde_json::from_str(text).expect("parse tool payload");
    assert!(hits.as_array().map(|a| !a.is_empty()).unwrap_or(false));
}

#[test]
fn http_get_is_declined_without_crashing_the_server() {
    let server = HttpServer::start();

    // A GET (SSE upgrade probe) is declined 405, and the server stays up for a
    // subsequent POST — one bad request must not take the loop down.
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .write_all(b"GET /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 405"), "expected 405, got: {raw}");

    // Server still serves after the declined GET.
    let ping = server.post(json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}));
    assert!(ping["result"].is_object());
}
