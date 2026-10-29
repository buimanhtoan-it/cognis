//! Property 16: Query determinism.
//!
//! Two executions with no intervening commit return identical result identifiers
//! in identical order and report the identical `source_generation`.
//!
//! **Validates: Requirements 13.5**

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cognis_core::MCP_TOOLS;
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::{McpServer, RetrievalEngine, StoreEngine};
use cognis_store::{Database, IndexPublisher};
use cognis_testgen::arb_publish_batch;
use proptest::prelude::*;
use serde_json::{json, Value};

const NAME_POOL: [&str; 10] = [
    "alpha", "beta", "gamma", "delta", "handler", "process", "render", "load", "resolve", "emit",
];

fn isolated_db() -> (PathBuf, Database) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p16-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let db = Database::open(dir.join("determinism.db")).expect("open Property 16 database");
    (dir, db)
}

fn cleanup(dir: &Path, db: &Database) {
    db.close_thread_connection();
    let _ = std::fs::remove_dir_all(dir);
}

fn published_symbol_ids(db: &Database) -> Vec<String> {
    let conn = db.connect().expect("open symbol listing connection");
    let mut stmt = conn
        .prepare("SELECT id FROM symbol ORDER BY id")
        .expect("prepare symbol listing");
    stmt.query_map([], |row| row.get(0))
        .expect("query published symbols")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("read published symbols")
}

fn call_tool<E: RetrievalEngine>(server: &McpServer<E>, name: &str, arguments: Value) -> Value {
    let response = server
        .handle(Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": arguments })),
        })
        .expect("tools/call response");
    response.result.expect("tools/call result")["structuredContent"].clone()
}

fn source_generation(structured: &Value) -> i64 {
    structured
        .get("source_generation")
        .and_then(Value::as_i64)
        .expect("Freshness_Metadata.source_generation")
}

/// Result identifiers in the order the tool returned them.
fn identifiers(tool: &str, structured: &Value) -> Vec<String> {
    let as_id = |value: &Value| {
        value
            .get("symbol_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match tool {
        "symbol_lookup" => structured
            .get("id")
            .and_then(Value::as_str)
            .into_iter()
            .map(str::to_string)
            .collect(),
        "resolve_symbols" => structured
            .get("symbols")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_id)
            .collect(),
        "dependency_trace" => structured
            .get("hits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_id)
            .collect(),
        "retrieve_context_capsule" => structured
            .get("relevant_symbols")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_id)
            .collect(),
        _ => structured
            .get("result")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_id)
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn arguments_for(
    tool: &str,
    query: &str,
    k: i64,
    published: &[String],
    selector: usize,
    direction: &str,
    depth: i64,
    task: &str,
    max_tokens: i64,
    resolve_count: usize,
) -> Value {
    let selected = published
        .get(selector % published.len().max(1))
        .cloned()
        .unwrap_or_else(|| query.to_string());
    match tool {
        "symbol_lookup" => json!({ "name_or_id": selected }),
        "resolve_symbols" => {
            let mut ids = Vec::new();
            if published.is_empty() {
                ids.push(selected);
            } else {
                for index in 0..resolve_count.max(1) {
                    ids.push(published[index % published.len()].clone());
                }
            }
            json!({ "symbol_ids": ids })
        }
        "dependency_trace" => json!({
            "symbol_id": selected,
            "direction": direction,
            "depth": depth,
        }),
        "retrieve_context_capsule" => json!({
            "task": task,
            "max_tokens": max_tokens,
        }),
        _ => json!({ "query": query, "k": k }),
    }
}

fn arb_query() -> impl Strategy<Value = String> {
    prop_oneof![
        (0usize..NAME_POOL.len()).prop_map(|index| NAME_POOL[index].to_string()),
        Just(
            "alpha OR beta OR gamma OR delta OR handler OR process OR render OR load OR resolve OR emit"
                .to_string(),
        ),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 16: Query determinism.
    /// **Validates: Requirements 13.5**
    #[test]
    fn two_calls_without_a_commit_return_identical_identifiers(
        batches in prop::collection::vec(arb_publish_batch(), 1..5),
        query in arb_query(),
        k in -5i64..40,
        selector in any::<usize>(),
        direction in prop_oneof![Just("out"), Just("in"), Just("both")],
        depth in -2i64..10,
        task in arb_query(),
        max_tokens in 400i64..12_000,
        resolve_count in 1usize..8,
    ) {
        let (dir, db) = isolated_db();
        let mut publisher = db.clone();
        for spec in &batches {
            publisher
                .publish(&spec.to_batch())
                .expect("commit generated Publish_Batch");
        }
        let published = published_symbol_ids(&db);
        let server = McpServer::new(StoreEngine::new(db.clone()));

        for tool in MCP_TOOLS {
            let arguments = arguments_for(
                tool,
                &query,
                k,
                &published,
                selector,
                direction,
                depth,
                &task,
                max_tokens,
                resolve_count,
            );
            let first = call_tool(&server, tool, arguments.clone());
            let second = call_tool(&server, tool, arguments);
            prop_assert_eq!(
                source_generation(&first),
                source_generation(&second),
                "{} reported different source_generation across identical calls",
                tool
            );
            prop_assert_eq!(
                identifiers(tool, &first),
                identifiers(tool, &second),
                "{} returned different result identifiers across identical calls",
                tool
            );
        }

        drop(server);
        cleanup(&dir, &db);
    }
}
