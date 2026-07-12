//! Contract-shaped MCP tool handlers.
//!
//! Each handler composes one of the 8 tools from the read-only
//! [`RetrievalEngine`] seam and renders the **exact JSON shape** the AI agent /
//! extension depends on (Requirement 3.2, Property 4). The shapes mirror the
//! Python `cognis_mcpd.tools` field-for-field — the keys asserted by the
//! contract e2e (`tests/contract_e2e.rs`) are the invariant surface:
//!
//! | tool                       | output shape                                            |
//! | -------------------------- | ------------------------------------------------------- |
//! | `symbol_search`            | `[{symbol_id,id,name,qualified_name,kind,file_path,line_start,line_end,score,match_reason,...}]` |
//! | `semantic_search`          | same compact hit shape (empty when no vector index)     |
//! | `discover_symbols`         | hybrid hit + `match_sources` + `snippet`                |
//! | `diffuse_context`          | CSAR hit + `on_path` + `ppr_score` + `match_sources`    |
//! | `symbol_lookup`            | full serialized symbol record / error envelope          |
//! | `dependency_trace`         | `{start,direction,depth,hits:[...]}`                    |
//! | `resolve_symbols`          | `{symbols,missing,requested_count,resolved_count}`      |
//! | `retrieve_context_capsule` | the Context Capsule schema                              |
//!
//! Handlers return `Result<Value, McpError>`; the [`server`](crate::server)
//! converts an `Err` into the stable error envelope so a tool never escapes an
//! unhandled error (a tool call always *succeeds* at the protocol level and the
//! payload carries the error, exactly like the Python server).

use std::collections::HashMap;

use cognis_core::{Hit, Symbol};
use serde_json::{json, Value};

use crate::caps::Caps;
use crate::engine::RetrievalEngine;
use crate::errors::McpError;

/// CSAR forward-push defaults (mirror `COGNIS_MCP_CSAR_ALPHA/EPS`).
const CSAR_ALPHA: f64 = 0.15;
const CSAR_EPS: f64 = 1e-5;
/// Seed breadth fed into diffusion before ranking (mirror `_CSAR_SEED_K`).
const CSAR_SEED_K: usize = 25;
/// Default `k` when a caller omits it (mirror the Python tool defaults).
const DEFAULT_K: i64 = 10;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

/// Require a non-empty string argument, else `INVALID_ARGUMENT`.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, McpError> {
    match arg_str(args, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(McpError::invalid_argument(format!(
            "'{key}' is required and must be a non-empty string"
        ))),
    }
}

/// Map an engine (`CognisError`) failure into the internal error envelope code.
fn engine_err(e: cognis_core::CognisError) -> McpError {
    McpError::internal(format!("retrieval failed: {e}"))
}

// ---------------------------------------------------------------------------
// Symbol → JSON renderers (mirror `_symbol_to_dict` / `_symbol_row_to_search_hit`)
// ---------------------------------------------------------------------------

/// Full serialized symbol record (mirrors `_symbol_to_dict`).
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

/// Compact search hit from a hydrated symbol (mirrors `_symbol_row_to_search_hit`).
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

/// Hydrate `ids` into an id→symbol map (missing ids simply absent).
fn hydrate_map(
    engine: &dyn RetrievalEngine,
    ids: &[String],
) -> Result<HashMap<String, Symbol>, McpError> {
    let symbols = engine.hydrate(ids).map_err(engine_err)?;
    Ok(symbols.into_iter().map(|s| (s.id.clone(), s)).collect())
}

/// Collect the distinct `symbol_id`s across per-layer hit lists, best-first.
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

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// `symbol_search(query, k=8)` — lexical FTS5 hits, best-first.
pub fn symbol_search(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = caps.clamp_k(arg_i64(args, "k").unwrap_or(8));
    let hits = engine.fts_search(query, k).map_err(engine_err)?;
    let by_id = hydrate_map(engine, &hit_ids(&[&hits]))?;

    let mut out = Vec::new();
    for h in &hits {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            out.push(search_hit(sym, h.score, "fts_bm25"));
        }
    }
    Ok(Value::Array(out))
}

/// `semantic_search(query, k=10)` — vector KNN hits (empty without an index).
pub fn semantic_search(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));
    let hits = engine.semantic_search(query, k).map_err(engine_err)?;
    let by_id = hydrate_map(engine, &hit_ids(&[&hits]))?;

    let mut out = Vec::new();
    for h in &hits {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            let mut hit = search_hit(sym, h.score, "semantic_knn");
            hit["match_sources"] = json!(["semantic"]);
            out.push(hit);
        }
    }
    Ok(Value::Array(out))
}

/// `discover_symbols(query, k=10)` — hybrid lexical + semantic, RRF-fused.
pub fn discover_symbols(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));

    let lexical = engine.fts_search(query, k).map_err(engine_err)?;
    let semantic = engine.semantic_search(query, k).map_err(engine_err)?;

    // Which layers produced each symbol (for `match_sources`).
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

    // Rank-based RRF fusion (byte-identical to the Python oracle, Task 5.2).
    let fused = cognis_retrieval::rrf_fuse(
        &[lexical.clone(), semantic.clone()],
        k,
        cognis_retrieval::DEFAULT_RRF_K,
    );
    let by_id = hydrate_map(engine, &hit_ids(&[&fused]))?;

    let mut out = Vec::new();
    for h in &fused {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            let mut hit = search_hit(sym, h.score, "hybrid_rrf");
            let mut src = sources.get(&h.symbol_id).cloned().unwrap_or_default();
            src.sort_unstable();
            src.dedup();
            hit["match_sources"] = json!(src);
            out.push(hit);
        }
    }
    Ok(Value::Array(out))
}

/// `diffuse_context(query, k=10, alpha?, eps?)` — flagship CSAR diffusion.
///
/// Seeds the diffusion with the lexical (and semantic, when available) hits,
/// runs forward-push PPR, and returns ranked hits each carrying `on_path` /
/// `ppr_score` evidence — the contract shape agents depend on.
pub fn diffuse_context(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let query = require_str(args, "query")?;
    let k = caps.clamp_k(arg_i64(args, "k").unwrap_or(DEFAULT_K));
    let alpha = args
        .get("alpha")
        .and_then(Value::as_f64)
        .unwrap_or(CSAR_ALPHA);
    let eps = args.get("eps").and_then(Value::as_f64).unwrap_or(CSAR_EPS);

    let lexical = engine.fts_search(query, CSAR_SEED_K).map_err(engine_err)?;
    let semantic = if engine.semantic_available() {
        engine
            .semantic_search(query, CSAR_SEED_K)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };

    let seeds = vec![lexical, semantic];
    let diffused = engine.diffuse(&seeds, k, alpha, eps).map_err(engine_err)?;
    let by_id = hydrate_map(engine, &hit_ids(&[&diffused]))?;

    let mut out = Vec::new();
    for h in &diffused {
        let Some(sym) = by_id.get(&h.symbol_id) else {
            continue;
        };
        // `on_path`/`ppr_score` come from the CSAR evidence (Property 4 /
        // P-CON-DIFF). A seed match is on_path=false; a flow-reached node true.
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
        out.push(hit);
    }
    Ok(Value::Array(out))
}

/// `symbol_lookup(name_or_id, kind?)` — resolve one symbol or error envelope.
pub fn symbol_lookup(
    engine: &dyn RetrievalEngine,
    _caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let name_or_id = require_str(args, "name_or_id")?;
    let kind = arg_str(args, "kind");
    match engine.lookup(name_or_id, kind).map_err(engine_err)? {
        Some(sym) => Ok(symbol_to_dict(&sym)),
        None => Err(McpError::not_found(format!(
            "No symbol matched '{name_or_id}'"
        ))),
    }
}

/// `dependency_trace(symbol_id, direction="out", depth=3)` — call-graph trace.
pub fn dependency_trace(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let symbol_id = require_str(args, "symbol_id")?;
    let direction = arg_str(args, "direction").unwrap_or("out");
    if !matches!(direction, "out" | "in" | "both") {
        return Err(McpError::invalid_argument(
            "'direction' must be one of: out, in, both",
        ));
    }
    let depth = caps.clamp_depth(arg_i64(args, "depth").unwrap_or(3));

    let hits = engine
        .dependency_trace(symbol_id, direction, depth)
        .map_err(engine_err)?;
    let by_id = hydrate_map(engine, &hit_ids(&[&hits]))?;

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

/// `resolve_symbols(symbol_ids)` — batch-hydrate ids with counts.
pub fn resolve_symbols(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
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
    caps.check_resolve_ids(ids.len())?;

    let by_id = hydrate_map(engine, &ids)?;
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

/// Coarse task-mode classifier (mirrors the planner's 6 modes, keyword-based).
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

/// Rough token estimate for a string (≈ 4 chars/token), floored at 1 per item.
fn estimate_tokens(text: &str) -> u32 {
    ((text.len() as u32) / 4).max(1)
}

/// `retrieve_context_capsule(task, max_tokens=8000)` — composed Context Capsule.
///
/// Fuses confident lexical/semantic hits with CSAR on-path context via the
/// additive-only [`compose_capsule`](cognis_retrieval::compose_capsule), then
/// renders the invariant Context Capsule schema (version `"1"`). Empty sections
/// are emitted as empty arrays, never `null` (Python parity, CP-9).
pub fn retrieve_context_capsule(
    engine: &dyn RetrievalEngine,
    caps: &Caps,
    args: &Value,
) -> Result<Value, McpError> {
    let task = require_str(args, "task")?;
    let max_tokens = caps.clamp_tokens(arg_i64(args, "max_tokens").unwrap_or(8000));
    let task_mode = classify_task_mode(task);

    // Confident direct layers + CSAR on-path context.
    let lexical = engine.fts_search(task, CSAR_SEED_K).map_err(engine_err)?;
    let semantic = if engine.semantic_available() {
        engine
            .semantic_search(task, CSAR_SEED_K)
            .map_err(engine_err)?
    } else {
        Vec::new()
    };
    let csar = engine
        .diffuse(
            &[lexical.clone(), semantic.clone()],
            CSAR_SEED_K,
            CSAR_ALPHA,
            CSAR_EPS,
        )
        .map_err(engine_err)?;

    // Budget the capsule to ~50 tokens/symbol (mirror the Python estimate).
    let symbol_budget = (max_tokens as usize / 50).max(1);
    // Additive-only integration-edge context (Requirement 11): the
    // directly-retrieved core (confident RRF prefix + additive CSAR context) is
    // composed exactly as before; when `artifact.integration_edge_context` is
    // enabled, edge-derived entries are appended strictly after it, deduped,
    // never reordering the confident prefix. Edges are never a fused ranking
    // signal — they are not passed through `rrf_fuse`, and `rrf_k` is untouched.
    //
    // The `RoutesTo`/`Reads`/`Writes` edge-context derivation from the resident
    // graph is not yet exposed on the read-only `RetrievalEngine` seam, so the
    // edge-context slice is empty for now; with the flag defaulting to `false`
    // this path is byte-for-byte identical to the pre-feature capsule
    // (Requirement 11.5). Follow-up: extend the engine seam to surface the
    // integration-edge neighbours of the directly-retrieved symbols and pass
    // them here as `edge_context` when the flag is enabled.
    let edge_context: Vec<Hit> = Vec::new();
    let composed = cognis_retrieval::compose_capsule_with_edges(
        &[lexical, semantic],
        &csar,
        &edge_context,
        symbol_budget,
        cognis_retrieval::DEFAULT_RRF_K,
        engine.integration_edge_context(),
    );
    let by_id = hydrate_map(engine, &hit_ids(&[&composed]))?;

    let mut relevant_symbols = Vec::new();
    let mut sources = Vec::new();
    let mut compressed = String::new();
    for h in &composed {
        if let Some(sym) = by_id.get(&h.symbol_id) {
            relevant_symbols.push(json!({
                "symbol_id": sym.id,
                "qualified_name": sym.qualified_name,
                "kind": sym.kind,
                "file_path": sym.file_path,
                "score": h.score,
                "layer": h.layer,
            }));
            sources.push(json!(sym.file_path));
            if let Some(body) = &sym.body_excerpt {
                compressed.push_str(&format!("// {}\n{}\n\n", sym.qualified_name, body));
            }
        }
    }
    sources.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    sources.dedup();

    // Confidence: present when we found confident relevant symbols.
    let confidence = if relevant_symbols.is_empty() {
        0.0
    } else {
        0.75
    };

    // Token estimate ≤ max_tokens (CP-8): trim the compressed context if needed.
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
        "risk_areas": [],
        "compressed_context": compressed,
        "sources": sources,
        "untrusted_sections": [],
        "token_estimate": token_estimate,
        "version": "1",
    }))
}
