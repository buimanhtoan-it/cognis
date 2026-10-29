//! `McpServer` — JSON-RPC 2.0 dispatch over the read-only retrieval & analysis engine.

use std::cell::Cell;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::audit::AuditLog;
use crate::caps::{Caps, ConcurrencyLimiter, Deadline, StageDecision};
use crate::engine::RetrievalEngine;
use crate::errors::{is_error_envelope, McpError, INDEX_NOT_READY};
use crate::jsonrpc::{
    read_message, write_message, Request, Response, RpcError, INVALID_PARAMS, METHOD_NOT_FOUND,
};
use crate::tools;
use cognis_store::GenerationSnapshot;

pub use crate::http::{
    is_loopback_host, BindOptions, RouteCredential, ALLOW_REMOTE_ENV, ROUTE_CREDENTIAL_ENV,
    ROUTE_CREDENTIAL_HEADER,
};

/// The MCP protocol version this server implements (echoed at `initialize`).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Cooperative cancellation flag for one admitted tool call.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// One pinned tool-call context shared by every retrieval stage.
pub struct ToolCtx<'a> {
    pub snapshot: GenerationSnapshot<'a>,
    pub pinned: i64,
    pub deadline: Deadline,
    pub cancel: CancelToken,
    pub caps: &'a Caps,
    /// Whether this call's payload included any vector-derived result.
    semantic_included: Cell<bool>,
    /// Soft-deadline truncation: further stages were not started.
    truncated: Cell<bool>,
    /// Extra elapsed time attributed by retrieval stages (tests + cooperative
    /// stage costs) so the gate observes work that does not have to sleep.
    simulated_elapsed: Cell<Duration>,
}

impl<'a> ToolCtx<'a> {
    pub fn new(snapshot: GenerationSnapshot<'a>, deadline: Deadline, caps: &'a Caps) -> Self {
        Self::with_cancel(snapshot, deadline, caps, CancelToken::new())
    }

    pub fn with_cancel(
        snapshot: GenerationSnapshot<'a>,
        deadline: Deadline,
        caps: &'a Caps,
        cancel: CancelToken,
    ) -> Self {
        let pinned = snapshot.source_generation();
        Self {
            snapshot,
            pinned,
            deadline,
            cancel,
            caps,
            semantic_included: Cell::new(false),
            truncated: Cell::new(false),
            simulated_elapsed: Cell::new(Duration::ZERO),
        }
    }

    /// Record that a vector-derived hit was included in this call's result.
    pub fn note_semantic_included(&self) {
        self.semantic_included.set(true);
    }

    /// Whether any vector-derived result was included (Requirement 15.5).
    pub fn semantic_included(&self) -> bool {
        self.semantic_included.get()
    }

    /// Whether the soft deadline stopped later retrieval stages (Requirement 19.3).
    pub fn truncated(&self) -> bool {
        self.truncated.get()
    }

    /// Attribute additional elapsed time to this call's deadline (Requirement 19.2).
    pub fn add_elapsed(&self, extra: Duration) {
        self.simulated_elapsed
            .set(self.simulated_elapsed.get().saturating_add(extra));
    }

    fn observed_elapsed(&self) -> Duration {
        self.deadline
            .elapsed()
            .saturating_add(self.simulated_elapsed.get())
    }

    /// Between-stage gate. `Continue` while work may proceed, `Truncate` once
    /// the soft budget is spent, and a retryable timeout envelope on hard
    /// deadline or cancellation.
    pub fn stage_gate(&self, tool: &str) -> Result<StageDecision, McpError> {
        e2e_stage_sleep();
        if self.cancel.is_cancelled() {
            return Err(McpError::timeout(format!(
                "Tool '{tool}' was cancelled before completion"
            )));
        }
        let elapsed = self.observed_elapsed();
        if elapsed >= self.deadline.hard_limit() {
            return Err(McpError::timeout(format!(
                "Tool '{tool}' exceeded hard wall time ({:.1}s > {:.1}s)",
                elapsed.as_secs_f64(),
                self.deadline.hard_limit().as_secs_f64()
            )));
        }
        if elapsed >= self.deadline.soft_limit() {
            self.truncated.set(true);
            return Ok(StageDecision::Truncate);
        }
        Ok(StageDecision::Continue)
    }
}

/// Optional E2E overlap hook (edit-during-call / disconnect-mid-call).
fn e2e_stage_sleep() {
    let Ok(raw) = std::env::var("COGNIS_E2E_STAGE_SLEEP_MS") else {
        return;
    };
    let Ok(ms) = raw.trim().parse::<u64>() else {
        return;
    };
    if ms == 0 {
        return;
    }
    std::thread::sleep(Duration::from_millis(ms.min(5_000)));
}

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
        let caps = Caps::from_env();
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

    /// Current in-flight tool-call count (Requirement 19.8).
    pub fn in_flight_calls(&self) -> usize {
        self.concurrency.in_flight()
    }

    #[cfg(test)]
    pub(crate) fn hold_concurrency_slot(&self) -> crate::caps::ConcurrencySlot<'_> {
        self.concurrency
            .acquire("test-hold")
            .expect("test hold should admit under a positive cap")
    }

    /// Run the blocking read→dispatch→write loop until the peer closes stdin.
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
        self.handle_with_cancel(req, CancelToken::new())
    }

    /// Dispatch one request with a caller-owned cancel token so a connection
    /// reader can trip cooperative cancellation on client disconnect.
    pub fn handle_with_cancel(&self, req: Request, cancel: CancelToken) -> Option<Response> {
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
            "tools/call" => Some(self.tools_call(id, req.params, cancel)),
            "resources/list" => Some(Response::success(id, self.resources_list_result())),
            "resources/read" => Some(self.resources_read(id, req.params, cancel)),
            other => Some(Response::error(
                id,
                RpcError::new(METHOD_NOT_FOUND, format!("unknown method '{other}'")),
            )),
        }
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": {
                "name": "cognis-mcpd",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "contractVersion": cognis_core::CONTRACT_VERSION,
        })
    }

    fn resources_list_result(&self) -> Value {
        json!({
            "resources": [
                {
                    "uri": "cognis://live/guardrails",
                    "name": "Live Algorithmic Complexity & Impact Guardrails",
                    "description": "Real-time workspace complexity hotspots, optimization advisories, and blast radius summary.",
                    "mimeType": "application/json",
                }
            ]
        })
    }

    fn resources_read(&self, id: Value, params: Option<Value>, cancel: CancelToken) -> Response {
        let uri = params
            .as_ref()
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
            .unwrap_or("cognis://live/guardrails");
        let deadline = Deadline::start(&self.caps);
        let text = match self.engine.open_snapshot() {
            Ok(snapshot) => {
                let ctx = ToolCtx::with_cancel(snapshot, deadline, &self.caps, cancel);
                let val = self.dispatch_tool(&ctx, "analyze_patch_impact", &json!({}));
                val.to_string()
            }
            Err(e) => json!({ "error": e.to_string() }).to_string(),
        };
        Response::success(
            id,
            json!({
                "contents": [
                    {
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": text,
                    }
                ]
            }),
        )
    }

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

    /// `tools/call` — admit (concurrency cap) first, then start a deadline.
    fn tools_call(&self, id: Value, params: Option<Value>, cancel: CancelToken) -> Response {
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

        let (payload, freshness, truncated) = match self.concurrency.acquire(&name) {
            Ok(_slot) => match self.engine.missing_required_indexes() {
                Ok(missing) if !missing.is_empty() => {
                    let message = format!("missing required index: {}", missing.join(", "));
                    (
                        McpError::with_retryable(INDEX_NOT_READY, message, false).to_envelope(),
                        None,
                        false,
                    )
                }
                Err(error) => (
                    McpError::internal(format!("failed to verify required indexes: {error}"))
                        .to_envelope(),
                    None,
                    false,
                ),
                Ok(_) => {
                    let deadline = Deadline::start(&self.caps);
                    match self.engine.open_snapshot() {
                        Ok(snapshot) => {
                            let ctx = ToolCtx::with_cancel(snapshot, deadline, &self.caps, cancel);
                            let payload = self.dispatch_tool(&ctx, &name, &arguments);
                            let truncated = ctx.truncated();
                            (payload, Some(FreshnessMetadata::from_ctx(&ctx)), truncated)
                        }
                        Err(error) => (
                            McpError::internal(format!(
                                "failed to pin generation snapshot: {error}"
                            ))
                            .to_envelope(),
                            None,
                            false,
                        ),
                    }
                }
            },
            Err(e) => (e.to_envelope(), None, false),
        };

        self.audit
            .record(&name, &arguments, !is_error_envelope(&payload));

        Response::success(id, wrap_tool_result(payload, freshness, truncated))
    }

    fn dispatch_tool(&self, ctx: &ToolCtx<'_>, name: &str, args: &Value) -> Value {
        let engine: &dyn RetrievalEngine = &self.engine;
        let result = match name {
            "symbol_search" => tools::symbol_search(engine, ctx, args),
            "semantic_search" => tools::semantic_search(engine, ctx, args),
            "discover_symbols" => tools::discover_symbols(engine, ctx, args),
            "diffuse_context" => tools::diffuse_context(engine, ctx, args),
            "symbol_lookup" => tools::symbol_lookup(engine, ctx, args),
            "dependency_trace" => tools::dependency_trace(engine, ctx, args),
            "resolve_symbols" => tools::resolve_symbols(engine, ctx, args),
            "retrieve_context_capsule" => tools::retrieve_context_capsule(engine, ctx, args),
            "analyze_patch_impact" => tools::analyze_patch_impact(engine, ctx, args),
            "inspect_symbol_complexity" => tools::inspect_symbol_complexity(engine, ctx, args),
            "trace_blast_radius" => tools::trace_blast_radius(engine, ctx, args),
            "suggest_optimal_refactor" => tools::suggest_optimal_refactor(engine, ctx, args),
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

struct FreshnessMetadata {
    source_generation: i64,
    semantic_generation: i64,
    pending_vector_count: i64,
    semantic_available: bool,
}

impl FreshnessMetadata {
    fn from_ctx(ctx: &ToolCtx<'_>) -> Self {
        Self {
            source_generation: ctx.pinned,
            semantic_generation: ctx.snapshot.semantic_generation(),
            pending_vector_count: ctx.snapshot.pending_vector_count(),
            semantic_available: ctx.semantic_included(),
        }
    }

    fn attach(&self, structured: &mut Value) {
        if let Some(object) = structured.as_object_mut() {
            object.insert("source_generation".into(), json!(self.source_generation));
            object.insert(
                "semantic_generation".into(),
                json!(self.semantic_generation),
            );
            object.insert(
                "pending_vector_count".into(),
                json!(self.pending_vector_count),
            );
            object.insert("semantic_available".into(), json!(self.semantic_available));
        }
    }
}

/// Attach `Freshness_Metadata` as sibling keys (Requirement 15, 22.3).
fn wrap_tool_result(
    payload: Value,
    freshness: Option<FreshnessMetadata>,
    truncated: bool,
) -> Value {
    let mut structured = if payload.is_object() {
        payload.clone()
    } else {
        json!({ "result": payload })
    };
    if let Some(freshness) = freshness {
        freshness.attach(&mut structured);
    }
    if truncated && !is_error_envelope(&payload) {
        if let Some(object) = structured.as_object_mut() {
            object.insert("truncated".into(), json!(true));
        }
    }
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": is_error_envelope(&payload),
    })
}

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
        "analyze_patch_impact" => {
            "Real-time guardrail (<15ms): evaluate unsaved buffers/diffs or modified files for inter-procedural Big-O complexity, CSIA blast radius, contract breakages, and optimal refactorings."
        }
        "inspect_symbol_complexity" => {
            "Audit intra- and inter-procedural time/space complexity (Big-O), recursion SCCs, I/O-in-loop bottlenecks, and blame chains."
        }
        "trace_blast_radius" => {
            "CSIA reverse forward-push impact analysis: quantify blast radius, loop-amplified complexity regressions, affected entrypoints, and minimal test suite."
        }
        "suggest_optimal_refactor" => {
            "Prescribe concrete algorithmic and data-structure optimizations (HashMap indexing, batch queries, loop hoisting, memoization) grounded in existing repository symbols."
        }
        _ => "cognis MCP tool.",
    }
}

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
        "analyze_patch_impact" => json!({
            "type": "object",
            "properties": {
                "file_path": string,
                "new_content": string,
                "proposed_content": string,
                "file_edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file_path": string,
                            "new_content": string,
                        },
                        "required": ["file_path", "new_content"],
                    }
                },
                "targets": { "type": "array", "items": string },
            },
        }),
        "inspect_symbol_complexity" => json!({
            "type": "object",
            "properties": {
                "name_or_id": string,
                "symbol_ids": { "type": "array", "items": string },
                "k": integer,
            },
        }),
        "trace_blast_radius" => json!({
            "type": "object",
            "properties": {
                "symbol_id": string,
                "signature_changed": { "type": "boolean" },
                "new_complexity": string,
            },
            "required": ["symbol_id"],
        }),
        "suggest_optimal_refactor" => json!({
            "type": "object",
            "properties": {
                "target": string,
                "code_snippet": string,
                "file_path": string,
            },
        }),
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
    use std::time::Duration;

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
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn tools_list_advertises_all_contract_tools() {
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
        let expected: Vec<String> = cognis_core::MCP_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(names, expected, "tools/list must preserve contract order");
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

    const FRESHNESS_KEYS: [&str; 4] = [
        "source_generation",
        "semantic_generation",
        "pending_vector_count",
        "semantic_available",
    ];

    fn freshness_fixture() -> FreshnessMetadata {
        FreshnessMetadata {
            source_generation: 42,
            semantic_generation: 41,
            pending_vector_count: 3,
            semantic_available: true,
        }
    }

    fn assert_freshness(payload: &Value, expected: &FreshnessMetadata) {
        assert_eq!(
            payload["source_generation"].as_i64(),
            Some(expected.source_generation)
        );
        assert_eq!(
            payload["semantic_generation"].as_i64(),
            Some(expected.semantic_generation)
        );
        assert_eq!(
            payload["pending_vector_count"].as_i64(),
            Some(expected.pending_vector_count)
        );
        assert_eq!(
            payload["semantic_available"].as_bool(),
            Some(expected.semantic_available)
        );
    }

    #[test]
    fn wrap_tool_result_adds_freshness_as_object_siblings() {
        let freshness = freshness_fixture();
        let wrapped = wrap_tool_result(
            json!({ "id": "sym:1", "name": "authenticate" }),
            Some(freshness_fixture()),
            false,
        );
        let structured = &wrapped["structuredContent"];
        assert_eq!(structured["id"], "sym:1");
        assert_eq!(structured["name"], "authenticate");
        assert!(structured.get("result").is_none());
        assert_freshness(structured, &freshness);
        assert_eq!(
            wrapped["content"][0]["text"].as_str().unwrap(),
            structured.to_string()
        );
    }

    #[test]
    fn wrap_tool_result_wraps_arrays_under_result_and_adds_freshness_siblings() {
        let freshness = freshness_fixture();
        let wrapped = wrap_tool_result(
            json!([{ "symbol_id": "sym:1", "score": 1.0 }]),
            Some(freshness_fixture()),
            false,
        );
        let structured = &wrapped["structuredContent"];
        assert_eq!(structured["result"][0]["symbol_id"], "sym:1");
        assert_eq!(structured["result"][0]["score"], 1.0);
        assert_freshness(structured, &freshness);
        assert_eq!(
            wrapped["content"][0]["text"].as_str().unwrap(),
            structured.to_string()
        );
    }

    #[test]
    fn tools_call_attaches_freshness_to_object_and_array_results() {
        let s = server();
        let lookup = call(&s, "symbol_lookup", json!({ "name_or_id": "authenticate" }));
        assert!(lookup.get("result").is_none(), "object tool must not wrap");
        assert!(lookup.get("id").is_some() || lookup.get("error").is_some());
        for key in FRESHNESS_KEYS {
            assert!(lookup.get(key).is_some(), "object tool missing {key}");
        }

        let search = call(
            &s,
            "symbol_search",
            json!({ "query": "authenticate", "k": 5 }),
        );
        assert!(search["result"].is_array());
        for key in FRESHNESS_KEYS {
            assert!(search.get(key).is_some(), "array tool missing {key}");
        }
    }

    #[test]
    fn symbol_search_hit_has_contract_keys() {
        let s = server();
        let hits = call(
            &s,
            "symbol_search",
            json!({ "query": "authenticate", "k": 5 }),
        );
        let arr = hits["result"].as_array().expect("list");
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
        let arr = hits["result"].as_array().expect("list");
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

    #[test]
    fn flagship_guardrail_and_impact_tools_execute_in_real_time() {
        let s = server();
        // 1. analyze_patch_impact with an in-memory shadow buffer introducing an O(n^2) scan
        let patch = call(
            &s,
            "analyze_patch_impact",
            json!({
                "file_path": "src/auth.py",
                "new_content": "def authenticate(users, ids):\n    for uid in ids:\n        if uid in users:\n            verify_credentials(uid)\n\ndef verify_credentials(uid):\n    return hash_password(uid)\n"
            }),
        );
        assert_eq!(patch["verdict"], "WARN_COMPLEXITY_REGRESSION");
        assert!(!patch["complexity_reports"].as_array().unwrap().is_empty());
        assert_eq!(patch["complexity_reports"][0]["propagated_time"], "O(n^2)");
        assert!(!patch["advisories"].as_array().unwrap().is_empty());

        // 2. trace_blast_radius from `verify_credentials`
        let blast = call(
            &s,
            "trace_blast_radius",
            json!({
                "symbol_id": "verify_credentials",
                "signature_changed": true,
                "new_complexity": "O(n)"
            }),
        );
        assert!(blast.get("impact_level").is_some());
        assert!(!blast["breaking_call_sites"].as_array().unwrap().is_empty());

        // 3. inspect_symbol_complexity
        let inspect = call(
            &s,
            "inspect_symbol_complexity",
            json!({ "name_or_id": "authenticate" }),
        );
        assert!(inspect["reports"].is_array());

        // 4. suggest_optimal_refactor on a snippet
        let refactor = call(
            &s,
            "suggest_optimal_refactor",
            json!({
                "file_path": "src/orders.ts",
                "code_snippet": "function matchOrders(orders: any[], users: any[]) {\n  for (const o of orders) {\n    const u = users.find(x => x.id === o.uid);\n  }\n}\n"
            }),
        );
        assert_eq!(refactor["current_complexity"], "O(n^2)");
        assert_eq!(refactor["target_complexity"], "O(n)");
        assert!(!refactor["advisories"].as_array().unwrap().is_empty());
    }

    #[test]
    fn capacity_rejection_is_retryable_and_starts_no_deadline() {
        let s = McpServer::new(fixture_engine()).with_caps(Caps {
            max_concurrency: 1,
            ..Caps::default()
        });
        let _hold = s.hold_concurrency_slot();
        let payload = call(
            &s,
            "symbol_search",
            json!({ "query": "authenticate", "k": 5 }),
        );
        assert!(is_error_envelope(&payload));
        assert_eq!(payload["error"]["retryable"], true);
        assert!(payload.get("truncated").is_none());
        assert!(payload.get("source_generation").is_none());
    }

    #[test]
    fn soft_deadline_returns_partial_results_with_truncated() {
        let s = McpServer::new(fixture_engine()).with_caps(Caps {
            soft_timeout: Duration::from_secs(0),
            hard_timeout: Duration::from_secs(10),
            ..Caps::default()
        });
        let payload = call(
            &s,
            "retrieve_context_capsule",
            json!({ "task": "authenticate the user", "max_tokens": 2000 }),
        );
        assert!(!is_error_envelope(&payload));
        assert_eq!(payload["truncated"], true);
        assert!(payload.get("source_generation").is_some());
        assert!(payload["relevant_symbols"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hard_deadline_returns_timeout_envelope() {
        let s = McpServer::new(fixture_engine()).with_caps(Caps {
            soft_timeout: Duration::from_secs(0),
            hard_timeout: Duration::from_secs(0),
            ..Caps::default()
        });
        let payload = call(
            &s,
            "symbol_search",
            json!({ "query": "authenticate", "k": 5 }),
        );
        assert!(is_error_envelope(&payload));
        assert_eq!(payload["error"]["retryable"], true);
        assert!(payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hard wall time"));
    }

    #[test]
    fn cancel_token_unwinds_at_the_next_gate() {
        let engine = fixture_engine();
        let caps = Caps::default();
        let snapshot = engine.open_snapshot().unwrap();
        let cancel = CancelToken::new();
        let ctx = ToolCtx::with_cancel(snapshot, Deadline::start(&caps), &caps, cancel.clone());
        cancel.cancel();
        let err = ctx.stage_gate("symbol_search").unwrap_err();
        assert!(err.is_retryable());
        assert!(err.message.contains("cancelled"));
    }

    fn engine_with_dropped_index(name: &str) -> crate::store_engine::StoreEngine {
        use cognis_store::Database;
        let db = Database::new(":memory:");
        db.connect()
            .unwrap()
            .execute(&format!("DROP INDEX {name}"), [])
            .unwrap();
        crate::store_engine::StoreEngine::new(db)
    }

    #[test]
    fn missing_required_index_refuses_tools_call_naming_the_index() {
        let s = McpServer::new(engine_with_dropped_index("idx_symbol_name_lower"));
        let payload = call(&s, "symbol_lookup", json!({ "name_or_id": "authenticate" }));
        assert!(is_error_envelope(&payload));
        assert_eq!(payload["error"]["code"], "INDEX_NOT_READY");
        assert_eq!(payload["error"]["retryable"], false);
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap()
                .contains("idx_symbol_name_lower"),
            "refusal must name the missing index: {}",
            payload["error"]["message"]
        );
        assert!(payload.get("source_generation").is_none());
    }
}
