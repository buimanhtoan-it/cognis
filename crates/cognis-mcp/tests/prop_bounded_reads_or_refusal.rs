//! Property 13: Bounded reads or refusal.
//!
//! A database missing any required index yields a refusal naming the missing
//! index, never a fallback to an unbounded scan.
//!
//! **Validates: Requirements 9.7**

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cognis_core::{Hit, Result as CognisResult, Symbol, MCP_TOOLS};
use cognis_mcp::caps::{Caps, Deadline};
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::server::ToolCtx;
use cognis_mcp::{is_error_envelope, McpServer, RetrievalEngine, StoreEngine};
use cognis_store::{Database, GenerationSnapshot, REQUIRED_INDEXES};
use proptest::prelude::*;
use serde_json::{json, Value};

fn tool_args(tool: &str) -> Value {
    match tool {
        "symbol_lookup" => json!({ "name_or_id": "alpha" }),
        "resolve_symbols" => json!({ "symbol_ids": ["alpha"] }),
        "dependency_trace" => json!({
            "symbol_id": "alpha",
            "direction": "out",
            "depth": 1,
        }),
        "retrieve_context_capsule" => json!({ "task": "alpha", "max_tokens": 800 }),
        _ => json!({ "query": "alpha", "k": 4 }),
    }
}

fn call_tool(server: &McpServer<ProbeEngine>, name: &str) -> Value {
    let response = server
        .handle(Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": tool_args(name) })),
        })
        .expect("tools/call response");
    response.result.expect("tools/call result")["structuredContent"].clone()
}

fn isolated_db() -> (PathBuf, Database) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p13-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let db = Database::open(dir.join("indexes.db")).expect("open Property 13 database");
    (dir, db)
}

fn cleanup(dir: &Path, db: &Database) {
    db.close_thread_connection();
    let _ = std::fs::remove_dir_all(dir);
}

fn drop_indexes(db: &Database, names: &[&str]) {
    let conn = db.connect().expect("connect to drop indexes");
    for name in names {
        conn.execute(&format!("DROP INDEX IF EXISTS {name}"), [])
            .expect("drop required index");
    }
}

/// Counts retrieval-stage entry so a missing-index refusal cannot silently
/// fall through to a table scan.
struct ProbeEngine {
    inner: StoreEngine,
    retrievals: Arc<AtomicU64>,
}

impl ProbeEngine {
    fn note(&self) {
        self.retrievals.fetch_add(1, Ordering::SeqCst);
    }
}

impl RetrievalEngine for ProbeEngine {
    fn open_snapshot(&self) -> CognisResult<GenerationSnapshot<'_>> {
        self.inner.open_snapshot()
    }

    fn missing_required_indexes(&self) -> CognisResult<Vec<String>> {
        self.inner.missing_required_indexes()
    }

    fn fts_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> CognisResult<Vec<Hit>> {
        self.note();
        self.inner.fts_search(ctx, query, k)
    }

    fn semantic_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> CognisResult<Vec<Hit>> {
        self.note();
        self.inner.semantic_search(ctx, query, k)
    }

    fn diffuse(
        &self,
        ctx: &ToolCtx<'_>,
        seeds: &[Vec<Hit>],
        k: usize,
        alpha: f64,
        eps: f64,
    ) -> CognisResult<Vec<Hit>> {
        self.note();
        self.inner.diffuse(ctx, seeds, k, alpha, eps)
    }

    fn hydrate(&self, ctx: &ToolCtx<'_>, ids: &[String]) -> CognisResult<Vec<Symbol>> {
        self.note();
        self.inner.hydrate(ctx, ids)
    }

    fn lookup(
        &self,
        ctx: &ToolCtx<'_>,
        name_or_id: &str,
        kind: Option<&str>,
    ) -> CognisResult<Option<Symbol>> {
        self.note();
        self.inner.lookup(ctx, name_or_id, kind)
    }

    fn dependency_trace(
        &self,
        ctx: &ToolCtx<'_>,
        symbol_id: &str,
        direction: &str,
        depth: u8,
    ) -> CognisResult<Vec<Hit>> {
        self.note();
        self.inner
            .dependency_trace(ctx, symbol_id, direction, depth)
    }
}

fn arb_dropped_indexes() -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(any::<bool>(), REQUIRED_INDEXES.len()).prop_map(|mask| {
        REQUIRED_INDEXES
            .iter()
            .zip(mask)
            .filter_map(|(name, dropped)| dropped.then_some(*name))
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 13: Bounded reads or refusal.
    /// **Validates: Requirements 9.7**
    #[test]
    fn missing_required_indexes_refuse_queries_by_name(
        dropped in arb_dropped_indexes(),
    ) {
        let (dir, db) = isolated_db();
        drop_indexes(&db, &dropped);
        let expected: Vec<String> = dropped.iter().map(|name| (*name).to_string()).collect();
        let retrievals = Arc::new(AtomicU64::new(0));
        let closer = db.clone();
        let engine = ProbeEngine {
            inner: StoreEngine::new(db),
            retrievals: Arc::clone(&retrievals),
        };
        prop_assert_eq!(
            engine.missing_required_indexes().expect("list missing indexes"),
            expected
        );

        if dropped.is_empty() {
            let caps = Caps::default();
            let snapshot = engine.open_snapshot().expect("fresh database must pin");
            drop(ToolCtx::new(snapshot, Deadline::start(&caps), &caps));
        } else {
            let error = match engine.open_snapshot() {
                Ok(_) => {
                    prop_assert!(
                        false,
                        "must refuse to pin a snapshot when a required index is missing"
                    );
                    String::new()
                }
                Err(error) => error.to_string(),
            };
            for name in &dropped {
                prop_assert!(
                    error.contains(name),
                    "snapshot refusal must name {}: {}",
                    name,
                    error
                );
            }
        }

        let server = McpServer::new(engine);
        for tool in MCP_TOOLS {
            let payload = call_tool(&server, tool);
            if dropped.is_empty() {
                prop_assert_ne!(
                    payload.get("error").and_then(|error| error.get("code")),
                    Some(&json!("INDEX_NOT_READY")),
                    "{} must not refuse a database with every required index",
                    tool
                );
            } else {
                prop_assert!(is_error_envelope(&payload), "{} must refuse", tool);
                prop_assert_eq!(&payload["error"]["code"], "INDEX_NOT_READY");
                prop_assert_eq!(&payload["error"]["retryable"], false);
                let message = payload["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                for name in &dropped {
                    prop_assert!(
                        message.contains(name),
                        "{} refusal must name {}: {}",
                        tool,
                        name,
                        message
                    );
                }
                prop_assert!(
                    payload.get("source_generation").is_none(),
                    "{} must not serve a freshness-wrapped scan",
                    tool
                );
            }
        }
        if !dropped.is_empty() {
            prop_assert_eq!(
                retrievals.load(Ordering::SeqCst),
                0,
                "a missing required index must not fall back to a retrieval scan"
            );
        }
        cleanup(&dir, &closer);
    }
}
