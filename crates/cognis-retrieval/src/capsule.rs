//! Capsule composer — additive-only union of confident retrieval hits + CSAR
//! on-path context.
//!
//! This is the K3 retrieval-level composer (the *ranking* core consumed by the
//! MCP `retrieve_context_capsule` / `diffuse_context` surface in Task 7; the
//! full schema-bearing capsule — sections, token budget, source attachment —
//! lives in the MCP layer). It takes the confident lexical/semantic layers and
//! the CSAR on-path context, and produces a single deduplicated, RRF-ordered
//! list where structural context is *added*, never substituted.
//!
//! ## Composition rules (design §Correctness Properties, Property 3)
//!
//! 1. **Dedup per-symbol.** Each `symbol_id` appears at most once. When a
//!    symbol is both a confident direct hit and reached by CSAR (a seed match,
//!    or a coincidental on-path node), the confident direct hit wins — the CSAR
//!    duplicate is dropped.
//! 2. **RRF order (confident prefix).** The confident hits come first, in the
//!    exact order [`rrf_fuse`] produces over the direct layers. This prefix is
//!    byte-identical to the standalone RRF-direct ranking.
//! 3. **CSAR on-path add (additive-only union).** CSAR context hits that are
//!    not already confident direct hits are *appended after* the direct prefix,
//!    in CSAR's own descending-score order. They fill the remaining budget; they
//!    never displace or reorder a confident hit.
//!
//! ## Monotonicity (Requirement 4.4 / Property 3 — proven by construction)
//!
//! Because the union is strictly additive — the confident RRF-direct ranking is
//! a *prefix* of the composed capsule and CSAR only appends new symbols — the
//! composer can never push out a confident lexical/semantic hit:
//!
//! ```text
//! recall(composed_top_k ⊇ direct_top_k) = 1.0   for every k
//! ```
//!
//! i.e. the composed capsule's confident hits are a prefix-preserving superset
//! of the RRF-direct ranking (`recall ≥ direct prefix`). The Python composer
//! fused *all* layers (including CSAR) into one RRF pass, which let structural
//! mass reorder — and silently drop — a confident hit; reimplementing the union
//! as additive-only restores the monotonicity guarantee. See
//! `tests/capsule_monotonicity.rs`.

use std::collections::HashSet;

use cognis_core::Hit;

use crate::fusion::rrf_fuse;

/// Compose a capsule's ranked hit list from the confident direct layers and
/// CSAR on-path context.
///
/// `direct_layers` are the lexical/semantic (non-CSAR) per-layer hit lists;
/// they are fused with [`rrf_fuse`] into the confident, deduplicated RRF
/// ranking. `csar_hits` are the CSAR context hits (from
/// `cognis_csar::diffuse_seed_hits`, carrying `on_path`/`ppr_score` evidence),
/// already sorted best-first.
///
/// The result is one hit per `symbol_id`, truncated to `k`: the RRF-direct
/// ranking first (each hit `layer = "fused"`), then CSAR context hits not
/// already present (each retaining its `layer = "csar"` evidence), appended in
/// CSAR order. The confident prefix is never reordered or dropped in favour of
/// CSAR context — the union is additive-only (Requirement 4.4).
///
/// Degrades to an empty `Vec` when `k == 0`. A non-positive `rrf_k` yields an
/// empty direct ranking (mirroring [`rrf_fuse`]); CSAR context still composes
/// on top so the capsule never panics on data.
pub fn compose_capsule(
    direct_layers: &[Vec<Hit>],
    csar_hits: &[Hit],
    k: usize,
    rrf_k: f64,
) -> Vec<Hit> {
    if k == 0 {
        return Vec::new();
    }

    // Rule 2: confident RRF-direct ranking (already deduplicated per symbol and
    // truncated to k by `rrf_fuse`). This is the prefix the composed capsule
    // preserves verbatim — the source of the monotonicity guarantee.
    let mut composed = rrf_fuse(direct_layers, k, rrf_k);

    // Rule 1: track symbols already in the confident prefix so CSAR cannot add
    // a duplicate (direct wins).
    let mut seen: HashSet<&str> = composed.iter().map(|h| h.symbol_id.as_str()).collect();

    // Rule 3: append CSAR context additively, in CSAR's own order, until the
    // budget is full. We collect ids to add first to avoid borrowing `composed`
    // both mutably and immutably.
    let mut to_add: Vec<&Hit> = Vec::new();
    for hit in csar_hits {
        if composed.len() + to_add.len() >= k {
            break;
        }
        if seen.insert(hit.symbol_id.as_str()) {
            to_add.push(hit);
        }
    }
    composed.extend(to_add.into_iter().cloned());

    composed
}

/// Return only the composed capsule's `symbol_id`s, best-first.
///
/// Thin convenience wrapper over [`compose_capsule`] for callers that only need
/// the ranked id list (eval / live retrieval paths).
pub fn compose_capsule_ids(
    direct_layers: &[Vec<Hit>],
    csar_hits: &[Hit],
    k: usize,
    rrf_k: f64,
) -> Vec<String> {
    compose_capsule(direct_layers, csar_hits, k, rrf_k)
        .into_iter()
        .map(|h| h.symbol_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fusion::{rrf_fuse_ids, DEFAULT_RRF_K};

    fn hit(sid: &str, score: f64, layer: &str) -> Hit {
        Hit::new(sid, score, layer, "t")
    }

    fn csar_hit(sid: &str, score: f64, on_path: bool) -> Hit {
        Hit::new(sid, score, "csar", "diffusion").with_evidence(
            serde_json::json!({ "ppr_score": score, "on_path": on_path, "seed": !on_path }),
        )
    }

    /// `k == 0` composes to nothing.
    #[test]
    fn k_zero_is_empty() {
        let lexical = vec![hit("a", 1.0, "lexical")];
        let csar = vec![csar_hit("b", 0.5, true)];
        assert!(compose_capsule(&[lexical], &csar, 0, DEFAULT_RRF_K).is_empty());
    }

    /// The confident RRF-direct ranking is preserved verbatim as the prefix of
    /// the composed capsule.
    #[test]
    fn direct_ranking_is_the_prefix() {
        let lexical = vec![hit("a", 0.9, "lexical"), hit("b", 0.5, "lexical")];
        let semantic = vec![hit("b", 0.95, "semantic"), hit("c", 0.8, "semantic")];
        let direct = rrf_fuse_ids(&[lexical.clone(), semantic.clone()], 10, DEFAULT_RRF_K);

        let csar = vec![csar_hit("d", 0.7, true)];
        let composed = compose_capsule_ids(&[lexical, semantic], &csar, 10, DEFAULT_RRF_K);

        assert!(
            composed.starts_with(&direct),
            "direct ranking {direct:?} must be a prefix of composed {composed:?}"
        );
    }

    /// CSAR on-path hits are appended additively after the direct prefix when
    /// the budget has room.
    #[test]
    fn csar_on_path_added_additively() {
        let lexical = vec![hit("a", 0.9, "lexical")];
        let csar = vec![csar_hit("x", 0.8, true), csar_hit("y", 0.6, true)];
        let composed = compose_capsule_ids(&[lexical], &csar, 10, DEFAULT_RRF_K);
        assert_eq!(composed, vec!["a", "x", "y"]);
    }

    /// A symbol that is both a confident direct hit and a CSAR seed appears
    /// once — the confident direct hit wins (dedup per-symbol).
    #[test]
    fn duplicate_symbol_deduped_direct_wins() {
        let lexical = vec![hit("a", 0.9, "lexical")];
        // "a" is also a CSAR seed (on_path=false) and there is one genuine
        // on-path addition "b".
        let csar = vec![csar_hit("a", 1.0, false), csar_hit("b", 0.7, true)];
        let composed = compose_capsule(&[lexical], &csar, 10, DEFAULT_RRF_K);
        let ids: Vec<&str> = composed.iter().map(|h| h.symbol_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        // The surviving "a" is the confident direct hit, not the CSAR duplicate.
        assert_eq!(composed[0].layer, "fused");
        assert_eq!(composed[1].layer, "csar");
    }

    /// CSAR context never displaces a confident hit even when its score dwarfs
    /// the direct hits: with the budget full of confident hits, no CSAR context
    /// is admitted.
    #[test]
    fn csar_never_displaces_confident_hit() {
        let lexical = vec![hit("a", 0.9, "lexical"), hit("b", 0.5, "lexical")];
        let direct = rrf_fuse_ids(std::slice::from_ref(&lexical), 2, DEFAULT_RRF_K);
        // High-scoring CSAR context, but k == 2 is fully consumed by confident
        // hits.
        let csar = vec![csar_hit("z", 9999.0, true)];
        let composed = compose_capsule_ids(&[lexical], &csar, 2, DEFAULT_RRF_K);
        assert_eq!(composed, direct, "confident hits must not be displaced");
        assert!(!composed.contains(&"z".to_string()));
    }

    /// CSAR fills only the budget remaining after the confident hits.
    #[test]
    fn csar_fills_remaining_budget_only() {
        let lexical = vec![hit("a", 0.9, "lexical"), hit("b", 0.5, "lexical")];
        let csar = vec![csar_hit("x", 0.8, true), csar_hit("y", 0.7, true)];
        // k == 3: 2 confident + room for exactly 1 CSAR add.
        let composed = compose_capsule_ids(&[lexical], &csar, 3, DEFAULT_RRF_K);
        assert_eq!(composed, vec!["a", "b", "x"]);
    }

    /// No symbol appears twice in the composed capsule.
    #[test]
    fn composed_has_no_duplicates() {
        let lexical = vec![hit("a", 0.9, "lexical"), hit("b", 0.5, "lexical")];
        let semantic = vec![hit("a", 0.8, "semantic")];
        let csar = vec![csar_hit("a", 1.0, false), csar_hit("b", 0.6, false)];
        let composed = compose_capsule_ids(&[lexical, semantic], &csar, 10, DEFAULT_RRF_K);
        let unique: HashSet<&String> = composed.iter().collect();
        assert_eq!(
            unique.len(),
            composed.len(),
            "composed {composed:?} has dups"
        );
    }
}
