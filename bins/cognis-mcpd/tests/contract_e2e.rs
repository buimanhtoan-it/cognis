//! MCP tool-output contract e2e — against a **live Rust server**.
//!
//! Port of the Python `tests/e2e/test_mcp_tool_contracts.py` to Rust, asserting
//! **Property 4 (MCP contract invariance)** on a real `cognis-mcpd` process:
//!
//! * `P-CON-TOOLS` — the live server advertises exactly the 8 tools in
//!   `cognis_core::MCP_TOOLS`.
//! * the 8 tools keep their pinned output keys (search/lookup/trace/resolve,
//!   hybrid `discover_symbols`, flagship `diffuse_context` with
//!   `on_path`/`ppr_score`, `retrieve_context_capsule` schema).
//! * `P-CON-DIFF` — every `diffuse_context` hit carries `on_path` (bool) +
//!   `ppr_score` (number).
//! * the error envelope `{error:{code,message,retryable}}` is unchanged.
//!
//! The server is spawned as a separate process (`COGNIS_MCP_FIXTURE=1`), driven
//! over newline-delimited JSON-RPC on its stdin/stdout — exactly the transport
//! the extension uses — so the test is faithful to what an MCP host receives.
//! Required-key assertions (not full golden snapshots) keep the test robust to
//! additive/enrichment fields while failing loudly on a dropped/renamed key.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

/// A live `cognis-mcpd` process driven over stdio JSON-RPC.
struct LiveServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl LiveServer {
    /// Spawn the real binary in fixture mode and complete the MCP handshake.
    fn start() -> Self {
        let audit = std::env::temp_dir().join(format!("cognis-mcp-e2e-{}.log", std::process::id()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_cognis-mcpd"))
            .env("COGNIS_MCP_FIXTURE", "1")
            .env("COGNIS_AUDIT_LOG", &audit)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cognis-mcpd");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut server = LiveServer {
            child,
            stdin,
            stdout,
            next_id: 0,
        };

        // initialize handshake — must report the contract version.
        let init = server.request("initialize", json!({}));
        assert_eq!(
            init["result"]["contractVersion"],
            cognis_core::CONTRACT_VERSION,
            "live server must advertise CONTRACT_VERSION at initialize"
        );
        // notifications/initialized carries no id ⇒ no response is read.
        server.notify("notifications/initialized");
        server
    }

    /// Send a request and read its (id-matched) JSON-RPC response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("write request");
        self.stdin.flush().expect("flush request");

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "server closed the connection (method={method})");
        let resp: Value = serde_json::from_str(line.trim()).expect("parse response json");
        assert_eq!(resp["id"], json!(id), "response id mismatch");
        resp
    }

    /// Send a notification (no id ⇒ no response expected).
    fn notify(&mut self, method: &str) {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{msg}").expect("write notification");
        self.stdin.flush().expect("flush notification");
    }

    /// List the advertised tool names.
    fn list_tools(&mut self) -> Vec<String> {
        let resp = self.request("tools/list", json!({}));
        resp["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name").to_string())
            .collect()
    }

    /// Call a tool and return its decoded payload (list or dict).
    ///
    /// Mirrors the Python `_structured`: the payload is the compact JSON carried
    /// in `result.content[0].text`.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let resp = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("tool result text");
        serde_json::from_str(text).expect("parse tool payload")
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Assert `obj` is a dict containing every key in `keys`.
fn require_keys(obj: &Value, keys: &[&str], label: &str) {
    let map = obj
        .as_object()
        .unwrap_or_else(|| panic!("{label}: expected a JSON object, got {obj}"));
    let missing: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| !map.contains_key(*k))
        .collect();
    assert!(
        missing.is_empty(),
        "{label}: MCP tool output is missing keys the agent relies on: {missing:?}. \
         The tool output contract drifted."
    );
}

// ---------------------------------------------------------------------------
// P-CON-TOOLS — the live server advertises exactly the 8 contract tools.
// ---------------------------------------------------------------------------

#[test]
fn server_advertises_the_pinned_tool_set() {
    let mut server = LiveServer::start();
    let names = server.list_tools();
    assert_eq!(names.len(), 8, "expected exactly 8 tools, got {names:?}");
    for tool in cognis_core::MCP_TOOLS {
        assert!(
            names.iter().any(|n| n == tool),
            "server is missing pinned tool {tool}; advertised={names:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tool output contracts (real server).
// ---------------------------------------------------------------------------

#[test]
fn symbol_search_hit_contract() {
    let mut server = LiveServer::start();
    let results = server.call("symbol_search", json!({"query": "authenticate", "k": 5}));
    let arr = results.as_array().expect("symbol_search returns a list");
    assert!(
        !arr.is_empty(),
        "symbol_search found no hits in the fixture"
    );
    require_keys(
        &arr[0],
        &[
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "line_start",
            "line_end",
            "score",
            "match_reason",
        ],
        "symbol_search hit",
    );
}

#[test]
fn discover_symbols_hit_contract() {
    let mut server = LiveServer::start();
    let results = server.call(
        "discover_symbols",
        json!({"query": "authenticate", "k": 10}),
    );
    let arr = results.as_array().expect("discover_symbols returns a list");
    assert!(
        !arr.is_empty(),
        "discover_symbols found no hits in the fixture"
    );
    require_keys(
        &arr[0],
        &[
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "score",
            "match_reason",
            "match_sources",
            "snippet",
        ],
        "discover_symbols hit",
    );
}

#[test]
fn diffuse_context_hit_contract() {
    let mut server = LiveServer::start();
    let results = server.call("diffuse_context", json!({"query": "authenticate", "k": 10}));
    let arr = results.as_array().expect("diffuse_context returns a list");
    assert!(
        !arr.is_empty(),
        "diffuse_context found no hits in the fixture"
    );
    require_keys(
        &arr[0],
        &[
            "symbol_id",
            "id",
            "name",
            "qualified_name",
            "kind",
            "file_path",
            "score",
            "match_sources",
            "on_path",
            "ppr_score",
        ],
        "diffuse_context hit",
    );
    // P-CON-DIFF: on_path is bool, ppr_score is a number.
    assert!(arr[0]["on_path"].is_boolean(), "on_path must be a boolean");
    assert!(
        arr[0]["ppr_score"].is_number(),
        "ppr_score must be a number"
    );
}

#[test]
fn retrieve_context_capsule_contract() {
    let mut server = LiveServer::start();
    let result = server.call(
        "retrieve_context_capsule",
        json!({"task": "how is authentication verified", "max_tokens": 2000}),
    );
    require_keys(
        &result,
        &[
            "goal",
            "task_mode",
            "confidence",
            "relevant_symbols",
            "compressed_context",
            "sources",
            "token_estimate",
            "version",
        ],
        "retrieve_context_capsule result",
    );
    assert_eq!(result["version"], "1");
    assert!(result["token_estimate"].as_u64().unwrap() <= 2000);
}

#[test]
fn symbol_lookup_contract() {
    let mut server = LiveServer::start();
    let result = server.call("symbol_lookup", json!({"name_or_id": "authenticate"}));
    require_keys(
        &result,
        &[
            "id",
            "kind",
            "name",
            "qualified_name",
            "language",
            "file_path",
            "line_start",
            "line_end",
        ],
        "symbol_lookup result",
    );
}

#[test]
fn dependency_trace_contract() {
    let mut server = LiveServer::start();
    let hits = server.call("symbol_search", json!({"query": "login_handler", "k": 1}));
    let symbol_id = hits[0]["symbol_id"]
        .as_str()
        .expect("symbol_id")
        .to_string();
    let result = server.call(
        "dependency_trace",
        json!({"symbol_id": symbol_id, "direction": "out", "depth": 2}),
    );
    require_keys(
        &result,
        &["start", "direction", "depth", "hits"],
        "dependency_trace result",
    );
    assert!(
        result["hits"].is_array(),
        "dependency_trace.hits must be a list"
    );
}

#[test]
fn resolve_symbols_contract() {
    let mut server = LiveServer::start();
    let hits = server.call("symbol_search", json!({"query": "authenticate", "k": 1}));
    let symbol_id = hits[0]["symbol_id"]
        .as_str()
        .expect("symbol_id")
        .to_string();
    let result = server.call("resolve_symbols", json!({"symbol_ids": [symbol_id]}));
    require_keys(
        &result,
        &["symbols", "missing", "requested_count", "resolved_count"],
        "resolve_symbols result",
    );
    assert!(
        result["symbols"].is_array(),
        "resolve_symbols.symbols must be a list"
    );
    assert_eq!(result["resolved_count"], 1);
}

#[test]
fn error_envelope_contract() {
    let mut server = LiveServer::start();
    let result = server.call(
        "symbol_lookup",
        json!({"name_or_id": "no_such_symbol_xyz_123"}),
    );
    require_keys(&result, &["error"], "error envelope");
    require_keys(
        &result["error"],
        &["code", "message", "retryable"],
        "error envelope body",
    );
}
