//! Reciprocal Rank Fusion — rank-based, scale-invariant cross-layer fusion.
//!
//! Pure-Rust port of `cognis_retrieval.fusion` (`fusion.py`). Each retrieval
//! layer scores hits on its *own* scale (BM25 magnitudes vs cosine in
//! `[-1, 1]`); comparing raw scores is scale-incoherent. RRF fuses on **ranks**
//! instead:
//!
//! ```text
//! rrf_score(d) = Σ_layer  1 / (rrf_k + rank_layer(d))
//! ```
//!
//! It is parameter-light (`rrf_k = 60`, Cormack et al. 2009), scale-invariant,
//! and robust to a layer emitting pathological magnitudes.
//!
//! ## Parity (Requirement 4.1 / Property 2 — P-PAR-FUSE)
//!
//! [`rrf_fuse`] mirrors `fusion.py::fuse_rankings` field-for-field so the fused
//! top-k is **byte-identical** to the Python oracle on the same seed set:
//!
//! - Hits are grouped by their `layer`, in first-appearance order across the
//!   flattened input (matching Python's insertion-ordered `dict`), so per-symbol
//!   score accumulation happens in the *same order* — float sums are bit-exact.
//! - Within a layer, hits are ranked by `(-score, symbol_id)` and each symbol
//!   contributes `1 / (rrf_k + rank)` **once** (one rank per symbol per layer).
//! - The fused list is sorted by `(-fused_score, symbol_id)` and truncated to
//!   `k`.

use cognis_core::Hit;

/// Standard RRF damping constant (Cormack et al. 2009). Mirrors
/// `fusion.py::DEFAULT_RRF_K`; not tuned to any cognis benchmark.
pub const DEFAULT_RRF_K: f64 = 60.0;

/// Fuse per-layer hit lists into one RRF-ranked list, best-first, truncated to
/// `k`.
///
/// Rank-based and scale-invariant — byte-identical to `fusion.py` on the same
/// seed set (Requirement 4.1). `layers` is a slice of per-layer hit lists;
/// hits are (re)grouped by their `layer` field so the result is independent of
/// how callers partition the input, exactly as the Python oracle groups a flat
/// list by `hit.layer`.
///
/// Each returned [`Hit`] carries `layer = "fused"`, its fused RRF score, and an
/// `evidence` payload `{"rrf_score": <score>, "rank": <1-based position>}`.
///
/// Degrades to an empty `Vec` when `k == 0`, when there are no hits, or when
/// `rrf_k <= 0` (the Python oracle raises `ValueError` on a non-positive
/// `rrf_k`; this library never panics on data and returns nothing instead).
pub fn rrf_fuse(layers: &[Vec<Hit>], k: usize, rrf_k: f64) -> Vec<Hit> {
    if k == 0 || rrf_k <= 0.0 {
        return Vec::new();
    }

    // Group hits by `layer`, preserving first-appearance order across the
    // flattened input — mirrors Python's insertion-ordered `by_layer` dict so
    // per-symbol score accumulation order (and thus the f64 sum) is identical.
    let mut layer_order: Vec<&str> = Vec::new();
    let mut grouped: Vec<Vec<&Hit>> = Vec::new();
    for hit in layers.iter().flat_map(|l| l.iter()) {
        match layer_order.iter().position(|&name| name == hit.layer) {
            Some(idx) => grouped[idx].push(hit),
            None => {
                layer_order.push(hit.layer.as_str());
                grouped.push(vec![hit]);
            }
        }
    }
    if grouped.is_empty() {
        return Vec::new();
    }

    // Accumulate fused scores. `fused` preserves first-contribution order only
    // for stability of iteration; the final ordering is by (-score, id).
    let mut fused: Vec<(String, f64)> = Vec::new();
    for layer_hits in &mut grouped {
        // Rank within the layer by descending score, ties by symbol_id — the
        // exact key `sorted(..., key=lambda h: (-h.score, h.symbol_id))`.
        layer_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.symbol_id.cmp(&b.symbol_id))
        });

        let mut rank = 0u32;
        let mut seen: Vec<&str> = Vec::new();
        for hit in layer_hits.iter() {
            if seen.contains(&hit.symbol_id.as_str()) {
                continue; // one rank per symbol per layer
            }
            seen.push(hit.symbol_id.as_str());
            rank += 1;
            let contribution = 1.0 / (rrf_k + f64::from(rank));
            match fused.iter_mut().find(|(sid, _)| sid == &hit.symbol_id) {
                Some((_, score)) => *score += contribution,
                None => fused.push((hit.symbol_id.clone(), contribution)),
            }
        }
    }

    // Final ranking: descending fused score, ties broken by symbol_id ascending
    // — mirrors `sorted(fused.items(), key=lambda kv: (-kv[1], kv[0]))`.
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused.truncate(k);

    fused
        .into_iter()
        .enumerate()
        .map(|(i, (symbol_id, score))| {
            let rank = i + 1;
            Hit::new(
                symbol_id,
                score,
                "fused",
                format!("RRF fused rank {rank} (score {score:.6})"),
            )
            .with_evidence(serde_json::json!({ "rrf_score": score, "rank": rank }))
        })
        .collect()
}

/// Return only the fused top-`k` `symbol_id`s, best-first.
///
/// Thin convenience wrapper over [`rrf_fuse`] mirroring
/// `fusion.py::reciprocal_rank_fusion` for callers (eval / live retrieval path)
/// that only need the ranked id list.
pub fn rrf_fuse_ids(layers: &[Vec<Hit>], k: usize, rrf_k: f64) -> Vec<String> {
    rrf_fuse(layers, k, rrf_k)
        .into_iter()
        .map(|h| h.symbol_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(sid: &str, score: f64, layer: &str) -> Hit {
        Hit::new(sid, score, layer, "t")
    }

    /// Empty input fuses to nothing (mirror `test_empty`).
    #[test]
    fn empty_input() {
        assert!(rrf_fuse(&[], 5, DEFAULT_RRF_K).is_empty());
        assert!(rrf_fuse(&[vec![]], 5, DEFAULT_RRF_K).is_empty());
        assert!(rrf_fuse_ids(&[], 5, DEFAULT_RRF_K).is_empty());
    }

    /// A huge-magnitude lexical score must not dominate a top semantic rank: a
    /// symbol that is rank-2 lexical + rank-1 semantic beats a rank-1-only
    /// symbol (mirror `test_scale_invariance`).
    #[test]
    fn scale_invariance() {
        let lexical = vec![hit("a", 1000.0, "lexical"), hit("b", 5.0, "lexical")];
        let semantic = vec![hit("b", 0.9, "semantic"), hit("c", 0.8, "semantic")];
        let ranked = rrf_fuse_ids(&[lexical, semantic], 3, DEFAULT_RRF_K);
        assert_eq!(ranked[0], "b");
    }

    /// Appearing in two layers beats a single rank-1 (mirror
    /// `test_appears_in_both_layers_beats_single_layer`).
    #[test]
    fn appears_in_both_layers_wins() {
        let lexical = vec![hit("x", 0.9, "lexical"), hit("y", 1.0, "lexical")];
        let semantic = vec![hit("x", 0.9, "semantic")];
        let ranked = rrf_fuse_ids(&[lexical, semantic], 2, DEFAULT_RRF_K);
        assert_eq!(ranked[0], "x");
    }

    /// Equal scores rank within-layer by symbol_id, so `a` (rank 1) outranks
    /// `b` (rank 2) (mirror `test_deterministic_tie_break_by_symbol_id`).
    #[test]
    fn deterministic_tie_break_by_symbol_id() {
        let lexical = vec![hit("b", 0.5, "lexical"), hit("a", 0.5, "lexical")];
        assert_eq!(rrf_fuse_ids(&[lexical], 2, DEFAULT_RRF_K), vec!["a", "b"]);
    }

    /// `k` truncates the result; `k == 0` yields nothing (mirror
    /// `test_k_truncation`).
    #[test]
    fn k_truncation() {
        let lexical: Vec<Hit> = "abcde"
            .chars()
            .map(|c| hit(&c.to_string(), 1.0, "lexical"))
            .collect();
        assert_eq!(
            rrf_fuse(std::slice::from_ref(&lexical), 3, DEFAULT_RRF_K).len(),
            3
        );
        assert!(rrf_fuse(&[lexical], 0, DEFAULT_RRF_K).is_empty());
    }

    /// A single rank-1 hit scores exactly `1 / (rrf_k + 1)` (mirror
    /// `test_score_formula`).
    #[test]
    fn score_formula() {
        let fused = rrf_fuse(&[vec![hit("a", 1.0, "lexical")]], 1, DEFAULT_RRF_K);
        assert_eq!(fused[0].symbol_id, "a");
        assert_eq!(fused[0].score, 1.0 / (DEFAULT_RRF_K + 1.0));
    }

    /// A non-positive `rrf_k` degrades to empty (Python raises `ValueError`;
    /// this never panics on data).
    #[test]
    fn non_positive_rrf_k_degrades_to_empty() {
        assert!(rrf_fuse(&[vec![hit("a", 1.0, "lexical")]], 5, 0.0).is_empty());
        assert!(rrf_fuse(&[vec![hit("a", 1.0, "lexical")]], 5, -1.0).is_empty());
    }
}
