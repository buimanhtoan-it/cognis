//! Seed construction + `diffuse_seed_hits` — the CSAR retrieval entry point.
//!
//! Pure-Rust port of `cognis_retrieval.csar.build_seed_distribution` and
//! `diffuse_seed_hits`. This is the shared core used by the `CSARLayer` and the
//! MCP `diffuse_context` tool: it fuses per-layer seed [`Hit`]s into a
//! normalized sparse seed distribution, runs forward-push PPR over the resident
//! [`CodeGraph`], and ranks symbols by diffused mass — tagging each returned hit
//! with `on_path`/`ppr_score` evidence (the contract shape agents depend on).
//!
//! Requirement 4.4: the structural layer must never *drop* a confident
//! lexical/semantic hit. Because every seeded node receives `alpha · seed_mass`
//! of estimate before any diffusion, seeds always survive into the ranking with
//! positive score; on-path symbols are *added* on top (additive-only union).

use std::collections::HashMap;

use cognis_core::{CodeGraph, Hit, Result};

use crate::push::approximate_ppr_push;

/// Default restart probability (interpolates semantic →1 / structural →0).
pub const DEFAULT_ALPHA: f64 = 0.15;
/// Default forward-push residual threshold.
pub const DEFAULT_EPS: f64 = 1e-5;

/// Fuse per-layer hits into a normalized sparse seed distribution.
///
/// Each layer's scores are min-max normalized to `[0, 1]` (so BM25 and cosine
/// scales contribute comparably), summed per symbol, then L1-normalized over
/// nodes present in the graph. Present-but-lowest hits keep a `1e-3` floor so
/// they still seed mass. Returns the seed as a `(node_index, mass)` vector in
/// **first-occurrence order** (mirroring the Python dict insertion order the
/// kernel was validated against) summing to 1, or an empty vector when there is
/// no usable seed mass.
pub fn build_seed_distribution(hits_per_layer: &[Vec<Hit>], g: &CodeGraph) -> Vec<(i32, f64)> {
    let mut order: Vec<usize> = Vec::new();
    let mut raw: HashMap<usize, f64> = HashMap::new();

    for hits in hits_per_layer {
        if hits.is_empty() {
            continue;
        }
        let lo = hits.iter().map(|h| h.score).fold(f64::INFINITY, f64::min);
        let hi = hits
            .iter()
            .map(|h| h.score)
            .fold(f64::NEG_INFINITY, f64::max);
        let span = hi - lo;
        for h in hits {
            let node = match g.index.get(&h.symbol_id) {
                Some(&i) => i,
                None => continue,
            };
            // Normalize within the layer; constant layers contribute 1.0 each.
            let mut norm = if span <= 0.0 {
                1.0
            } else {
                (h.score - lo) / span
            };
            if norm <= 0.0 {
                // Floor so a present-but-lowest hit still seeds mass.
                norm = 1e-3;
            }
            if !raw.contains_key(&node) {
                order.push(node);
            }
            *raw.entry(node).or_insert(0.0) += norm;
        }
    }

    // Sum in first-occurrence order to match the Python `sum(raw.values())`
    // over the insertion-ordered dict (float association parity).
    let total: f64 = order.iter().map(|node| raw[node]).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    order
        .into_iter()
        .map(|node| (node as i32, raw[&node] / total))
        .collect()
}

/// Diffuse seed hits over `g` and return the top-`k` CSAR hits.
///
/// Builds a seed distribution from `hits_per_layer`, runs forward-push PPR, and
/// ranks symbols by diffused mass (descending score, ties broken by symbol id —
/// the deterministic order the Python oracle uses). Each returned [`Hit`] has
/// `layer = "csar"` and `evidence` carrying `ppr`, `seed` (true for original
/// seed matches), `on_path` (true for symbols reached via code flow), and
/// `alpha`. Returns an empty vector when the graph is empty or there is no
/// usable seed mass.
///
/// # Errors
/// Propagates [`crate::approximate_ppr_push`]'s error when `alpha ∉ (0, 1]` or
/// `eps <= 0`.
pub fn diffuse_seed_hits(
    g: &CodeGraph,
    hits_per_layer: &[Vec<Hit>],
    k: usize,
    alpha: f64,
    eps: f64,
) -> Result<Vec<Hit>> {
    if g.is_empty() {
        return Ok(Vec::new());
    }
    let seed = build_seed_distribution(hits_per_layer, g);
    if seed.is_empty() {
        return Ok(Vec::new());
    }

    let push = approximate_ppr_push(g, &seed, alpha, eps)?;
    if push.estimate.is_empty() {
        return Ok(Vec::new());
    }

    let seed_nodes: std::collections::HashSet<i32> = seed.iter().map(|&(n, _)| n).collect();

    // Rank by (-score, node_id) — descending mass, lexicographic tie-break on
    // the symbol id, matching the Python `sorted(..., key=(-score, node_id))`.
    let mut ranked: Vec<(i32, f64)> = push.estimate.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| g.node_ids[a.0 as usize].cmp(&g.node_ids[b.0 as usize]))
    });

    let mut hits = Vec::with_capacity(k.min(ranked.len()));
    for &(node, score) in ranked.iter().take(k) {
        let symbol_id = g.node_ids[node as usize].clone();
        let on_path = !seed_nodes.contains(&node);
        let reason = format!(
            "CSAR diffusion score {score:.6}{}",
            if on_path {
                " (reached via code flow)"
            } else {
                " (seed match)"
            }
        );
        let evidence = serde_json::json!({
            "ppr": score,
            "ppr_score": score,
            "seed": !on_path,
            "on_path": on_path,
            "alpha": alpha,
        });
        hits.push(Hit::new(symbol_id, score, "csar", reason).with_evidence(evidence));
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path graph 0-1-2-3-4 (undirected, unit weights). node ids "s0".."s4".
    fn path_graph() -> CodeGraph {
        CodeGraph {
            indptr: vec![0, 1, 3, 5, 7, 8],
            indices: vec![1, 0, 2, 1, 3, 2, 4, 3],
            weights: vec![1.0; 8],
            degree: vec![1.0, 2.0, 2.0, 2.0, 1.0],
            node_ids: (0..5).map(|i| format!("s{i}")).collect(),
            index: (0..5).map(|i| (format!("s{i}"), i)).collect(),
        }
    }

    fn seed_hit(id: &str, score: f64) -> Hit {
        Hit::new(id, score, "lexical", "seed")
    }

    #[test]
    fn empty_graph_or_no_seed_yields_empty() {
        let g = path_graph();
        // No usable seed (unknown symbol ids).
        let none = diffuse_seed_hits(&g, &[vec![seed_hit("nope", 1.0)]], 10, 0.15, 1e-6).unwrap();
        assert!(none.is_empty());

        let empty = CodeGraph {
            indptr: vec![0],
            indices: vec![],
            weights: vec![],
            degree: vec![],
            node_ids: vec![],
            index: HashMap::new(),
        };
        assert!(
            diffuse_seed_hits(&empty, &[vec![seed_hit("x", 1.0)]], 5, 0.15, 1e-6)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn seed_distribution_normalizes_to_one() {
        let g = path_graph();
        let seed = build_seed_distribution(&[vec![seed_hit("s0", 3.0), seed_hit("s2", 1.0)]], &g);
        let total: f64 = seed.iter().map(|&(_, m)| m).sum();
        assert!((total - 1.0).abs() < 1e-12, "seed must L1-normalize to 1");
    }

    #[test]
    fn evidence_carries_on_path_and_ppr_score() {
        let g = path_graph();
        let hits = diffuse_seed_hits(&g, &[vec![seed_hit("s0", 1.0)]], 5, 0.15, 1e-7).unwrap();
        assert!(!hits.is_empty());
        for h in &hits {
            assert_eq!(h.layer, "csar");
            let ev = &h.evidence;
            assert!(ev.get("on_path").and_then(|v| v.as_bool()).is_some());
            assert!(ev.get("ppr_score").and_then(|v| v.as_f64()).is_some());
            assert!(ev.get("seed").and_then(|v| v.as_bool()).is_some());
        }
        // The seed node must be present and tagged seed (not on_path).
        let s0 = hits
            .iter()
            .find(|h| h.symbol_id == "s0")
            .expect("seed s0 present");
        assert_eq!(s0.evidence["on_path"], serde_json::json!(false));
        assert_eq!(s0.evidence["seed"], serde_json::json!(true));
    }

    #[test]
    fn confident_seeds_are_never_dropped() {
        // Requirement 4.4 / Property 3: the structural layer must not drop a
        // confident seed. With k ≥ #seed nodes, every seeded symbol appears in
        // the diffuse output with positive score.
        let g = path_graph();
        let seeds = vec![vec![seed_hit("s0", 2.0), seed_hit("s4", 1.0)]];
        let hits = diffuse_seed_hits(&g, &seeds, g.n(), 0.15, 1e-9).unwrap();
        let ids: std::collections::HashSet<&str> =
            hits.iter().map(|h| h.symbol_id.as_str()).collect();
        assert!(ids.contains("s0"), "seed s0 dropped");
        assert!(ids.contains("s4"), "seed s4 dropped");
        for h in &hits {
            assert!(h.score > 0.0, "diffused hit must carry positive mass");
        }
    }

    #[test]
    fn results_sorted_descending_by_score() {
        let g = path_graph();
        let hits = diffuse_seed_hits(&g, &[vec![seed_hit("s2", 1.0)]], 5, 0.15, 1e-7).unwrap();
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score, "results must be sorted desc");
        }
    }
}
