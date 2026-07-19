//! `McpServer` — JSON-RPC 2.0 dispatch over the read-only retrieval engine.
//!
//! Ties the transport ([`jsonrpc`](crate::jsonrpc)), the hard caps
//! ([`caps`](crate::caps)), the audit log ([`audit`](crate::audit)) and the
//! contract-shaped [`tools`](crate::tools) into one read-only server. It speaks
//! the subset of MCP the extension uses over newline-delimited stdio JSON-RPC:
//!
//! * `initialize` — capability + server-info handshake (advertises
//!   [`CONTRACT_VERSION`](crate::CONTRACT_VERSION)).
//! * `notifications/initialized` (and other notifications) — accepted silently.
//! * `tools/list` — advertises **exactly** the 8 tools in
//!   [`MCP_TOOLS`](crate::MCP_TOOLS), in contract order (P-CON-TOOLS).
//! * `tools/call` — dispatches to a [`tools`](crate::tools) handler, applies the
//!   concurrency cap + audit, and wraps the contract-shaped payload (or the
//!   error envelope) in an MCP `CallToolResult`.
//! * `ping` — liveness.
//!
//! The server is **read-only**: it is generic over the
//! [`RetrievalEngine`](crate::engine::RetrievalEngine) seam, which exposes no
//! writer. Every tool failure is rendered as the stable
//! `{error:{code,message,retryable}}` envelope inside a *successful* tool
//! result, never as a JSON-RPC protocol error (Python/fastmcp parity).
//!
//! ## Isolation for shared routes (Requirement 2.12 / Task 8.1)
//!
//! When the same dispatch surface is exposed over HTTP (see [`crate::http`]),
//! the transport layer enforces:
//! * **Loopback-only bind by default** — non-loopback hosts are rejected unless
//!   the operator opts in (`COGNIS_MCP_ALLOW_REMOTE=1` /
//!   [`crate::http::BindOptions::allow_non_loopback`]).
//! * **Unguessable scoped credential per route** — every HTTP POST must present
//!   a [`crate::http::RouteCredential`] (`Authorization: Bearer …` or
//!   `X-Cognis-Route-Token`). Stdio remains process-scoped (the OS already
//!   isolates the pipe), so no extra credential is required on that transport.
//!
//! Repository-identity and model-fingerprint verification on attachment are
//! Task 8.2 and live in [`cognis_core::identity`] + [`cognis_embed::fingerprint`]
//! (enforced by the HTTP transport via `HttpServeConfig`). This module
//! re-exports the bind/credential primitives so daemon entry points can depend
//! on a single server-facing surface.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use crate::audit::AuditLog;
use crate::caps::{Caps, ConcurrencyLimiter};
use crate::engine::RetrievalEngine;
use crate::errors::{is_error_envelope, McpError};
use crate::jsonrpc::{
    read_message, write_message, Request, Response, RpcError, INVALID_PARAMS, METHOD_NOT_FOUND,
};
use crate::tools;

// Re-export isolation primitives (Task 8.1) so callers can configure HTTP bind
// policy and route credentials without reaching into `http` internals.
pub use crate::http::{
    is_loopback_host, BindOptions, RouteCredential, ALLOW_REMOTE_ENV, ROUTE_CREDENTIAL_ENV,
    ROUTE_CREDENTIAL_HEADER,
};

/// The MCP protocol version this server implements (echoed at `initialize`).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A read-only MCP server bound to one retrieval engine.
pub struct McpServer<E: RetrievalEngine> {
    engine: E,
    caps: Caps,
    audit: AuditLog,
    concurrency: ConcurrencyLimiter,
}

impl<E: RetrievalEngine> McpServer<E> {
    /// Build a server over `engine` with the default caps + audit log.
    pub fn new(engine: E) -> Self {
        let caps = Caps::default();
        let concurrency = ConcurrencyLimiter::new(caps.max_concurrency);
        McpServer {
            engine,
            caps,
            audit: AuditLog::default(),
            concurrency,
        }
    }

    /// Override the hard caps (also resizes the concurrency limiter).
    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.concurrency = ConcurrencyLimiter::new(caps.max_concurrency);
        self.caps = caps;
        self
    }

    /// Override the audit log sink.
    pub fn with_audit(mut self, audit: AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// Run the blocking read→dispatch→write loop until the peer closes stdin.
    ///
    /// One newline-delimited JSON-RPC message per line. Notifications (no `id`)
    /// produce no response. A malformed line yields a JSON-RPC error response
    /// with a `null` id (it never crashes the loop).
    pub fn serve<R: BufRead, W: Write>(&self, reader: &mut R, writer: &mut W) -> io::Result<()> {
        while let Some(message) = read_message(reader)? {
            let response = match message {
                Ok(req) => self.handle(req),
                Err(rpc) => Some(Response::error(Value::Null, rpc)),
            };
            if let Some(resp) = response {
                write_message(writer, &resp)?;
            }
        }
        Ok(())
    }

    /// Dispatch one request. Returns `None` for notifications (no `id`).
    pub fn handle(&self, req: Request) -> Option<Response> {
        let id = req.id.clone();

        // Notifications carry no id and never get a response.
        if req.is_notification() {
            return None;
        }
        let id = id.unwrap_or(Value::Null);

        match req.method.as_str() {
            "initialize" => Some(Response::success(id, self.initialize_result())),
            "ping" => Some(Response::success(id, json!({}))),
            "tools/list" => Some(Response::success(id, self.tools_list_result())),
            "tools/call" => Some(self.tools_call(id, req.params)),
            other => Some(Response::error(
                id,
                RpcError::new(METHOD_NOT_FOUND, format!("unknown method '{other}'")),
            )),
        }
    }

    /// `initialize` result — protocol + capabilities + server/contract info.
    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "cognis-mcpd",
                "version": env!("CARGO_PKG_VERSION"),
            },
            // Contract metadata the extension can cross-check against its own
            // EXPECTED_CONTRACT_VERSION (Requirement 3.3, lockstep).
            "contractVersion": cognis_core::CONTRACT_VERSION,
        })
    }

    /// `tools/list` result — exactly the 8 contract tools, in contract order.
    fn tools_list_result(&self) -> Value {
        let tools: Vec<Value> = cognis_core::MCP_TOOLS
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "description": tool_description(name),
                    "inputSchema": tool_input_schema(name),
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    /// `tools/call` — admit (concurrency cap), audit, dispatch, wrap.
    fn tools_call(&self, id: Value, params: Option<Value>) -> Response {
        let params = params.unwrap_or(Value::Null);
        let name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None => {
                return Response::error(
                    id,
                    RpcError::new(INVALID_PARAMS, "tools/call requires a 'name'"),
                )
            }
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        // Hard cap: admit at most `max_concurrency` in-flight calls. A saturated
        // server returns a retryable TIMEOUT envelope (not a protocol error).
        let payload = match self.concurrency.acquire(&name) {
            Ok(_slot) => self.dispatch_tool(&name, &arguments),
            Err(e) => e.to_envelope(),
        };

        // Audit: tool name + hashed args + ok flag (never the raw args).
        self.audit
            .record(&name, &arguments, !is_error_envelope(&payload));

        Response::success(id, wrap_tool_result(payload))
    }

    /// Route a tool name to its handler; unknown names → INVALID_ARGUMENT
    /// envelope. A handler `Err` becomes the stable error envelope.
    fn dispatch_tool(&self, name: &str, args: &Value) -> Value {
        let engine: &dyn RetrievalEngine = &self.engine;
        let result = match name {
            "symbol_search" => tools::symbol_search(engine, &self.caps, args),
            "semantic_search" => tools::semantic_search(engine, &self.caps, args),
            "discover_symbols" => tools::discover_symbols(engine, &self.caps, args),
            "diffuse_context" => tools::diffuse_context(engine, &self.caps, args),
            "symbol_lookup" => tools::symbol_lookup(engine, &self.caps, args),
            "dependency_trace" => tools::dependency_trace(engine, &self.caps, args),
            "resolve_symbols" => tools::resolve_symbols(engine, &self.caps, args),
            "retrieve_context_capsule" => tools::retrieve_context_capsule(engine, &self.caps, args),
            other => Err(McpError::invalid_argument(format!(
                "unknown tool '{other}'"
            ))),
        };
        match result {
            Ok(value) => value,
            Err(e) => e.to_envelope(),
        }
    }
}

/// Wrap a tool payload in an MCP `CallToolResult`.
///
/// fastmcp parity: a dict return is exposed verbatim as `structuredContent`; a
/// list is wrapped under `{"result": [...]}`. `content[0].text` always carries
/// the compact JSON so a text-only client can still read the payload.
fn wrap_tool_result(payload: Value) -> Value {
    let structured = if payload.is_object() {
        payload.clone()
    } else {
        json!({ "result": payload })
    };
    json!({
        "content": [{ "type": "text", "text": payload.to_string() }],
        "structuredContent": structured,
        "isError": is_error_envelope(&payload),
    })
}

/// One-line tool description for `tools/list`.
fn tool_description(name: &str) -> &'static str {
    match name {
        "diffuse_context" => {
            "Flagship CSAR diffusion: ranked on-path context with on_path/ppr_score."
        }
        "symbol_lookup" => "Resolve one symbol by id, qualified name, or fuzzy name.",
        "symbol_search" => "Lexical FTS5 search over the code knowledge graph.",
        "discover_symbols" => "Hybrid lexical + semantic discovery, RRF-fused.",
        "semantic_search" => "Semantic vector KNN search over symbol embeddings.",
        "resolve_symbols" => "Batch-hydrate symbol ids into full records.",
        "dependency_trace" => "Trace caller/callee dependencies over the call graph.",
        "retrieve_context_capsule" => "Compose a token-budgeted Context Capsule for a task.",
        _ => "cognis MCP tool.",
    }
}

/// Minimal JSON Schema for a tool's arguments (`tools/list`).
fn tool_input_schema(name: &str) -> Value {
    let string = json!({ "type": "string" });
    let integer = json!({ "type": "integer" });
    match name {
        "symbol_lookup" => json!({
            "type": "object",
            "properties": { "name_or_id": string, "kind": { "type": "string" } },
            "required": ["name_or_id"],
        }),
        "resolve_symbols" => json!({
            "type": "object",
            "properties": { "symbol_ids": { "type": "array", "items": string } },
            "required": ["symbol_ids"],
        }),
        "dependency_trace" => json!({
            "type": "object",
            "properties": {
                "symbol_id": string,
                "direction": { "type": "string", "enum": ["out", "in", "both"] },
                "depth": integer,
            },
            "required": ["symbol_id"],
        }),
        "retrieve_context_capsule" => json!({
            "type": "object",
            "properties": { "task": string, "max_tokens": integer },
            "required": ["task"],
        }),
        // The query-shaped tools.
        _ => json!({
            "type": "object",
            "properties": { "query": string, "k": integer },
            "required": ["query"],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_engine::tests::fixture_engine;

    fn server() -> McpServer<crate::store_engine::StoreEngine> {
        McpServer::new(fixture_engine())
    }

    fn call(
        server: &McpServer<crate::store_engine::StoreEngine>,
        name: &str,
        args: Value,
    ) -> Value {
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": args })),
        };
        let resp = server.handle(req).expect("response");
        // The payload is the compact JSON in content[0].text.
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn tools_list_advertises_exactly_the_eight() {
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        };
        let resp = server().handle(req).unwrap();
        let tools = resp.result.unwrap();
        let names: Vec<String> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 8);
        for t in cognis_core::MCP_TOOLS {
            assert!(names.contains(&t.to_string()), "missing {t}");
        }
    }

    #[test]
    fn initialize_reports_contract_version() {
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = server().handle(req).unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["contractVersion"], cognis_core::CONTRACT_VERSION);
    }

    #[test]
    fn notification_gets_no_response() {
        let req = Request {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(server().handle(req).is_none());
    }

    #[test]
    fn symbol_search_hit_has_contract_keys() {
        let s = server();
        let hits = call(
            &s,
            "symbol_search",
            json!({ "query": "authenticate", "k": 5 }),
        );
        let arr = hits.as_array().expect("list");
        assert!(!arr.is_empty(), "expected a hit for 'authenticate'");
        for key in [
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
        ] {
            assert!(arr[0].get(key).is_some(), "missing key {key}");
        }
    }

    #[test]
    fn diffuse_context_hit_has_on_path_and_ppr_score() {
        let s = server();
        let hits = call(
            &s,
            "diffuse_context",
            json!({ "query": "authenticate", "k": 10 }),
        );
        let arr = hits.as_array().expect("list");
        assert!(!arr.is_empty(), "expected diffused hits");
        for key in ["symbol_id", "on_path", "ppr_score", "match_sources"] {
            assert!(arr[0].get(key).is_some(), "missing key {key}");
        }
        assert!(arr[0]["on_path"].is_boolean());
        assert!(arr[0]["ppr_score"].is_number());
    }

    #[test]
    fn lookup_miss_returns_error_envelope() {
        let s = server();
        let payload = call(
            &s,
            "symbol_lookup",
            json!({ "name_or_id": "no_such_xyz_123" }),
        );
        assert!(is_error_envelope(&payload));
        for key in ["code", "message", "retryable"] {
            assert!(payload["error"].get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn capsule_has_schema_keys_and_respects_budget() {
        let s = server();
        let cap = call(
            &s,
            "retrieve_context_capsule",
            json!({ "task": "authenticate the user", "max_tokens": 2000 }),
        );
        for key in [
            "goal",
            "task_mode",
            "confidence",
            "relevant_symbols",
            "compressed_context",
            "sources",
            "token_estimate",
            "version",
        ] {
            assert!(cap.get(key).is_some(), "missing key {key}");
        }
        assert_eq!(cap["goal"], "authenticate the user");
        assert!(cap["token_estimate"].as_u64().unwrap() <= 2000);
    }
}
