//! Property 19: the eight-tool contract is stable and freshness is additive.
//!
//! **Validates: Requirements 15.1, 15.3, 15.5, 15.6, 22.2, 22.3**

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cognis_core::{Hit, Result as CognisResult, Symbol, SymbolKind};
use cognis_mcp::caps::Deadline;
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::server::ToolCtx;
use cognis_mcp::{tools, Caps, McpServer, RetrievalEngine, MCP_TOOLS};
use cognis_store::{Database, GenerationSnapshot};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use rusqlite::params;
use serde_json::{json, Value};

const PRE_FEATURE_TOOL_ORDER: [&str; 8] = [
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
];
const VECTOR_SYMBOL_ID: &str = "generated:semantic";
const FRESHNESS_FIELDS: [&str; 4] = [
    "source_generation",
    "semantic_generation",
    "pending_vector_count",
    "semantic_available",
];

#[derive(Clone, Copy, Debug)]
struct FreshnessTruth {
    pinned_generation: i64,
    semantic_generation: i64,
    pending_vector_count: i64,
    semantic_source_available: bool,
}

#[derive(Clone, Debug)]
struct GeneratedArguments {
    query: String,
    k: i64,
    select_second_symbol: bool,
    resolve_selectors: Vec<u8>,
    direction: &'static str,
    depth: i64,
    task: String,
    max_tokens: i64,
}

impl GeneratedArguments {
    fn for_tool(&self, name: &str) -> Value {
        let selected = if self.select_second_symbol {
            "generated:helper"
        } else {
            VECTOR_SYMBOL_ID
        };
        match name {
            "symbol_lookup" => json!({ "name_or_id": selected }),
            "resolve_symbols" => json!({
                "symbol_ids": self.resolve_selectors.iter().map(|selector| match selector % 3 {
                    0 => VECTOR_SYMBOL_ID,
                    1 => "generated:helper",
                    _ => "generated:missing",
                }).collect::<Vec<_>>()
            }),
            "dependency_trace" => json!({
                "symbol_id": selected,
                "direction": self.direction,
                "depth": self.depth,
            }),
            "retrieve_context_capsule" => json!({
                "task": self.task,
                "max_tokens": self.max_tokens,
            }),
            _ => json!({ "query": self.query, "k": self.k }),
        }
    }
}

struct ContractEngine {
    db: Database,
    db_path: PathBuf,
    symbols: Vec<Symbol>,
    truth: FreshnessTruth,
}

impl Clone for ContractEngine {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            db_path: self.db_path.clone(),
            symbols: self.symbols.clone(),
            truth: self.truth,
        }
    }
}

impl ContractEngine {
    fn new(truth: FreshnessTruth) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let db_path = std::env::temp_dir().join(format!(
            "cognis-p19-{}-{}.db",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&db_path);
        let db = Database::open(&db_path).expect("p19 fixture database");
        seed_freshness(&db, truth);
        Self {
            db,
            db_path,
            symbols: vec![
                symbol(
                    VECTOR_SYMBOL_ID,
                    "semantic_target",
                    "generated.semantic_target",
                    "src/generated.rs",
                ),
                symbol(
                    "generated:helper",
                    "helper",
                    "generated.helper",
                    "src/helper.rs",
                ),
            ],
            truth,
        }
    }

    fn hit(&self, id: &str, layer: &str, score: f64) -> Hit {
        Hit::new(id, score, layer, "generated contract hit")
    }

    fn close(self) {
        self.db.close_thread_connection();
        let _ = std::fs::remove_file(&self.db_path);
        let _ = std::fs::remove_file(format!("{}-wal", self.db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.db_path.display()));
    }
}

impl RetrievalEngine for ContractEngine {
    fn open_snapshot(&self) -> CognisResult<GenerationSnapshot<'_>> {
        GenerationSnapshot::open(&self.db)
    }

    fn fts_search(&self, _ctx: &ToolCtx<'_>, _query: &str, k: usize) -> CognisResult<Vec<Hit>> {
        Ok(self
            .symbols
            .iter()
            .filter(|symbol| symbol.id != VECTOR_SYMBOL_ID)
            .take(k)
            .enumerate()
            .map(|(rank, symbol)| self.hit(&symbol.id, "lexical", 1.0 / (rank + 1) as f64))
            .collect())
    }

    fn semantic_search(
        &self,
        _ctx: &ToolCtx<'_>,
        _query: &str,
        k: usize,
    ) -> CognisResult<Vec<Hit>> {
        if !self.truth.semantic_source_available || k == 0 {
            return Ok(Vec::new());
        }
        Ok(vec![self.hit(VECTOR_SYMBOL_ID, "semantic", 0.875)])
    }

    fn diffuse(
        &self,
        _ctx: &ToolCtx<'_>,
        seeds: &[Vec<Hit>],
        k: usize,
        _alpha: f64,
        _eps: f64,
    ) -> CognisResult<Vec<Hit>> {
        Ok(seeds
            .iter()
            .flatten()
            .take(k)
            .map(|hit| {
                self.hit(&hit.symbol_id, "csar", hit.score)
                    .with_evidence(json!({
                        "on_path": true,
                        "ppr_score": hit.score,
                    }))
            })
            .collect())
    }

    fn hydrate(&self, _ctx: &ToolCtx<'_>, ids: &[String]) -> CognisResult<Vec<Symbol>> {
        Ok(self
            .symbols
            .iter()
            .filter(|symbol| ids.contains(&symbol.id))
            .cloned()
            .collect())
    }

    fn lookup(
        &self,
        _ctx: &ToolCtx<'_>,
        name_or_id: &str,
        kind: Option<&str>,
    ) -> CognisResult<Option<Symbol>> {
        Ok(self
            .symbols
            .iter()
            .find(|symbol| {
                (symbol.id == name_or_id
                    || symbol.name == name_or_id
                    || symbol.qualified_name == name_or_id)
                    && kind.is_none_or(|expected| expected == "function")
            })
            .cloned())
    }

    fn dependency_trace(
        &self,
        _ctx: &ToolCtx<'_>,
        symbol_id: &str,
        _direction: &str,
        depth: u8,
    ) -> CognisResult<Vec<Hit>> {
        let target = if symbol_id == VECTOR_SYMBOL_ID {
            "generated:helper"
        } else {
            VECTOR_SYMBOL_ID
        };
        Ok(vec![self
            .hit(target, "structural", 1.0)
            .with_evidence(json!({ "depth": depth.min(1) }))])
    }

    fn semantic_available(&self, _ctx: &ToolCtx<'_>) -> bool {
        self.truth.semantic_source_available
    }
}

fn seed_freshness(db: &Database, truth: FreshnessTruth) {
    let conn = db.connect().expect("connect p19 fixture");
    conn.execute(
        "UPDATE generation \
         SET source_generation = ?1, semantic_generation = ?2, updated_at = 1 \
         WHERE id = 1",
        params![truth.pinned_generation, truth.semantic_generation],
    )
    .expect("seed generation");
    conn.execute("DELETE FROM embedding_queue", [])
        .expect("clear embedding queue");
    for index in 0..truth.pending_vector_count {
        conn.execute(
            "INSERT INTO embedding_queue( \
                 symbol_id, file_path, enqueued_at, attempts, next_attempt_at, last_error \
             ) VALUES (?1, ?2, 1, 0, 0, NULL)",
            params![format!("symbol-{index}"), format!("src/{index}.rs")],
        )
        .expect("seed embedding queue");
    }
}

fn symbol(id: &str, name: &str, qualified_name: &str, file_path: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        language: "rust".to_string(),
        module: "generated".to_string(),
        file_path: file_path.to_string(),
        line_start: 1,
        line_end: 3,
        signature: Some(format!("fn {name}()")),
        docstring: Some("generated contract fixture".to_string()),
        content_hash: "generated-hash".to_string(),
        body_excerpt: Some(format!("fn {name}() {{}}")),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 1,
    }
}

fn bounded_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z][A-Za-z0-9 _-]{0,31}").unwrap()
}

fn arb_arguments() -> impl Strategy<Value = GeneratedArguments> {
    (
        bounded_text(),
        -100i64..101,
        any::<bool>(),
        prop::collection::vec(any::<u8>(), 0..16),
        prop_oneof![Just("out"), Just("in"), Just("both")],
        -20i64..21,
        bounded_text(),
        -1_000i64..40_001,
    )
        .prop_map(
            |(
                query,
                k,
                select_second_symbol,
                resolve_selectors,
                direction,
                depth,
                task,
                max_tokens,
            )| {
                GeneratedArguments {
                    query,
                    k,
                    select_second_symbol,
                    resolve_selectors,
                    direction,
                    depth,
                    task,
                    max_tokens,
                }
            },
        )
}

fn arb_truth() -> impl Strategy<Value = FreshnessTruth> {
    (0i64..1_000, 0i64..1_000, 0i64..100, any::<bool>()).prop_map(
        |(pinned_generation, semantic_seed, pending_vector_count, semantic_source_available)| {
            FreshnessTruth {
                pinned_generation,
                semantic_generation: semantic_seed.min(pinned_generation),
                pending_vector_count,
                semantic_source_available,
            }
        },
    )
}

fn list_tools<E: RetrievalEngine>(server: &McpServer<E>) -> Value {
    let response = server
        .handle(Request {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        })
        .expect("tools/list response");
    response.result.expect("tools/list result")
}

fn call_tool<E: RetrievalEngine>(server: &McpServer<E>, name: &str, arguments: Value) -> Value {
    let response = server
        .handle(Request {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "tools/call".to_string(),
            params: Some(json!({ "name": name, "arguments": arguments })),
        })
        .expect("tools/call response");
    response.result.expect("tools/call result")
}

fn baseline_output(engine: &ContractEngine, name: &str, arguments: &Value) -> (Value, bool) {
    let caps = Caps::default();
    let snapshot = engine.open_snapshot().expect("baseline snapshot");
    let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
    let result = match name {
        "diffuse_context" => tools::diffuse_context(engine, &ctx, arguments),
        "symbol_lookup" => tools::symbol_lookup(engine, &ctx, arguments),
        "symbol_search" => tools::symbol_search(engine, &ctx, arguments),
        "discover_symbols" => tools::discover_symbols(engine, &ctx, arguments),
        "semantic_search" => tools::semantic_search(engine, &ctx, arguments),
        "resolve_symbols" => tools::resolve_symbols(engine, &ctx, arguments),
        "dependency_trace" => tools::dependency_trace(engine, &ctx, arguments),
        "retrieve_context_capsule" => tools::retrieve_context_capsule(engine, &ctx, arguments),
        _ => unreachable!("contract tool"),
    };
    (
        result.unwrap_or_else(|error| error.to_envelope()),
        ctx.semantic_included(),
    )
}

fn pre_feature_structured(payload: &Value) -> Value {
    if payload.is_object() {
        payload.clone()
    } else {
        json!({ "result": payload })
    }
}

fn canonical_input_schemas() -> [Value; 8] {
    let query = || {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "k": { "type": "integer" },
            },
            "required": ["query"],
        })
    };
    [
        query(),
        json!({
            "type": "object",
            "properties": {
                "name_or_id": { "type": "string" },
                "kind": { "type": "string" },
            },
            "required": ["name_or_id"],
        }),
        query(),
        query(),
        query(),
        json!({
            "type": "object",
            "properties": {
                "symbol_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                },
            },
            "required": ["symbol_ids"],
        }),
        json!({
            "type": "object",
            "properties": {
                "symbol_id": { "type": "string" },
                "direction": { "type": "string", "enum": ["out", "in", "both"] },
                "depth": { "type": "integer" },
            },
            "required": ["symbol_id"],
        }),
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string" },
                "max_tokens": { "type": "integer" },
            },
            "required": ["task"],
        }),
    ]
}

fn assert_exactly_additive(
    structured: &Value,
    baseline: &Value,
) -> std::result::Result<(), TestCaseError> {
    let actual = structured
        .as_object()
        .ok_or_else(|| TestCaseError::fail("structuredContent must be an object"))?;
    let expected = baseline
        .as_object()
        .ok_or_else(|| TestCaseError::fail("baseline wrapper must be an object"))?;
    let freshness: BTreeSet<&str> = FRESHNESS_FIELDS.into_iter().collect();
    let added: BTreeSet<&str> = actual
        .keys()
        .filter(|key| !expected.contains_key(*key))
        .map(String::as_str)
        .collect();

    prop_assert_eq!(
        added,
        freshness,
        "freshness must be the only additive output"
    );
    for (key, value) in expected {
        prop_assert_eq!(actual.get(key), Some(value), "pre-feature field {}", key);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 19: For every contract tool
    // and any valid arguments, freshness is exactly additive and truthful.
    // **Validates: Requirements 15.1, 15.3, 15.5, 15.6, 22.2, 22.3**
    #[test]
    fn contract_stability_with_additive_freshness(
        arguments in arb_arguments(),
        truth in arb_truth(),
    ) {
        let engine = ContractEngine::new(truth);
        let server = McpServer::new(engine.clone());
        let listed = list_tools(&server);
        let advertised = listed["tools"].as_array().expect("tools array");
        let schemas = canonical_input_schemas();

        prop_assert_eq!(&MCP_TOOLS[..PRE_FEATURE_TOOL_ORDER.len()], PRE_FEATURE_TOOL_ORDER.as_slice());
        prop_assert_eq!(advertised.len(), MCP_TOOLS.len());

        for (index, name) in PRE_FEATURE_TOOL_ORDER.iter().enumerate() {
            prop_assert_eq!(advertised[index]["name"].as_str(), Some(*name));
            prop_assert_eq!(&advertised[index]["inputSchema"], &schemas[index]);

            let args = arguments.for_tool(name);
            let (baseline_payload, semantic_included) = baseline_output(&engine, name, &args);
            let baseline_structured = pre_feature_structured(&baseline_payload);
            let call = call_tool(&server, name, args);
            let structured = &call["structuredContent"];

            assert_exactly_additive(structured, &baseline_structured)?;
            prop_assert_eq!(structured["source_generation"].as_i64(), Some(truth.pinned_generation));
            prop_assert_eq!(structured["semantic_generation"].as_i64(), Some(truth.semantic_generation));
            prop_assert_eq!(structured["pending_vector_count"].as_i64(), Some(truth.pending_vector_count));
            prop_assert_eq!(
                structured["semantic_available"].as_bool(),
                Some(semantic_included),
                "{} must report whether its result included vector-derived data",
                name,
            );
            prop_assert_eq!(
                call["content"][0]["text"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok()),
                Some(structured.clone()),
                "text and structured result must expose the same additive contract",
            );
        }
        drop(server);
        engine.close();
    }
}
