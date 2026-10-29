//! Capsule composer monotonicity test (rust-engine-migration, task 5.3 /
//! Requirement 4.4).
//!
//! Asserts the additive-only union the capsule composer performs is **monotone
//! with respect to the RRF-direct ranking** — Property 3 (Monotonicity / low
//! contamination): UNION/CSAR-as-context never pushes out a confident
//! lexical/semantic hit, so `recall ≥ direct prefix`.
//!
//! Two layers of evidence:
//!
//! 1. A `proptest` over arbitrary direct layers + arbitrary CSAR context +
//!    arbitrary budget `k`, asserting the four monotonicity invariants below.
//! 2. An end-to-end case that drives the *real* CSAR kernel
//!    (`cognis_csar::diffuse_seed_hits`) over a code graph, so the property is
//!    exercised against the on-path/`ppr_score` evidence the live path emits —
//!    not just synthetic CSAR hits.
//!
//! Invariants (for every input and every `k`):
//! - **Prefix**: the RRF-direct top-k is a verbatim prefix of the composed
//!   capsule (RRF order preserved, confident hits first).
//! - **Recall ≥ direct**: every confident hit in the direct ranking survives in
//!   the composed capsule (recall of the direct ranking is 1.0 — a superset).
//! - **Dedup**: no `symbol_id` appears twice.
//! - **Budget**: the composed capsule never exceeds `k`.

use cognis_core::{CodeGraph, Hit};
use cognis_csar::diffuse_seed_hits;
use cognis_retrieval::{compose_capsule_ids, rrf_fuse_ids, DEFAULT_RRF_K};
use proptest::prelude::*;
use std::collections::HashSet;

/// Split a flat list of `(symbol_id, score, is_semantic)` triples into the two
/// confident direct layers (lexical / semantic) the composer fuses.
fn direct_layers(rows: &[(String, f64, bool)]) -> Vec<Vec<Hit>> {
    let mut lexical = Vec::new();
    let mut semantic = Vec::new();
    for (sid, score, is_sem) in rows {
        if *is_sem {
            semantic.push(Hit::new(sid.clone(), *score, "semantic", "t"));
        } else {
            lexical.push(Hit::new(sid.clone(), *score, "lexical", "t"));
        }
    }
    vec![lexical, semantic]
}

fn csar_context(rows: &[(String, f64)]) -> Vec<Hit> {
    rows.iter()
        .map(|(sid, score)| {
            Hit::new(sid.clone(), *score, "csar", "diffusion")
                .with_evidence(serde_json::json!({ "ppr_score": score, "on_path": true }))
        })
        .collect()
}

/// Core assertion: the composed capsule is a monotone, additive-only superset
/// of the RRF-direct ranking.
fn assert_monotone(layers: &[Vec<Hit>], csar: &[Hit], k: usize) -> Result<(), TestCaseError> {
    let direct = rrf_fuse_ids(layers, k, DEFAULT_RRF_K);
    let composed = compose_capsule_ids(layers, csar, k, DEFAULT_RRF_K);

    // Prefix: RRF order preserved, confident hits first.
    prop_assert!(
        composed.starts_with(&direct),
        "k={k}: direct ranking {direct:?} is not a prefix of composed {composed:?}"
    );

    // Recall ≥ direct prefix: every confident hit survives (superset).
    let composed_set: HashSet<&String> = composed.iter().collect();
    for sid in &direct {
        prop_assert!(
            composed_set.contains(sid),
            "k={k}: confident hit {sid:?} dropped from composed {composed:?}"
        );
    }

    // Dedup per-symbol.
    prop_assert!(
        composed_set.len() == composed.len(),
        "k={k}: composed {composed:?} contains a duplicate symbol"
    );

    // Budget honoured.
    prop_assert!(
        composed.len() <= k,
        "k={k}: composed {composed:?} exceeds budget"
    );

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `recall ≥ direct prefix` holds for arbitrary direct layers, arbitrary
    /// CSAR context, and arbitrary budget.
    #[test]
    fn composed_capsule_is_monotone_over_direct_ranking(
        direct in proptest::collection::vec(
            (0usize..12, -5.0f64..5.0, any::<bool>()),
            0..24,
        ),
        csar in proptest::collection::vec((0usize..16, 0.0f64..1.0), 0..16),
        k in 1usize..16,
    ) {
        let direct_rows: Vec<(String, f64, bool)> = direct
            .into_iter()
            .map(|(i, s, sem)| (format!("s{i}"), s, sem))
            .collect();
        let csar_rows: Vec<(String, f64)> =
            csar.into_iter().map(|(i, s)| (format!("s{i}"), s)).collect();

        let layers = direct_layers(&direct_rows);
        let csar_hits = csar_context(&csar_rows);
        assert_monotone(&layers, &csar_hits, k)?;
    }
}

/// Path graph 0-1-2-3-4-5 (undirected, unit weights), node ids "s0".."s5".
fn path_graph() -> CodeGraph {
    CodeGraph {
        indptr: vec![0, 1, 3, 5, 7, 9, 10],
        indices: vec![1, 0, 2, 1, 3, 2, 4, 3, 5, 4],
        weights: vec![1.0; 10],
        degree: vec![1.0, 2.0, 2.0, 2.0, 2.0, 1.0],
        node_ids: (0..6).map(|i| format!("s{i}")).collect(),
        index: (0..6).map(|i| (format!("s{i}"), i)).collect(),
    }
}

/// End-to-end: feed real CSAR diffusion output (with genuine `on_path`/
/// `ppr_score` evidence) into the composer and confirm monotonicity holds — the
/// structural layer adds on-path context without dropping the confident seed.
#[test]
fn monotone_with_real_csar_diffusion() {
    let g = path_graph();

    // Confident direct hits: a strong lexical match on s0 and a semantic match
    // on s2 (the seeds the live path would diffuse from).
    let lexical = vec![Hit::new("s0", 4.2, "lexical", "bm25")];
    let semantic = vec![Hit::new("s2", 0.87, "semantic", "cosine")];
    let layers = vec![lexical, semantic];

    // Real CSAR kernel over the resident graph — adds on-path neighbours.
    let csar = diffuse_seed_hits(&g, &layers, g.n(), 0.15, 1e-9).expect("diffuse");
    assert!(
        csar.iter()
            .any(|h| h.evidence["on_path"] == serde_json::json!(true)),
        "expected at least one on-path CSAR context hit"
    );

    for k in 1..=8usize {
        let direct = rrf_fuse_ids(&layers, k, DEFAULT_RRF_K);
        let composed = compose_capsule_ids(&layers, &csar, k, DEFAULT_RRF_K);

        assert!(
            composed.starts_with(&direct),
            "k={k}: direct {direct:?} not a prefix of composed {composed:?}"
        );
        let composed_set: HashSet<&String> = composed.iter().collect();
        for sid in &direct {
            assert!(
                composed_set.contains(sid),
                "k={k}: confident hit {sid:?} dropped"
            );
        }
        assert_eq!(
            composed_set.len(),
            composed.len(),
            "k={k}: duplicate symbol"
        );
        assert!(composed.len() <= k, "k={k}: over budget");
    }

    // With ample budget the confident seeds plus on-path context all compose.
    let composed = compose_capsule_ids(&layers, &csar, 8, DEFAULT_RRF_K);
    assert!(composed.contains(&"s0".to_string()), "seed s0 dropped");
    assert!(composed.contains(&"s2".to_string()), "seed s2 dropped");
    assert!(
        composed.len() > 2,
        "expected CSAR on-path context to be added, got {composed:?}"
    );
}
