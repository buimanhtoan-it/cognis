//! End-to-End Real-World AI Agent Guardrail & Impact Analysis Test.
//!
//! Creates a realistic multi-file TypeScript codebase on disk, indexes it through
//! the real Tree-sitter `cognis_indexer::Pipeline` into SQLite, mounts `StoreEngine`
//! and `McpServer`, and drives all 4 flagship complexity/impact/optimization MCP tools
//! plus the `cognis://live/guardrails` resource over JSON-RPC.

use std::fs;
use std::time::Instant;

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::{McpServer, StoreEngine};
use cognis_store::Database;
use serde_json::{json, Value};

fn call_tool(server: &McpServer<StoreEngine>, name: &str, args: Value) -> (Value, f64) {
    let t0 = Instant::now();
    let resp = server
        .handle(Request {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": name,
                "arguments": args,
            })),
        })
        .expect("JSON-RPC response");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let result = resp.result.expect("tools/call result");
    (result["structuredContent"].clone(), elapsed_ms)
}

#[test]
fn real_world_ai_agent_continuous_guardrail_workflow() {
    let tmp_dir = std::env::temp_dir().join(format!(
        "cognis-live-agent-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let repo_root = tmp_dir.join("repo");
    fs::create_dir_all(repo_root.join("src/routes")).unwrap();
    fs::create_dir_all(repo_root.join("src/services")).unwrap();
    fs::create_dir_all(repo_root.join("src/utils")).unwrap();
    fs::create_dir_all(repo_root.join("tests")).unwrap();

    // 1. Write a realistic multi-file TypeScript project:
    // - src/routes/checkout.ts: API entrypoint calling processBatchCheckout inside a request handler
    fs::write(
        repo_root.join("src/routes/checkout.ts"),
        r#"
import { processBatchCheckout } from "../services/order_service";

export async function handleCheckoutRoute(reqOrders: any[], activeUsers: any[]) {
    return processBatchCheckout(reqOrders, activeUsers);
}
"#,
    )
    .unwrap();

    // - src/services/order_service.ts: Service calling validateUserPermissions inside a loop
    fs::write(
        repo_root.join("src/services/order_service.ts"),
        r#"
export function validateUserPermissions(userId: string, activeUsers: any[]): boolean {
    for (const user of activeUsers) {
        if (user.id === userId) {
            return true;
        }
    }
    return false;
}

export function processBatchCheckout(orders: any[], activeUsers: any[]): any[] {
    const approved: any[] = [];
    for (const order of orders) {
        if (validateUserPermissions(order.userId, activeUsers)) {
            approved.push(order);
        }
    }
    return approved;
}
"#,
    )
    .unwrap();

    // - src/utils/lookup_map.ts: Existing O(1) index helper in the repo that OPT-001 should discover
    fs::write(
        repo_root.join("src/utils/lookup_map.ts"),
        r#"
export function buildUserLookupMap(users: any[]): Map<string, any> {
    const indexMap = new Map<string, any>();
    for (const u of users) {
        indexMap.set(u.id, u);
    }
    return indexMap;
}
"#,
    )
    .unwrap();

    // - tests/checkout.test.ts: Downstream test exercising handleCheckoutRoute -> processBatchCheckout
    fs::write(
        repo_root.join("tests/checkout.test.ts"),
        r#"
import { handleCheckoutRoute } from "../src/routes/checkout";

export function test_checkout_flow() {
    const res = handleCheckoutRoute([{ userId: "u1" }], [{ id: "u1" }]);
    return res.length === 1;
}
"#,
    )
    .unwrap();

    // 2. Index the real repository into SQLite using cognis_indexer::IndexerPipeline
    let db_path = tmp_dir.join("uckg.db");
    let db = Database::open(&db_path).expect("open sqlite db");
    let mut pipeline = IndexerPipeline::new(db.clone(), Config::default());
    let stats = pipeline
        .index_repo(&repo_root, true)
        .expect("index repository");
    println!(
        "\n=== [STEP 0] Indexed Real Repository: {} files, {} symbols, {} edges ===",
        stats.files_processed, stats.symbols_indexed, stats.edges_resolved
    );
    assert!(stats.symbols_indexed >= 4);

    let engine = StoreEngine::new(db);
    let server = McpServer::new(engine);

    // 3. STEP 1: `inspect_symbol_complexity` — Detect hidden cross-function O(n^2)!
    // `processBatchCheckout` has 1 loop O(n), calling `validateUserPermissions` which has 1 loop O(n) => Propagated O(n^2)!
    let (inspect_res, inspect_ms) = call_tool(
        &server,
        "inspect_symbol_complexity",
        json!({ "name_or_id": "processBatchCheckout" }),
    );
    println!(
        "\n=== [STEP 1] inspect_symbol_complexity ({:.2}ms) ===\n{}",
        inspect_ms,
        serde_json::to_string_pretty(&inspect_res).unwrap()
    );
    let reports = inspect_res["reports"].as_array().expect("reports array");
    assert!(!reports.is_empty());
    let proc_report = reports
        .iter()
        .find(|r| {
            r["qualified_name"]
                .as_str()
                .unwrap_or("")
                .contains("processBatchCheckout")
        })
        .expect("processBatchCheckout report");
    assert_eq!(proc_report["local_time"], "O(n)");
    assert_eq!(
        proc_report["propagated_time"], "O(n^2)",
        "Must detect hidden inter-procedural O(n^2) across caller -> callee loop!"
    );

    // 4. STEP 2: `analyze_patch_impact` — Mid-flight speculative shadow edit (unsaved buffer)
    // AI agent edits `src/services/order_service.ts` in memory, adding a 3-deep nested loop O(n^3)
    let proposed_code = r#"
export function validateUserPermissions(userId: string, activeUsers: any[]): boolean {
    for (const user of activeUsers) {
        if (user.id === userId) {
            return true;
        }
    }
    return false;
}

export function processBatchCheckout(orders: any[], activeUsers: any[]): any[] {
    const approved: any[] = [];
    for (const order of orders) {
        for (const item of order.items) {
            if (validateUserPermissions(order.userId, activeUsers)) {
                approved.push(item);
            }
        }
    }
    return approved;
}
"#;
    let (patch_res, patch_ms) = call_tool(
        &server,
        "analyze_patch_impact",
        json!({
            "file_path": "src/services/order_service.ts",
            "new_content": proposed_code,
        }),
    );
    println!(
        "\n=== [STEP 2] analyze_patch_impact (Shadow AST + CSIA in {:.2}ms) ===\nSummary: {}\nVerdict: {}",
        patch_ms,
        patch_res["summary_markdown"].as_str().unwrap_or(""),
        patch_res["verdict"].as_str().unwrap_or("")
    );
    assert_eq!(patch_res["verdict"], "WARN_COMPLEXITY_REGRESSION");
    let patch_reports = patch_res["complexity_reports"].as_array().unwrap();
    let patched_proc = patch_reports
        .iter()
        .find(|r| {
            r["qualified_name"]
                .as_str()
                .unwrap_or("")
                .contains("processBatchCheckout")
        })
        .expect("patched processBatchCheckout");
    assert_eq!(patched_proc["propagated_time"], "O(n^3)");

    // 5. STEP 3: `trace_blast_radius` — CSIA reverse-PPR blast radius from `validateUserPermissions`
    let (blast_res, blast_ms) = call_tool(
        &server,
        "trace_blast_radius",
        json!({
            "symbol_id": "validateUserPermissions",
            "signature_changed": true,
            "new_complexity": "O(n^2)"
        }),
    );
    println!(
        "\n=== [STEP 3] trace_blast_radius ({:.2}ms) ===\n{}",
        blast_ms,
        serde_json::to_string_pretty(&blast_res).unwrap()
    );
    assert!(blast_res["reached_symbol_count"].as_u64().unwrap() >= 1);
    assert!(!blast_res["breaking_call_sites"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!blast_res["complexity_amplifications"]
        .as_array()
        .unwrap()
        .is_empty());

    // 6. STEP 4: `suggest_optimal_refactor` — Grounded optimization advisories
    let (refactor_res, refactor_ms) = call_tool(
        &server,
        "suggest_optimal_refactor",
        json!({
            "symbol_id": "processBatchCheckout",
        }),
    );
    println!(
        "\n=== [STEP 4] suggest_optimal_refactor ({:.2}ms) ===\n{}",
        refactor_ms,
        serde_json::to_string_pretty(&refactor_res).unwrap()
    );
    let advisories = refactor_res["advisories"].as_array().unwrap();
    assert!(
        advisories.iter().any(|a| a["rule_id"] == "OPT-001"),
        "Must recommend OPT-001 (HashSet/Map pre-indexing) for cross-function loop lookup"
    );

    // 7. STEP 5: Live MCP Resource `cognis://live/guardrails`
    let resource_resp = server
        .handle(Request {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(99)),
            method: "resources/read".to_string(),
            params: Some(json!({ "uri": "cognis://live/guardrails" })),
        })
        .expect("resources/read response")
        .result
        .expect("resources/read result");
    println!(
        "\n=== [STEP 5] resources/read cognis://live/guardrails ===\n{}",
        resource_resp["contents"][0]["text"].as_str().unwrap_or("")
    );
    assert_eq!(
        resource_resp["contents"][0]["uri"],
        "cognis://live/guardrails"
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}
