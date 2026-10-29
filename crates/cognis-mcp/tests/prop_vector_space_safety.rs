//! Property 17: Vector-space safety.
//!
//! A similarity score is computed only when the persisted and query fingerprints
//! are equal — including the equal-dimension-different-model trap. While
//! withholding, the count of similarity computations is zero for every tool,
//! and lexical/graph results are still returned with `semantic_available` false.
//!
//! **Validates: Requirements 14.2, 14.3, 14.4, 14.5, 14.7, 14.8, 24.5**

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cognis_core::{Edge, EdgeKind, Result as CognisResult, Symbol, SymbolKind, MCP_TOOLS};
use cognis_embed::{Embedder, ModelFingerprint};
use cognis_mcp::caps::{Caps, Deadline};
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::server::ToolCtx;
use cognis_mcp::{guard, tools, GuardVerdict, McpServer, RetrievalEngine, StoreEngine};
use cognis_store::{Database, IndexPublisher, PublishBatch, PublishReason};
use cognis_testgen::{arb_fingerprint_pair, FingerprintPair, FingerprintRelation};
use proptest::prelude::*;
use serde_json::{json, Value};

const ALPHA_ID: &str = "python:src/a.py:mod.alpha@aaaaaaa1";
const BETA_ID: &str = "python:src/a.py:mod.beta@bbbbbbb2";

struct InstrumentedEmbedder {
    dim: usize,
    embeds: Arc<AtomicU64>,
    forbid: bool,
}

impl Embedder for InstrumentedEmbedder {
    fn embedding_dim(&self) -> usize {
        self.dim
    }

    fn embed_text(&self, _text: &str) -> CognisResult<Vec<f32>> {
        self.embeds.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.forbid,
            "embed_text must not run when Vector_Space_Guard withholds"
        );
        Ok(vec![1.0; self.dim])
    }

    fn embed_batch(&self, texts: &[String]) -> CognisResult<Vec<Vec<f32>>> {
        texts.iter().map(|_| self.embed_text("")).collect()
    }
}

fn symbol(id: &str, name: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: format!("mod.{name}"),
        language: "python".to_string(),
        module: "mod".to_string(),
        file_path: "src/a.py".to_string(),
        line_start: 1,
        line_end: 2,
        signature: None,
        docstring: None,
        content_hash: name.to_string(),
        body_excerpt: Some(format!("def {name}():\n    pass")),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 1,
    }
}

fn populated_db(pair: &FingerprintPair) -> Database {
    let mut db = Database::new(":memory:");
    db.connect()
        .expect("migrate in-memory vector-space database");
    let dim = pair.persisted.dim;
    let mut batch = PublishBatch::empty(PublishReason::Initial);
    batch.symbol_upserts = vec![symbol(ALPHA_ID, "alpha"), symbol(BETA_ID, "beta")];
    batch.edge_upserts = vec![Edge {
        src_id: ALPHA_ID.to_string(),
        dst_id: BETA_ID.to_string(),
        kind: EdgeKind::Calls,
        confidence: 1.0,
        meta: serde_json::Value::Null,
    }];
    batch.vector_upserts = vec![
        (ALPHA_ID.to_string(), vec![1.0; dim]),
        (BETA_ID.to_string(), vec![0.5; dim]),
    ];
    batch.fingerprint = Some(pair.persisted.to_fingerprint());
    db.publish(&batch).expect("publish vector-space fixture");
    db
}

fn engine_for(pair: &FingerprintPair, embeds: Arc<AtomicU64>) -> StoreEngine {
    StoreEngine::with_embedder(
        populated_db(pair),
        Some(Box::new(InstrumentedEmbedder {
            dim: pair.query.dim,
            embeds,
            forbid: pair.must_withhold(),
        })),
    )
    .with_query_fingerprint(ModelFingerprint::from_digest(pair.query.digest()))
}

fn tool_args(name: &str) -> Value {
    match name {
        "symbol_lookup" => json!({ "name_or_id": ALPHA_ID }),
        "resolve_symbols" => json!({ "symbol_ids": [ALPHA_ID, BETA_ID] }),
        "dependency_trace" => json!({
            "symbol_id": ALPHA_ID,
            "direction": "out",
            "depth": 3,
        }),
        "retrieve_context_capsule" => json!({ "task": "alpha", "max_tokens": 800 }),
        "analyze_patch_impact" => json!({ "targets": [ALPHA_ID] }),
        "inspect_symbol_complexity" => json!({ "name_or_id": ALPHA_ID }),
        "trace_blast_radius" => json!({ "symbol_id": ALPHA_ID }),
        "suggest_optimal_refactor" => json!({ "symbol_id": ALPHA_ID }),
        _ => json!({ "query": "alpha", "k": 8 }),
    }
}

fn dispatch_tool(engine: &StoreEngine, ctx: &ToolCtx<'_>, name: &str) -> Value {
    let args = tool_args(name);
    let result = match name {
        "symbol_search" => tools::symbol_search(engine, ctx, &args),
        "semantic_search" => tools::semantic_search(engine, ctx, &args),
        "discover_symbols" => tools::discover_symbols(engine, ctx, &args),
        "diffuse_context" => tools::diffuse_context(engine, ctx, &args),
        "symbol_lookup" => tools::symbol_lookup(engine, ctx, &args),
        "dependency_trace" => tools::dependency_trace(engine, ctx, &args),
        "resolve_symbols" => tools::resolve_symbols(engine, ctx, &args),
        "retrieve_context_capsule" => tools::retrieve_context_capsule(engine, ctx, &args),
        "analyze_patch_impact" => tools::analyze_patch_impact(engine, ctx, &args),
        "inspect_symbol_complexity" => tools::inspect_symbol_complexity(engine, ctx, &args),
        "trace_blast_radius" => tools::trace_blast_radius(engine, ctx, &args),
        "suggest_optimal_refactor" => tools::suggest_optimal_refactor(engine, ctx, &args),
        other => panic!("unexpected tool {other}"),
    };
    result.unwrap_or_else(|error| error.to_envelope())
}

fn call_mcp(server: &McpServer<StoreEngine>, name: &str) -> Value {
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 17: Vector-space safety.
    /// **Validates: Requirements 14.2, 14.3, 14.4, 14.5, 14.7, 14.8, 24.5**
    #[test]
    fn similarity_is_scored_only_when_fingerprints_match(
        pair in arb_fingerprint_pair(),
    ) {
        let (persisted, query) = pair.to_fingerprints();
        let query_fp = ModelFingerprint::from_digest(&query.digest);
        match guard(Some(&persisted), &query_fp) {
            GuardVerdict::Allow => {
                prop_assert!(
                    !pair.must_withhold(),
                    "guard allowed a pair that must be withheld: {:?}",
                    pair.relation
                );
            }
            GuardVerdict::Withhold { .. } => {
                prop_assert!(
                    pair.must_withhold(),
                    "guard withheld an equal fingerprint pair"
                );
            }
        }
        if let FingerprintRelation::EqualDimensionDifferentSpace { .. } = pair.relation {
            prop_assert_eq!(persisted.dim, query.dim);
            prop_assert_ne!(&persisted.digest, &query.digest);
        }

        let embeds = Arc::new(AtomicU64::new(0));
        let engine = engine_for(&pair, Arc::clone(&embeds));
        let caps = Caps::default();
        let snapshot = engine.open_snapshot().expect("pin snapshot");
        let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);

        let lexical = engine.fts_search(&ctx, "alpha", 8).expect("lexical search");
        prop_assert!(
            lexical.iter().any(|hit| hit.symbol_id == ALPHA_ID),
            "lexical results must still be returned"
        );
        let traced = engine
            .dependency_trace(&ctx, ALPHA_ID, "out", 3)
            .expect("graph trace");
        prop_assert!(
            traced.iter().any(|hit| hit.symbol_id == BETA_ID),
            "graph results must still be returned"
        );
        let semantic = engine
            .semantic_search(&ctx, "alpha", 8)
            .expect("semantic search");
        drop(ctx);

        for tool in MCP_TOOLS {
            let snapshot = engine.open_snapshot().expect("per-tool snapshot");
            let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
            let _payload = dispatch_tool(&engine, &ctx, tool);
            if pair.must_withhold() {
                prop_assert!(
                    !ctx.semantic_included(),
                    "{} included vector-derived results while withholding",
                    tool
                );
            }
        }

        if pair.must_withhold() {
            prop_assert!(semantic.is_empty(), "withheld semantic_search must be empty");
            prop_assert_eq!(
                engine.similarity_computations(),
                0,
                "no persisted-vector similarity may run while withholding ({:?})",
                pair.relation
            );
            prop_assert_eq!(
                embeds.load(Ordering::SeqCst),
                0,
                "query embedder must not run while withholding"
            );
        } else {
            prop_assert!(
                engine.similarity_computations() > 0,
                "matching fingerprints must be allowed to score persisted vectors"
            );
            prop_assert!(
                !semantic.is_empty(),
                "matching space should return semantic hits"
            );
        }

        let server = McpServer::new(engine_for(&pair, Arc::new(AtomicU64::new(0))));
        for tool in MCP_TOOLS {
            let structured = call_mcp(&server, tool);
            if pair.must_withhold() {
                let available = structured
                    .get("semantic_available")
                    .and_then(Value::as_bool)
                    .expect("Freshness_Metadata.semantic_available");
                prop_assert!(
                    !available,
                    "{} must report semantic_available false while withholding",
                    tool
                );
            }
        }
    }
}
