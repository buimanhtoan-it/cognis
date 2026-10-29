//! Property test for additive-only integration-edge capsule context.
//!
//! Feature: non-code-artifact-coverage, Property 19: Integration-edge capsule
//! context is additive-only.
//!
//! Validates: Requirements 11.1, 11.2, 11.3, 11.4, 11.5.

use std::collections::HashSet;

use cognis_core::Hit;
use cognis_retrieval::{compose_capsule, compose_capsule_with_edges, DEFAULT_RRF_K};
use proptest::prelude::*;

/// Build a `Hit` from a symbol-id index, score, layer, and source pool.
fn mk_hit(sid: &str, score: f64, layer: &str, source: &str) -> Hit {
    Hit::new(sid, score, layer, source)
}

/// A hit whose `symbol_id` is drawn from a shared, small pool so direct / CSAR
/// / edge inputs overlap and exercise the dedup path. `score` is finite and
/// bounded (RRF ranks by relative score, so magnitude is irrelevant — only the
/// ordering within a layer matters).
fn arb_hit(layer: &'static str, source: &'static str) -> impl Strategy<Value = Hit> {
    (0usize..12, -1000.0f64..1000.0f64)
        .prop_map(move |(i, score)| mk_hit(&format!("s{i}"), score, layer, source))
}

fn arb_direct_layers() -> impl Strategy<Value = Vec<Vec<Hit>>> {
    prop::collection::vec(
        prop::collection::vec(arb_hit("lexical", "match"), 0..6),
        0..4,
    )
}

fn arb_csar() -> impl Strategy<Value = Vec<Hit>> {
    prop::collection::vec(arb_hit("csar", "diffusion"), 0..6)
}

fn arb_edges() -> impl Strategy<Value = Vec<Hit>> {
    prop::collection::vec(arb_hit("integration", "edge"), 0..6)
}

fn ids_of(hits: &[Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.symbol_id.as_str()).collect()
}

fn id_set(hits: &[Hit]) -> HashSet<&str> {
    hits.iter().map(|h| h.symbol_id.as_str()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 19: Integration-edge capsule
    // context is additive-only.
    #[test]
    fn integration_edge_capsule_context_is_additive_only(
        direct_layers in arb_direct_layers(),
        csar_hits in arb_csar(),
        edge_context in arb_edges(),
        k in 0usize..15,
    ) {
        let rrf_k = DEFAULT_RRF_K;

        // Pre-feature capsule (the immutable directly-retrieved core).
        let base = compose_capsule(&direct_layers, &csar_hits, k, rrf_k);

        // ---- Flag disabled/unset: byte-for-byte identical, no edge entry ----
        let disabled =
            compose_capsule_with_edges(&direct_layers, &csar_hits, &edge_context, k, rrf_k, false);
        prop_assert_eq!(
            &disabled, &base,
            "flag-disabled capsule must be byte-for-byte identical to the pre-feature capsule"
        );

        // Every symbol in the flag-disabled capsule must come from a
        // directly-retrieved source (direct layers or CSAR); no entry may exist
        // only because it is in `edge_context` (Requirement 11.5).
        let mut direct_universe: HashSet<&str> = HashSet::new();
        for layer in &direct_layers {
            direct_universe.extend(layer.iter().map(|h| h.symbol_id.as_str()));
        }
        direct_universe.extend(csar_hits.iter().map(|h| h.symbol_id.as_str()));
        for h in &disabled {
            prop_assert!(
                direct_universe.contains(h.symbol_id.as_str()),
                "flag-disabled capsule contains {} which is not directly retrieved (edge leak)",
                h.symbol_id
            );
        }

        // ---- Flag enabled: additive-only append after the core ----
        let composed =
            compose_capsule_with_edges(&direct_layers, &csar_hits, &edge_context, k, rrf_k, true);

        // Budget is respected (Requirement 11.1 budget clause).
        prop_assert!(composed.len() <= k, "composed len {} exceeds k {}", composed.len(), k);

        // `compose_capsule` already truncates to k, so the core is a strict
        // prefix of the composed capsule: the directly-retrieved results appear
        // first, in the same order and set as the flag-disabled capsule
        // (Requirements 11.1, 11.2).
        prop_assert!(base.len() <= k);
        prop_assert!(
            composed.len() >= base.len(),
            "composed ({}) shorter than its core ({})",
            composed.len(),
            base.len()
        );
        prop_assert_eq!(
            &composed[..base.len()],
            &base[..],
            "directly-retrieved core must be preserved verbatim as the prefix"
        );

        // Every entry beyond the core is edge-derived and not already in the
        // core (Requirements 11.3, 11.4).
        let base_ids = id_set(&base);
        let edge_ids = id_set(&edge_context);
        for h in &composed[base.len()..] {
            let sid = h.symbol_id.as_str();
            prop_assert!(
                edge_ids.contains(sid),
                "appended entry {} is not from edge_context",
                sid
            );
            prop_assert!(
                !base_ids.contains(sid),
                "appended edge entry {} duplicates a directly-retrieved result",
                sid
            );
        }

        // The set of directly-retrieved symbol_ids in the composed capsule is
        // exactly that of the core — edges never remove or replace a
        // directly-retrieved result (Requirements 11.2, 11.4). The
        // directly-retrieved members of `composed` are precisely its first
        // `base.len()` entries.
        let composed_direct_ids: HashSet<&str> = composed[..base.len()]
            .iter()
            .map(|h| h.symbol_id.as_str())
            .collect();
        prop_assert_eq!(
            composed_direct_ids,
            base_ids,
            "directly-retrieved set must be identical between core and composed capsule"
        );

        // No symbol appears twice in the composed capsule.
        let unique = id_set(&composed);
        prop_assert_eq!(
            unique.len(),
            composed.len(),
            "composed capsule {:?} contains duplicate symbol_ids",
            ids_of(&composed)
        );
    }
}
