//! Contract-shaped MCP tool handlers: retrieval, real-time complexity analysis,
//! repo-native optimization synthesis, and CSIA change-impact evaluation.

use std::collections::{HashMap, HashSet};

use cognis_core::{Edge, EdgeKind, Hit, ImpactGraph, Symbol};
use cognis_csar::{evaluate_change_impact, SymbolImpactSeed};
use cognis_indexer::{
    analyze_shadow_patch, analyze_workspace_complexity, parse_notation_to_class, ShadowFileEdit,
};
use cognis_store::Direction;
use serde_json::{json, Value};

use crate::caps::StageDecision;
use crate::engine::RetrievalEngine;
use crate::errors::McpError;
use crate::server::ToolCtx;

/// CSAR forward-push defaults (mirror `COGNIS_MCP_CSAR_ALPHA/EPS`).
const CSAR_ALPHA: f64 = 0.15;
const CSAR_EPS: f64 = 1e-5;
/// Seed breadth fed into diffusion before ranking (mirror `_CSAR_SEED_K`).
const CSAR_SEED_K: usize = 25;
/// Default `k` when a caller omits it (mirror the Python tool defaults).
const DEFAULT_K: i64 = 10;
/// Cap on hydrated subgraph nodes for real-time complexity/impact evaluation.
const ANALYSIS_SUBGRAPH_CAP: usize = 96;

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, McpError> {
    match arg_str(args, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(McpError::invalid_argument(format!(
            "'{key}' is required and must be a non-empty string"
        ))),
    }
}

fn engine_err(e: cognis_core::CognisError) -> McpError {
    McpError::internal(format!("retrieval failed: {e}"))
}

fn continue_stage(ctx: &ToolCtx<'_>, tool: &str) -> Result<bool, McpError> {
    match ctx.stage_gate(tool)? {
        StageDecision::Continue => Ok(true),
        StageDecision::Truncate => Ok(false),
    }
}

fn symbol_to_dict(s: &Symbol) -> Value {
    json!({
        "id": s.id,
        "kind": s.kind,
        "name": s.name,
        "qualified_name": s.qualified_name,
        "language": s.language,
        "module": s.module,
        "file_path": s.file_path,
        "line_start": s.line_start,
        "line_end": s.line_end,
        "signature": s.signature,
        "docstring": s.docstring,
        "content_hash": s.content_hash,
        "body_excerpt": s.body_excerpt,
        "risk_score": s.risk_score,
    })
}

fn search_hit(s: &Symbol, score: f64, match_reason: &str) -> Value {
    json!({
        "symbol_id": s.id,
        "id": s.id,
        "name": s.name,
        "qualified_name": s.qualified_name,
        "kind": s.kind,
        "file_path": s.file_path,
        "line_start": s.line_start,
        "line_end": s.line_end,
        "score": score,
        "match_reason": match_reason,
        "snippet": s.body_excerpt,
        "body_excerpt": s.body_excerpt,
    })
}

fn hydrate_map(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    ids: &[String],
) -> Result<HashMap<String, Symbol>, McpError> {
    let symbols = engine.hydrate(ctx, ids).map_err(engine_err)?;
    Ok(symbols.into_iter().map(|s| (s.id.clone(), s)).collect())
}

fn hit_ids(layers: &[&[Hit]]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    for layer in layers {
        for h in *layer {
            if seen.insert(h.symbol_id.clone()) {
                ids.push(h.symbol_id.clone());
            }
        }
    }
    ids
}

pub fn symbol_search(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = ctx.caps.clamp_k(arg_i64(args, "k").unwrap_or(8));
    let hits = if continue_stage(ctx, "symbol_search")? {
        engine.fts_search(ctx, query, k).map_err(engine_err)?
    } else {
        Vec::new()
    };
    let by_id = if continue_stage(ctx, "symbol_search")? {
        hydrate_map(engine, ctx, &hit_ids(&[&hits]))?
    } else {
        HashMap::new()
    };

    let mut out = Vec::new();
    for h in &hits {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            out.push(search_hit(sym, h.score, "fts_bm25"));
        }
    }
    Ok(Value::Array(out))
}

pub fn semantic_search(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = ctx.caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));
    let hits = if continue_stage(ctx, "semantic_search")? {
        engine.semantic_search(ctx, query, k).map_err(engine_err)?
    } else {
        Vec::new()
    };
    let by_id = if continue_stage(ctx, "semantic_search")? {
        hydrate_map(engine, ctx, &hit_ids(&[&hits]))?
    } else {
        HashMap::new()
    };

    let mut out = Vec::new();
    for h in &hits {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            let mut hit = search_hit(sym, h.score, "semantic_knn");
            hit["match_sources"] = json!(["semantic"]);
            out.push(hit);
        }
    }
    if !out.is_empty() {
        ctx.note_semantic_included();
    }
    Ok(Value::Array(out))
}

pub fn discover_symbols(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = ctx.caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));

    let lexical = if continue_stage(ctx, "discover_symbols")? {
        engine.fts_search(ctx, query, k).map_err(engine_err)?
    } else {
        Vec::new()
    };
    let semantic = if continue_stage(ctx, "discover_symbols")? {
        engine.semantic_search(ctx, query, k).map_err(engine_err)?
    } else {
        Vec::new()
    };

    let mut sources: HashMap<String, Vec<&'static str>> = HashMap::new();
    for h in &lexical {
        sources
            .entry(h.symbol_id.clone())
            .or_default()
            .push("lexical");
    }
    for h in &semantic {
        sources
            .entry(h.symbol_id.clone())
            .or_default()
            .push("semantic");
    }

    let fused = cognis_retrieval::rrf_fuse(
        &[lexical.clone(), semantic.clone()],
        k,
        cognis_retrieval::DEFAULT_RRF_K,
    );
    let by_id = if continue_stage(ctx, "discover_symbols")? {
        hydrate_map(engine, ctx, &hit_ids(&[&fused]))?
    } else {
        HashMap::new()
    };

    let mut out = Vec::new();
    for h in &fused {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            let mut hit = search_hit(sym, h.score, "hybrid_rrf");
            let mut src = sources.get(&h.symbol_id).cloned().unwrap_or_default();
            src.sort_unstable();
            src.dedup();
            hit["match_sources"] = json!(src);
            if src.contains(&"semantic") {
                ctx.note_semantic_included();
            }
            out.push(hit);
        }
    }
    Ok(Value::Array(out))
}

pub fn diffuse_context(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = ctx.caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));
    let alpha = args
        .get("alpha")
        .and_then(Value::as_f64)
        .unwrap_or(CSAR_ALPHA);
    let eps = args.get("eps").and_then(Value::as_f64).unwrap_or(CSAR_EPS);

    let lexical = if continue_stage(ctx, "diffuse_context")? {
        engine
            .fts_search(ctx, query, CSAR_SEED_K)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };
    let semantic = if continue_stage(ctx, "diffuse_context")? && engine.semantic_available(ctx) {
        engine
            .semantic_search(ctx, query, CSAR_SEED_K)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };

    let semantic_ids: HashSet<String> = semantic.iter().map(|hit| hit.symbol_id.clone()).collect();
    let seeds = vec![lexical, semantic];
    let diffused = if continue_stage(ctx, "diffuse_context")? {
        engine
            .diffuse(ctx, &seeds, k, alpha, eps)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };
    let by_id = if continue_stage(ctx, "diffuse_context")? {
        hydrate_map(engine, ctx, &hit_ids(&[&diffused]))?
    } else {
        HashMap::new()
    };

    let mut out = Vec::new();
    for h in &diffused {
        let Some(sym) = by_id.get(&h.symbol_id) else {
            continue;
        };
        let on_path = h
            .evidence
            .get("on_path")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ppr_score = h
            .evidence
            .get("ppr_score")
            .and_then(Value::as_f64)
            .unwrap_or(h.score);
        let mut hit = search_hit(sym, h.score, "csar_diffusion");
        hit["match_sources"] = json!(["csar"]);
        hit["on_path"] = json!(on_path);
        hit["ppr_score"] = json!(ppr_score);
        if semantic_ids.contains(&h.symbol_id) {
            ctx.note_semantic_included();
        }
        out.push(hit);
    }
    Ok(Value::Array(out))
}

pub fn symbol_lookup(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let name_or_id = require_str(args, "name_or_id")?;
    let kind = arg_str(args, "kind");
    if !continue_stage(ctx, "symbol_lookup")? {
        return Ok(json!({}));
    }
    match engine.lookup(ctx, name_or_id, kind).map_err(engine_err)? {
        Some(sym) => Ok(symbol_to_dict(&sym)),
        None => Err(McpError::not_found(format!(
            "No symbol matched '{name_or_id}'"
        ))),
    }
}

pub fn dependency_trace(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let symbol_id = require_str(args, "symbol_id")?;
    let direction = arg_str(args, "direction").unwrap_or("out");
    if !matches!(direction, "out" | "in" | "both") {
        return Err(McpError::invalid_argument(
            "'direction' must be one of: out, in, both",
        ));
    }
    let depth = ctx.caps.clamp_depth(arg_i64(args, "depth").unwrap_or(3));

    let hits = if continue_stage(ctx, "dependency_trace")? {
        engine
            .dependency_trace(ctx, symbol_id, direction, depth)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };
    let by_id = if continue_stage(ctx, "dependency_trace")? {
        hydrate_map(engine, ctx, &hit_ids(&[&hits]))?
    } else {
        HashMap::new()
    };

    let mut hit_dicts = Vec::new();
    for h in &hits {
        let mut entry = json!({
            "symbol_id": h.symbol_id,
            "score": h.score,
            "depth": h.evidence.get("depth").cloned().unwrap_or(json!(null)),
        });
        if let Some(sym) = by_id.get(&h.symbol_id) {
            entry["qualified_name"] = json!(sym.qualified_name);
            entry["kind"] = json!(sym.kind);
            entry["file_path"] = json!(sym.file_path);
            entry["line_start"] = json!(sym.line_start);
            entry["line_end"] = json!(sym.line_end);
        }
        hit_dicts.push(entry);
    }

    Ok(json!({
        "start": symbol_id,
        "direction": direction,
        "depth": depth,
        "hits": hit_dicts,
    }))
}

pub fn resolve_symbols(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let ids: Vec<String> = match args.get("symbol_ids").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        None => {
            return Err(McpError::invalid_argument(
                "'symbol_ids' is required and must be an array of strings",
            ))
        }
    };
    ctx.caps.check_resolve_ids(ids.len())?;
    if !continue_stage(ctx, "resolve_symbols")? {
        return Ok(json!({
            "symbols": [],
            "missing": ids,
            "requested_count": ids.len(),
            "resolved_count": 0,
        }));
    }

    let by_id = hydrate_map(engine, ctx, &ids)?;
    let mut symbols = Vec::new();
    let mut missing = Vec::new();
    for id in &ids {
        match by_id.get(id) {
            Some(sym) => symbols.push(symbol_to_dict(sym)),
            None => missing.push(id.clone()),
        }
    }
    let resolved_count = symbols.len();
    Ok(json!({
        "symbols": symbols,
        "missing": missing,
        "requested_count": ids.len(),
        "resolved_count": resolved_count,
    }))
}

fn classify_task_mode(task: &str) -> &'static str {
    let t = task.to_lowercase();
    let has = |kws: &[&str]| kws.iter().any(|kw| t.contains(kw));
    if has(&["bug", "fix", "error", "fail", "crash", "broken"]) {
        "bugfix"
    } else if has(&["refactor", "rename", "clean up", "restructure"]) {
        "refactor"
    } else if has(&["test", "coverage", "assert"]) {
        "test"
    } else if has(&["add", "implement", "feature", "build", "create"]) {
        "feature"
    } else if has(&["how", "why", "explain", "understand", "what"]) {
        "explain"
    } else {
        "chore"
    }
}

fn estimate_tokens(text: &str) -> u32 {
    ((text.len() as u32) / 4).max(1)
}

pub fn retrieve_context_capsule(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let task = require_str(args, "task")?;
    let max_tokens = ctx
        .caps
        .clamp_tokens(arg_i64(args, "max_tokens").unwrap_or(8000));
    let task_mode = classify_task_mode(task);

    let lexical = if continue_stage(ctx, "retrieve_context_capsule")? {
        engine
            .fts_search(ctx, task, CSAR_SEED_K)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };
    let semantic =
        if continue_stage(ctx, "retrieve_context_capsule")? && engine.semantic_available(ctx) {
            engine
                .semantic_search(ctx, task, CSAR_SEED_K)
                .map_err(engine_err)?
        } else {
            Vec::new()
        };
    let csar = if continue_stage(ctx, "retrieve_context_capsule")? {
        engine
            .diffuse(
                ctx,
                &[lexical.clone(), semantic.clone()],
                CSAR_SEED_K,
                CSAR_ALPHA,
                CSAR_EPS,
            )
            .map_err(engine_err)?
    } else {
        Vec::new()
    };

    let symbol_budget = (max_tokens as usize / 50).max(1);
    let edge_context: Vec<Hit> = Vec::new();
    let composed = cognis_retrieval::compose_capsule_with_edges(
        &[lexical, semantic],
        &csar,
        &edge_context,
        symbol_budget,
        cognis_retrieval::DEFAULT_RRF_K,
        engine.integration_edge_context(ctx),
    );
    let ids = hit_ids(&[&composed]);
    let by_id = if continue_stage(ctx, "retrieve_context_capsule")? {
        hydrate_map(engine, ctx, &ids)?
    } else {
        HashMap::new()
    };

    let hydrated_vec: Vec<Symbol> = by_id.values().cloned().collect();
    let capsule_edges = ctx
        .snapshot
        .edges_incident(&ids, Direction::Both)
        .unwrap_or_default();
    let (complexity_map, _) =
        analyze_workspace_complexity(&hydrated_vec, &capsule_edges, &HashMap::new());

    let mut relevant_symbols = Vec::new();
    let mut risk_areas = Vec::new();
    let mut sources = Vec::new();
    let mut compressed = String::new();
    for h in &composed {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            let cplx = complexity_map.get(&sym.id);
            let time_str = cplx.map(|c| c.propagated_time.as_str()).unwrap_or("O(1)");
            let space_str = cplx.map(|c| c.space_complexity.as_str()).unwrap_or("O(1)");
            let cplx_risk = cplx.map(|c| c.risk_score).unwrap_or(sym.risk_score);

            relevant_symbols.push(json!({
                "symbol_id": sym.id,
                "qualified_name": sym.qualified_name,
                "kind": sym.kind,
                "file_path": sym.file_path,
                "score": h.score,
                "layer": h.layer,
                "time_complexity": time_str,
                "space_complexity": space_str,
                "risk_score": cplx_risk,
            }));
            if let Some(rep) = cplx {
                if rep.effective_degree >= 2.0 || !rep.advisories.is_empty() {
                    risk_areas.push(json!({
                        "symbol_id": sym.id,
                        "qualified_name": sym.qualified_name,
                        "file_path": sym.file_path,
                        "line_start": sym.line_start,
                        "time_complexity": rep.propagated_time,
                        "risk_score": rep.risk_score,
                        "advisories": rep.advisories,
                    }));
                }
            }
            if h.layer == "semantic" {
                ctx.note_semantic_included();
            }
            sources.push(json!(sym.file_path));
            if let Some(body) = &sym.body_excerpt {
                compressed.push_str(&format!(
                    "// {} [time: {}, space: {}]\n{}\n\n",
                    sym.qualified_name, time_str, space_str, body
                ));
            }
        }
    }
    sources.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    sources.dedup();

    let confidence = if relevant_symbols.is_empty() {
        0.0
    } else {
        0.75
    };

    let cap_chars = (max_tokens as usize) * 4;
    if compressed.len() > cap_chars {
        compressed.truncate(cap_chars);
    }
    let token_estimate = estimate_tokens(&compressed).min(max_tokens);

    Ok(json!({
        "goal": task,
        "task_mode": task_mode,
        "confidence": confidence,
        "root_cause_candidates": [],
        "relevant_symbols": relevant_symbols,
        "call_chain": [],
        "runtime_evidence": [],
        "neighbor_patterns": [],
        "risk_areas": risk_areas,
        "compressed_context": compressed,
        "sources": sources,
        "untrusted_sections": [],
        "token_estimate": token_estimate,
        "version": "1",
    }))
}

// ---------------------------------------------------------------------------
// Bounded Subgraph Hydration for Real-Time Complexity, Optimization & CSIA
// ---------------------------------------------------------------------------

/// Load a bounded subgraph (`Vec<Symbol>`, `Vec<Edge>`) around `focus_queries`
/// and `file_paths` using snapshot-bounded queries and the resident CSR graph.
fn load_analysis_subgraph(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    focus_queries: &[String],
    file_paths: &[String],
) -> Result<(Vec<Symbol>, Vec<Edge>), McpError> {
    let mut seed_ids: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for path in file_paths {
        if let Ok(ids) = ctx.snapshot.symbol_ids_in_file(path) {
            for id in ids {
                if seen_ids.insert(id.clone()) {
                    seed_ids.push(id);
                }
            }
        }
    }

    for q in focus_queries {
        let q_trim = q.trim();
        if q_trim.is_empty() {
            continue;
        }
        if let Ok(Some(sym)) = engine.lookup(ctx, q_trim, None) {
            if seen_ids.insert(sym.id.clone()) {
                seed_ids.push(sym.id);
            }
        } else if let Ok(hits) = engine.fts_search(ctx, q_trim, 10) {
            for h in hits {
                if seen_ids.insert(h.symbol_id.clone()) {
                    seed_ids.push(h.symbol_id);
                }
            }
        }
        if let Ok(file_ids) = ctx.snapshot.symbol_ids_in_file(q_trim) {
            for id in file_ids {
                if seen_ids.insert(id.clone()) {
                    seed_ids.push(id);
                }
            }
        }
    }

    // If the resident graph is small (<= ANALYSIS_SUBGRAPH_CAP) or no specific seeds matched,
    // hydrate up to ANALYSIS_SUBGRAPH_CAP nodes from the resident CSR graph.
    if let Ok(res_graph) = engine.resident_code_graph(ctx) {
        if seed_ids.is_empty() || res_graph.node_ids.len() <= ANALYSIS_SUBGRAPH_CAP {
            for id in res_graph.node_ids.iter().take(ANALYSIS_SUBGRAPH_CAP) {
                if seen_ids.insert(id.clone()) {
                    seed_ids.push(id.clone());
                }
            }
        }
    }

    // Expand 2 hops in both directions (`In` callers and `Out` callees) over active
    // execution/test edges so inter-procedural complexity and CSIA reverse blast radius
    // capture the active cone without pulling in unrelated module-containment edges!
    let mut frontier = seed_ids.clone();
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut seen_edges: HashSet<(String, String, EdgeKind)> = HashSet::new();

    for _hop in 0..2 {
        if frontier.is_empty() || seen_ids.len() >= ANALYSIS_SUBGRAPH_CAP {
            break;
        }
        let incident = ctx
            .snapshot
            .edges_incident(&frontier, Direction::Both)
            .unwrap_or_default();
        let mut next_frontier = Vec::new();
        for edge in incident {
            if matches!(edge.kind, EdgeKind::Imports) {
                continue;
            }
            if seen_edges.insert((edge.src_id.clone(), edge.dst_id.clone(), edge.kind)) {
                if seen_ids.len() < ANALYSIS_SUBGRAPH_CAP && seen_ids.insert(edge.src_id.clone()) {
                    next_frontier.push(edge.src_id.clone());
                }
                if seen_ids.len() < ANALYSIS_SUBGRAPH_CAP && seen_ids.insert(edge.dst_id.clone()) {
                    next_frontier.push(edge.dst_id.clone());
                }
                all_edges.push(edge);
            }
        }
        frontier = next_frontier;
    }

    let all_ids: Vec<String> = seen_ids.into_iter().collect();
    let mut symbols = engine.hydrate(ctx, &all_ids).map_err(engine_err)?;
    symbols.sort_by(|a, b| a.id.cmp(&b.id));

    // If the engine is a test fake whose `snapshot` is empty, synthesize edges from `dependency_trace`.
    if all_edges.is_empty() && !symbols.is_empty() {
        for sym in symbols.iter().take(16) {
            if let Ok(out_hits) = engine.dependency_trace(ctx, &sym.id, "out", 1) {
                for h in out_hits {
                    all_edges.push(Edge {
                        src_id: sym.id.clone(),
                        dst_id: h.symbol_id,
                        kind: EdgeKind::Calls,
                        confidence: 1.0,
                        meta: Value::Null,
                    });
                }
            }
        }
    }

    Ok((symbols, all_edges))
}

// ---------------------------------------------------------------------------
// 4 Flagship Real-Time Complexity, Optimization & Change Impact Tools
// ---------------------------------------------------------------------------

/// `analyze_patch_impact` — Pre-flight and mid-flight Guardrail Capsule evaluator.
/// Accepts in-memory `file_edits` (or single `file_path` + `new_content`) and/or
/// `targets` (symbol names/IDs/paths) and returns $\Delta$ Big-O complexity,
/// cross-call hidden loop amplifiers, CSIA 4D blast radius, and optimization recipes.
pub fn analyze_patch_impact(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    if !continue_stage(ctx, "analyze_patch_impact")? {
        return Ok(json!({}));
    }

    let mut file_edits: Vec<ShadowFileEdit> = Vec::new();
    if let Some(arr) = args.get("file_edits").and_then(Value::as_array) {
        for item in arr {
            if let (Some(fp), Some(content)) = (
                item.get("file_path").and_then(Value::as_str),
                item.get("new_content")
                    .or_else(|| item.get("proposed_content"))
                    .and_then(Value::as_str),
            ) {
                file_edits.push(ShadowFileEdit {
                    file_path: fp.to_string(),
                    new_content: content.to_string(),
                });
            }
        }
    }
    if let (Some(fp), Some(content)) = (
        arg_str(args, "file_path"),
        arg_str(args, "new_content").or_else(|| arg_str(args, "proposed_content")),
    ) {
        if !file_edits.iter().any(|e| e.file_path == fp) {
            file_edits.push(ShadowFileEdit {
                file_path: fp.to_string(),
                new_content: content.to_string(),
            });
        }
    }

    let mut targets: Vec<String> = Vec::new();
    if let Some(arr) = args.get("targets").and_then(Value::as_array) {
        for item in arr {
            if let Some(s) = item.as_str() {
                targets.push(s.to_string());
            }
        }
    }
    if let Some(fp) = arg_str(args, "file_path") {
        if !targets.iter().any(|t| t == fp) {
            targets.push(fp.to_string());
        }
    }
    if let Some(q) = arg_str(args, "query") {
        targets.push(q.to_string());
    }

    let file_paths: Vec<String> = file_edits.iter().map(|e| e.file_path.clone()).collect();
    let (base_symbols, base_edges) = load_analysis_subgraph(engine, ctx, &targets, &file_paths)?;

    let gen_label = if file_edits.is_empty() {
        format!("{}", ctx.pinned)
    } else {
        format!("{}+shadow", ctx.pinned)
    };

    let capsule = analyze_shadow_patch(
        &base_symbols,
        &base_edges,
        &file_edits,
        &targets,
        &gen_label,
    );

    serde_json::to_value(capsule).map_err(|e| McpError::internal(e.to_string()))
}

/// `inspect_symbol_complexity` — Deep intra + inter-procedural Big-O & bottleneck audit.
pub fn inspect_symbol_complexity(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    if !continue_stage(ctx, "inspect_symbol_complexity")? {
        return Ok(json!({}));
    }

    let k = ctx.caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));
    let mut queries: Vec<String> = Vec::new();
    if let Some(q) = arg_str(args, "name_or_id").or_else(|| arg_str(args, "query")) {
        queries.push(q.to_string());
    }
    if let Some(arr) = args.get("symbol_ids").and_then(Value::as_array) {
        for v in arr {
            if let Some(s) = v.as_str() {
                queries.push(s.to_string());
            }
        }
    }

    let (symbols, edges) = load_analysis_subgraph(engine, ctx, &queries, &[])?;
    let (reports_map, _) = analyze_workspace_complexity(&symbols, &edges, &HashMap::new());

    let mut matched_reports: Vec<_> = if queries.is_empty() {
        reports_map.into_values().collect()
    } else {
        let mut filtered = Vec::new();
        for rep in reports_map.values() {
            let hit = queries.iter().any(|q| {
                let q_trim = q.trim();
                rep.symbol_id == q_trim
                    || rep.qualified_name == q_trim
                    || rep.qualified_name.ends_with(&format!(".{q_trim}"))
                    || rep.file_path == q_trim
                    || rep.qualified_name.contains(q_trim)
            });
            if hit {
                filtered.push(rep.clone());
            }
        }
        if filtered.is_empty() {
            reports_map.into_values().collect()
        } else {
            filtered
        }
    };

    matched_reports.sort_by(|a, b| {
        b.effective_degree
            .partial_cmp(&a.effective_degree)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.risk_score
                    .partial_cmp(&a.risk_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    matched_reports.truncate(k);

    let highest_complexity = matched_reports
        .first()
        .map(|r| r.propagated_time.clone())
        .unwrap_or_else(|| "O(1)".to_string());
    let max_risk_score = matched_reports
        .iter()
        .map(|r| r.risk_score)
        .fold(0.0f64, f64::max);
    let advisory_count: usize = matched_reports.iter().map(|r| r.advisories.len()).sum();

    Ok(json!({
        "query": queries.first().cloned().unwrap_or_default(),
        "highest_complexity": highest_complexity,
        "max_risk_score": max_risk_score,
        "advisory_count": advisory_count,
        "reports": matched_reports,
    }))
}

/// `trace_blast_radius` — CSIA Multi-Channel Reverse Forward-Push Impact Analysis.
pub fn trace_blast_radius(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    let target = arg_str(args, "symbol_id")
        .or_else(|| arg_str(args, "name_or_id"))
        .or_else(|| arg_str(args, "query"))
        .or_else(|| {
            args.get("symbol_ids")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| McpError::invalid_argument("'symbol_id' (or 'name_or_id') is required"))?;
    if !continue_stage(ctx, "trace_blast_radius")? {
        return Ok(json!({}));
    }

    let sig_changed = args
        .get("signature_changed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let proposed_cplx_str = arg_str(args, "new_complexity");

    let queries = vec![target.to_string()];
    let (symbols, edges) = load_analysis_subgraph(engine, ctx, &queries, &[])?;
    let (reports_map, edge_loop_depths) =
        analyze_workspace_complexity(&symbols, &edges, &HashMap::new());

    let resolved_sym = engine
        .lookup(ctx, target, None)
        .map_err(engine_err)?
        .or_else(|| {
            symbols
                .iter()
                .find(|s| s.id == target || s.qualified_name == target || s.name == target)
                .cloned()
        });

    let Some(sym) = resolved_sym else {
        return Err(McpError::not_found(format!("No symbol matched '{target}'")));
    };

    let current_cplx = reports_map
        .get(&sym.id)
        .map(|r| parse_notation_to_class(&r.propagated_time))
        .unwrap_or_default();
    let new_cplx = proposed_cplx_str
        .map(parse_notation_to_class)
        .unwrap_or(current_cplx);

    let seed = if sig_changed {
        SymbolImpactSeed::signature_edit(
            sym.id.clone(),
            0.85,
            format!(
                "Signature or contract modification on `{}`",
                sym.qualified_name
            ),
            current_cplx,
            new_cplx,
        )
    } else {
        SymbolImpactSeed::body_edit(sym.id.clone(), current_cplx, new_cplx)
    };

    let node_ids: Vec<String> = symbols.iter().map(|s| s.id.clone()).collect();
    let sym_map: HashMap<String, Symbol> = symbols.into_iter().map(|s| (s.id.clone(), s)).collect();
    let impact_graph = ImpactGraph::build(node_ids, &edges, &edge_loop_depths);
    let impact_report = evaluate_change_impact(&impact_graph, &sym_map, &[seed], &edge_loop_depths)
        .map_err(engine_err)?;

    let mut out =
        serde_json::to_value(&impact_report).map_err(|e| McpError::internal(e.to_string()))?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("start".into(), json!(sym.id));
        obj.insert("symbol".into(), symbol_to_dict(&sym));
        obj.insert("current_complexity".into(), json!(current_cplx.notation()));
        obj.insert("simulated_complexity".into(), json!(new_cplx.notation()));
    }
    Ok(out)
}

/// `suggest_optimal_refactor` — Prescriptive Repo-Native Optimization Synthesizer.
pub fn suggest_optimal_refactor(
    engine: &dyn RetrievalEngine,
    ctx: &ToolCtx<'_>,
    args: &Value,
) -> Result<Value, McpError> {
    if !continue_stage(ctx, "suggest_optimal_refactor")? {
        return Ok(json!({}));
    }

    let target = arg_str(args, "target")
        .or_else(|| arg_str(args, "name_or_id"))
        .or_else(|| arg_str(args, "query"))
        .unwrap_or("");
    let code_snippet = arg_str(args, "code_snippet");
    let file_path = arg_str(args, "file_path").unwrap_or("src/snippet.ts");

    let mut file_edits = Vec::new();
    if let Some(snippet) = code_snippet {
        file_edits.push(ShadowFileEdit {
            file_path: file_path.to_string(),
            new_content: snippet.to_string(),
        });
    }

    let queries = if target.is_empty() {
        Vec::new()
    } else {
        vec![target.to_string()]
    };
    let edit_paths: Vec<String> = file_edits.iter().map(|e| e.file_path.clone()).collect();
    let (base_symbols, base_edges) = load_analysis_subgraph(engine, ctx, &queries, &edit_paths)?;

    let capsule = analyze_shadow_patch(
        &base_symbols,
        &base_edges,
        &file_edits,
        &queries,
        &format!("{}", ctx.pinned),
    );

    let top_report = capsule.complexity_reports.first();
    let current_complexity = top_report
        .map(|r| r.propagated_time.clone())
        .unwrap_or_else(|| "O(1)".to_string());
    let target_complexity = capsule
        .advisories
        .first()
        .map(|a| a.target_complexity.clone())
        .unwrap_or_else(|| current_complexity.clone());
    let bottleneck_chain = top_report
        .map(|r| r.bottleneck_chain.clone())
        .unwrap_or_default();
    let reusable_repo_assets: Vec<String> = capsule
        .advisories
        .iter()
        .filter_map(|a| a.repo_asset_ref.clone())
        .collect();

    Ok(json!({
        "target": target,
        "verdict": capsule.verdict,
        "current_complexity": current_complexity,
        "target_complexity": target_complexity,
        "advisories": capsule.advisories,
        "bottleneck_chain": bottleneck_chain,
        "reusable_repo_assets": reusable_repo_assets,
        "summary_markdown": capsule.summary_markdown,
    }))
}
