//! Property 15: Snapshot consistency.
//!
//! For any interleaving of tool calls and `Publish_Batch` commits, every call's
//! results are derivable from exactly one committed generation, that generation
//! is the one reported as `source_generation` in `Freshness_Metadata`, and
//! content committed during the call is excluded — across lexical, semantic,
//! graph, and hydration stages.
//!
//! **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 15.2, 24.8**

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use cognis_core::{Hit, Result as CognisResult, Symbol};
use cognis_mcp::caps::{Caps, Deadline};
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::server::ToolCtx;
use cognis_mcp::{McpServer, RetrievalEngine, StoreEngine};
use cognis_store::{Database, Direction, GenerationSnapshot, IndexPublisher, PublishBatch};
use cognis_testgen::{arb_publish_batch, PublishBatchSpec};
use proptest::prelude::*;
use serde_json::{json, Value};

/// NAME_POOL tokens joined so FTS5 MATCH covers every generated identifier.
const FTS_QUERY: &str =
    "alpha OR beta OR gamma OR delta OR handler OR process OR render OR load OR resolve OR emit";

fn isolated_db() -> (PathBuf, Database) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p15-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("snapshot.db");
    let db = Database::open(&path).expect("open Property 15 database");
    (dir, db)
}

fn cleanup(dir: &Path, db: &Database) {
    db.close_thread_connection();
    let _ = std::fs::remove_dir_all(dir);
}

fn source_generation(db: &Database) -> i64 {
    db.connect()
        .expect("open generation connection")
        .query_row(
            "SELECT source_generation FROM generation WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("read source_generation")
}

fn symbol_ids_on(snapshot: &GenerationSnapshot<'_>) -> Vec<String> {
    let mut stmt = snapshot
        .connection()
        .prepare("SELECT id FROM symbol ORDER BY id")
        .expect("prepare symbol id query");
    stmt.query_map([], |row| row.get(0))
        .expect("query symbol ids")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("read symbol ids")
}

fn upserted_ids(batches: &[PublishBatchSpec]) -> BTreeSet<String> {
    batches
        .iter()
        .flat_map(|batch| batch.symbol_upserts.iter().map(|symbol| symbol.id.clone()))
        .collect()
}

fn query_vector(before: &[PublishBatchSpec], during: &[PublishBatchSpec]) -> Vec<f32> {
    before
        .iter()
        .chain(during.iter())
        .find_map(|batch| {
            batch
                .vector_upserts
                .first()
                .map(|(_, vector)| vector.clone())
        })
        .unwrap_or_else(|| vec![0.0; 4])
}

fn hit_ids(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|hit| hit.symbol_id.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StageView {
    source_generation: i64,
    semantic_generation: i64,
    pending_vector_count: i64,
    lexical_ids: Vec<String>,
    semantic_ids: Vec<String>,
    graph_ids: Vec<String>,
    incident_keys: Vec<(String, String, String)>,
    hydrated_ids: Vec<String>,
    trace_ids: Vec<String>,
}

fn capture_stages(
    engine: &StoreEngine,
    ctx: &ToolCtx<'_>,
    probe_ids: &[String],
    vector: &[f32],
) -> StageView {
    let lexical = engine
        .fts_search(ctx, FTS_QUERY, 50)
        .expect("lexical stage");
    let semantic = ctx.snapshot.vec_search(vector, 50).expect("semantic stage");
    let graph = ctx.snapshot.build_code_graph(None).expect("graph stage");
    let seed = graph
        .node_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "missing-start".to_string());
    let incident = ctx
        .snapshot
        .edges_incident(std::slice::from_ref(&seed), Direction::Both)
        .expect("incident edges");
    let mut incident_keys: Vec<(String, String, String)> = incident
        .into_iter()
        .map(|edge| (edge.src_id, edge.dst_id, format!("{:?}", edge.kind)))
        .collect();
    incident_keys.sort();
    let hydrated = engine.hydrate(ctx, probe_ids).expect("hydration stage");
    let mut hydrated_ids: Vec<String> = hydrated.into_iter().map(|symbol| symbol.id).collect();
    hydrated_ids.sort();
    let trace = engine
        .dependency_trace(ctx, &seed, "both", 3)
        .expect("graph trace");

    StageView {
        source_generation: ctx.pinned,
        semantic_generation: ctx.snapshot.semantic_generation(),
        pending_vector_count: ctx.snapshot.pending_vector_count(),
        lexical_ids: hit_ids(&lexical),
        semantic_ids: hit_ids(&semantic),
        graph_ids: graph.node_ids,
        incident_keys,
        hydrated_ids,
        trace_ids: hit_ids(&trace),
    }
}

fn tool_structured<E: RetrievalEngine>(
    server: &McpServer<E>,
    name: &str,
    arguments: Value,
) -> Value {
    let request = Request {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": arguments })),
    };
    let response = server.handle(request).expect("tools/call response");
    response.result.expect("tools/call result")["structuredContent"].clone()
}

fn result_symbol_ids(structured: &Value) -> BTreeSet<String> {
    structured
        .get("result")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            hit.get("symbol_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// Publishes `during` batches after the first retrieval stage of a tool call so
/// later stages (and `Freshness_Metadata`) must ignore that commit.
struct InterleaveEngine {
    inner: StoreEngine,
    writer: Mutex<Database>,
    during: Vec<PublishBatch>,
    flushed: AtomicBool,
}

impl InterleaveEngine {
    fn flush_during(&self) {
        if self.flushed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut writer = self.writer.lock().expect("mid-call publisher");
        for batch in &self.during {
            writer
                .publish(batch)
                .expect("Publish_Batch committed during the tool call");
        }
    }
}

impl RetrievalEngine for InterleaveEngine {
    fn open_snapshot(&self) -> CognisResult<GenerationSnapshot<'_>> {
        self.inner.open_snapshot()
    }

    fn missing_required_indexes(&self) -> CognisResult<Vec<String>> {
        self.inner.missing_required_indexes()
    }

    fn fts_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> CognisResult<Vec<Hit>> {
        let hits = self.inner.fts_search(ctx, query, k)?;
        self.flush_during();
        Ok(hits)
    }

    fn semantic_search(&self, ctx: &ToolCtx<'_>, query: &str, k: usize) -> CognisResult<Vec<Hit>> {
        let hits = self.inner.semantic_search(ctx, query, k)?;
        self.flush_during();
        Ok(hits)
    }

    fn diffuse(
        &self,
        ctx: &ToolCtx<'_>,
        seeds: &[Vec<Hit>],
        k: usize,
        alpha: f64,
        eps: f64,
    ) -> CognisResult<Vec<Hit>> {
        let hits = self.inner.diffuse(ctx, seeds, k, alpha, eps)?;
        self.flush_during();
        Ok(hits)
    }

    fn hydrate(&self, ctx: &ToolCtx<'_>, ids: &[String]) -> CognisResult<Vec<Symbol>> {
        let symbols = self.inner.hydrate(ctx, ids)?;
        self.flush_during();
        Ok(symbols)
    }

    fn lookup(
        &self,
        ctx: &ToolCtx<'_>,
        name_or_id: &str,
        kind: Option<&str>,
    ) -> CognisResult<Option<Symbol>> {
        let symbol = self.inner.lookup(ctx, name_or_id, kind)?;
        self.flush_during();
        Ok(symbol)
    }

    fn dependency_trace(
        &self,
        ctx: &ToolCtx<'_>,
        symbol_id: &str,
        direction: &str,
        depth: u8,
    ) -> CognisResult<Vec<Hit>> {
        let hits = self
            .inner
            .dependency_trace(ctx, symbol_id, direction, depth)?;
        self.flush_during();
        Ok(hits)
    }

    fn semantic_available(&self, ctx: &ToolCtx<'_>) -> bool {
        self.inner.semantic_available(ctx)
    }

    fn integration_edge_context(&self, ctx: &ToolCtx<'_>) -> bool {
        self.inner.integration_edge_context(ctx)
    }
}

fn arb_interleaving() -> impl Strategy<Value = (Vec<PublishBatchSpec>, Vec<PublishBatchSpec>)> {
    (
        prop::collection::vec(arb_publish_batch(), 1..5),
        prop::collection::vec(arb_publish_batch(), 0..4),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 15: Snapshot consistency.
    /// **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 15.2, 24.8**
    #[test]
    fn tool_call_results_stay_pinned_across_mid_call_publishes(
        (before, during) in arb_interleaving(),
    ) {
        let (dir, db) = isolated_db();
        let mut publisher = db.clone();
        for spec in &before {
            publisher
                .publish(&spec.to_batch())
                .expect("commit prefix Publish_Batch");
        }
        let pinned_generation = source_generation(&db);
        let engine = StoreEngine::new(db.clone());
        let caps = Caps::default();
        let snapshot = engine.open_snapshot().expect("pin tool-call snapshot");
        let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
        prop_assert_eq!(ctx.pinned, pinned_generation);
        prop_assert_eq!(ctx.snapshot.source_generation(), pinned_generation);

        let pinned_ids: BTreeSet<String> = symbol_ids_on(&ctx.snapshot).into_iter().collect();
        let during_only: Vec<String> = upserted_ids(&during)
            .into_iter()
            .filter(|id| !pinned_ids.contains(id))
            .collect();
        let mut probe_ids: Vec<String> = pinned_ids.iter().cloned().collect();
        probe_ids.extend(during_only.iter().cloned());
        let vector = query_vector(&before, &during);
        let before_view = capture_stages(&engine, &ctx, &probe_ids, &vector);

        let server = McpServer::new(InterleaveEngine {
            inner: StoreEngine::new(db.clone()),
            writer: Mutex::new(db.clone()),
            during: during.iter().map(PublishBatchSpec::to_batch).collect(),
            flushed: AtomicBool::new(false),
        });
        let structured = tool_structured(
            &server,
            "diffuse_context",
            json!({ "query": FTS_QUERY, "k": 10 }),
        );
        let reported = structured
            .get("source_generation")
            .and_then(Value::as_i64)
            .expect("Freshness_Metadata.source_generation");
        prop_assert_eq!(
            reported,
            pinned_generation,
            "Freshness_Metadata must report the generation pinned at call start"
        );
        let call_ids = result_symbol_ids(&structured);
        prop_assert!(
            call_ids.is_subset(&pinned_ids),
            "diffuse_context returned ids committed during the call: {:?}",
            call_ids.difference(&pinned_ids).collect::<Vec<_>>()
        );

        prop_assert_eq!(ctx.pinned, pinned_generation);
        prop_assert_eq!(ctx.snapshot.source_generation(), pinned_generation);
        let after_view = capture_stages(&engine, &ctx, &probe_ids, &vector);
        prop_assert_eq!(
            &after_view,
            &before_view,
            "lexical/semantic/graph/hydration results must stay at the pinned generation"
        );
        prop_assert_eq!(after_view.source_generation, pinned_generation);

        let still_hidden = ctx
            .snapshot
            .hydrate_ids(&during_only)
            .expect("hydrate ids committed during the call");
        prop_assert!(
            still_hidden.is_empty(),
            "hydration saw symbols committed during the call: {:?}",
            still_hidden.iter().map(|symbol| symbol.id.clone()).collect::<Vec<_>>()
        );

        let live = GenerationSnapshot::open(&db).expect("live snapshot after mid-call publishes");
        if during.is_empty() {
            prop_assert_eq!(live.source_generation(), pinned_generation);
        } else {
            prop_assert!(
                live.source_generation() > pinned_generation,
                "writer must advance past the pinned generation when batches commit during the call"
            );
        }
        drop(live);
        drop(ctx);
        drop(server);
        cleanup(&dir, &db);
    }
}
