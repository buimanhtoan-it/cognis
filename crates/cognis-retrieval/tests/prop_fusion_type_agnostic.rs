//! Property test: RRF fusion is type-agnostic and edge-blind.
//!
//! Feature: non-code-artifact-coverage, Property 18: Fusion is type-agnostic
//! and edge-blind.
//!
//! Validates: Requirements 9.3, 10.1, 10.5.
//!
//! Property statement (design.md): For any mixed set of artifact and code hits,
//! `rrf_fuse` ranks them together using the identical RRF formula
//! (`rrf_k = 60`) regardless of symbol type; and for any query whose top-k
//! includes a Markdown heading symbol, that symbol's fused rank is identical
//! whether or not integration edges exist (fusion never consults an edge to
//! include, exclude, or reorder a symbol).
//!
//! `rrf_fuse` takes ONLY hit layers — it has no edge parameter, so it is
//! *structurally* edge-blind: there is no expressible way for an integration
//! edge to reach it. The `Hit` carries no "type" field either; the only thing a
//! symbol's kind (Markdown heading / config key / code function) contributes to
//! the input is its `symbol_id` namespace prefix. The test encodes the kind as
//! a `symbol_id` suffix that is *never* reachable by any ordering comparison
//! (the unique numeric key dominates), so flipping every symbol's kind provably
//! cannot change the fused output. Together with the closed-form rank-purity
//! check this shows fusion's output depends only on `(symbol_id, per-layer
//! rank)` and `rrf_k`, never on type or edges.

use cognis_retrieval::fusion::{rrf_fuse, DEFAULT_RRF_K};
use proptest::prelude::*;

/// The pinned RRF damping constant under test (`rrf_k = 60`, per the property).
const RRF_K: f64 = DEFAULT_RRF_K;

/// Symbol kind — the "type" that fusion must be blind to. `Md` models a
/// Markdown heading symbol (the clause about Markdown-rank-invariance); the
/// other two model the artifact-vs-code partition of a mixed hit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Md,
    Artifact,
    Code,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Md => "md-heading",
            Kind::Artifact => "artifact",
            Kind::Code => "code",
        }
    }

    /// Flip the artifact/code label; a Markdown heading stays a heading.
    fn flipped(self) -> Kind {
        match self {
            Kind::Md => Kind::Md,
            Kind::Artifact => Kind::Code,
            Kind::Code => Kind::Artifact,
        }
    }
}

/// Encode a `symbol_id`: a zero-padded numeric key (unique per symbol, so it
/// alone determines every lexicographic tie-break) followed by the kind tag.
/// Because the numeric prefixes of two distinct symbols always differ *within*
/// the fixed-width digits, the trailing kind tag is never reached by any
/// comparison — so relabelling kinds cannot reorder anything.
fn encode(idx: usize, kind: Kind) -> String {
    format!("{idx:08}:{}", kind.tag())
}

/// Recover the numeric key from an encoded `symbol_id` (the kind-independent
/// identity of the symbol).
fn decode_idx(symbol_id: &str) -> usize {
    symbol_id
        .split(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("encoded symbol_id has an 8-digit numeric prefix")
}

/// Per-symbol kinds: index 0 is always the Markdown heading symbol; the rest
/// are artifact/code per the generated bits.
fn kinds_of(n: usize, bits: &[bool]) -> Vec<Kind> {
    (0..n)
        .map(|i| {
            if i == 0 {
                Kind::Md
            } else if bits[i] {
                Kind::Artifact
            } else {
                Kind::Code
            }
        })
        .collect()
}

/// Build `rrf_fuse` input layers from abstract `(symbol_index, score)` entries,
/// giving each layer a distinct name so `rrf_fuse`'s group-by-layer step maps
/// one input `Vec` to one layer, in order.
fn build_layers(layers: &[Vec<(usize, i64)>], kinds: &[Kind]) -> Vec<Vec<cognis_core::Hit>> {
    layers
        .iter()
        .enumerate()
        .map(|(li, entries)| {
            entries
                .iter()
                .map(|&(idx, score)| {
                    cognis_core::Hit::new(
                        encode(idx, kinds[idx]),
                        score as f64,
                        format!("layer{li}"),
                        "t",
                    )
                })
                .collect()
        })
        .collect()
}

/// Reference fusion over abstract `(idx, score)` layers, computing each fused
/// score purely from *rank positions* and `rrf_k` — there is **no type term and
/// no edge term** anywhere in this function. It mirrors `rrf_fuse`'s float
/// accumulation order exactly (layer order, then within-layer `(-score, idx)`
/// order, one rank per symbol per layer) so the resulting `f64` scores are
/// bit-identical, letting us assert exact equality. Because `encode` makes
/// `symbol_id` ordering coincide with `idx` ordering, tie-breaking by `idx`
/// here matches `rrf_fuse`'s tie-breaking by `symbol_id`.
fn reference_fuse(layers: &[Vec<(usize, i64)>], k: usize, rrf_k: f64) -> Vec<(usize, f64)> {
    if k == 0 || rrf_k <= 0.0 {
        return Vec::new();
    }
    let mut fused: Vec<(usize, f64)> = Vec::new();
    for entries in layers {
        let mut ranked = entries.clone();
        // (-score, idx) ascending == score descending, ties by idx ascending.
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut rank: u32 = 0;
        let mut seen: Vec<usize> = Vec::new();
        for &(idx, _score) in &ranked {
            if seen.contains(&idx) {
                continue; // one rank per symbol per layer
            }
            seen.push(idx);
            rank += 1;
            let contribution = 1.0 / (rrf_k + f64::from(rank));
            match fused.iter_mut().find(|(i, _)| *i == idx) {
                Some((_, s)) => *s += contribution,
                None => fused.push((idx, contribution)),
            }
        }
    }
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused.truncate(k);
    fused
}

#[derive(Debug, Clone)]
struct Scenario {
    n: usize,
    type_bits: Vec<bool>,
    layers: Vec<Vec<(usize, i64)>>,
    k: usize,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    (2usize..=8).prop_flat_map(|n| {
        let type_bits = prop::collection::vec(any::<bool>(), n);
        let layers = prop::collection::vec(
            prop::collection::vec((0usize..n, -5i64..=5i64), 0..=n + 2),
            1..=3,
        );
        (Just(n), type_bits, layers, 1usize..=n + 2).prop_map(|(n, type_bits, layers, k)| {
            Scenario {
                n,
                type_bits,
                layers,
                k,
            }
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 18: Fusion is type-agnostic and edge-blind
    #[test]
    fn fusion_is_type_agnostic_and_edge_blind(mut sc in scenario()) {
        // Guarantee the Markdown heading symbol (idx 0) participates so the
        // Markdown-rank clause is exercised.
        if sc.layers.is_empty() {
            sc.layers.push(Vec::new());
        }
        if !sc.layers[0].iter().any(|&(idx, _)| idx == 0) {
            sc.layers[0].push((0, 0));
        }

        let kinds = kinds_of(sc.n, &sc.type_bits);
        let flipped: Vec<Kind> = kinds.iter().map(|k| k.flipped()).collect();

        let fused = rrf_fuse(&build_layers(&sc.layers, &kinds), sc.k, RRF_K);

        // --- Sub-assertion A: type-flip invariance -------------------------
        // Relabelling every symbol's kind (artifact<->code; heading stays a
        // heading) — without changing any rank position — must produce an
        // identical fused ranking. Fusion ignores type.
        let fused_flipped = rrf_fuse(&build_layers(&sc.layers, &flipped), sc.k, RRF_K);
        prop_assert_eq!(fused.len(), fused_flipped.len());
        for (a, b) in fused.iter().zip(fused_flipped.iter()) {
            prop_assert_eq!(
                decode_idx(&a.symbol_id),
                decode_idx(&b.symbol_id),
                "type flip reordered the fused ranking"
            );
            prop_assert_eq!(a.score, b.score, "type flip changed a fused score");
        }

        // --- Sub-assertion B: closed-form rank purity ----------------------
        // The fused output equals a reference computed *only* from rank
        // positions and rrf_k — no type term, no edge term exists in that
        // formula. Bit-exact because the accumulation order is mirrored.
        let reference = reference_fuse(&sc.layers, sc.k, RRF_K);
        prop_assert_eq!(fused.len(), reference.len());
        for (hit, &(idx, score)) in fused.iter().zip(reference.iter()) {
            prop_assert_eq!(decode_idx(&hit.symbol_id), idx, "fused id order != rank-only order");
            prop_assert_eq!(hit.score, score, "fused score != closed-form rank score");
        }

        // --- Sub-assertion C: edge-blind determinism -----------------------
        // `rrf_fuse` takes no edge argument, so no integration edge can reach
        // it. Constructing an arbitrary edge set referencing these symbols
        // therefore cannot be passed in and cannot change the output: a repeat
        // call with the same layers is byte-identical.
        let _edges: Vec<(String, String)> = (0..sc.n)
            .map(|idx| (encode(idx, kinds[idx]), format!("code:site:{idx}")))
            .collect();
        let fused_again = rrf_fuse(&build_layers(&sc.layers, &kinds), sc.k, RRF_K);
        prop_assert_eq!(&fused, &fused_again, "fusion is not deterministic / edge-independent");

        // --- Sub-assertion D: Markdown heading rank invariance -------------
        // When the top-k contains the Markdown heading symbol, its fused score
        // is a pure function of its per-layer ranks (the closed form), the same
        // whether or not any integration edges were built above.
        let md_id = encode(0, Kind::Md);
        if let Some(md_hit) = fused.iter().find(|h| h.symbol_id == md_id) {
            let expected_md = reference
                .iter()
                .find(|&&(idx, _)| idx == 0)
                .map(|&(_, s)| s)
                .expect("md symbol present in fused output must be present in reference");
            prop_assert_eq!(md_hit.score, expected_md, "md heading fused rank consulted an edge");
        }
    }
}
