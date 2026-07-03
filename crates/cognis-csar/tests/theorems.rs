//! CSAR PPR theorems T1–T5 reproduced as property-based tests (Task 4.3).
//!
//! These are the mathematical moat (`design.md` → Correctness Properties /
//! Property 1, P-T1..P-T5; Requirement 5.1, 5.2). The theorems are **proven**
//! (machine-checked algebra); this `proptest` suite *reproduces* them on
//! randomly generated valid code graphs, mirroring the Python hypothesis/oracle
//! statements and tolerances in `tests/unit/test_csar.py`. They are the gate
//! that must pass before the CSAR Python implementation is removed (Task 11).
//!
//! The five properties (each a separate proptest) are:
//!
//! * **P-T1 existence/uniqueness** — `∀ G, α∈(0,1], seed s (‖s‖₁=1): ∃! r =
//!   α·s + (1−α)·P·r`. We confirm the exact solve *solves* the equation
//!   (existence) and that fixed-point iteration from arbitrary starting vectors
//!   converges to the same `r` (uniqueness — the map is an L1-contraction with
//!   factor `1−α`).
//! * **P-T2 geometric convergence** — power iteration error after `t` steps is
//!   `≤ (1−α)^t · ‖r₀ − r*‖₁`, independent of `n`.
//! * **P-T3 mass conservation** — `‖r‖₁ = ‖s‖₁` and `r ≥ 0`.
//! * **P-T4 endpoints** — `α→1 ⇒ r = s`; `α→0 ⇒ r → π` (the degree-proportional
//!   stationary distribution, which `P` fixes: `P·π = π`).
//! * **P-T5 forward-push invariant + work bound** — `ppr(seed) = p +
//!   ppr(residual)`, `‖ppr(seed) − p‖₁ ≤ ‖residual‖₁`, and work `Σ d_u ≤
//!   1/(α·ε)`, repo-size-independent.
//!
//! **Validates: Requirements 5.1, 5.2**

use std::collections::BTreeMap;

use cognis_csar::{
    approximate_ppr_push, personalized_pagerank_exact, transition_matrix, CodeGraph,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Graph construction (mirrors `build_code_graph`: symmetrized, parallel edges
// summed, isolated nodes carry a unit self-loop so P stays column-stochastic).
// ---------------------------------------------------------------------------

/// Build a valid resident CSR [`CodeGraph`] from `n` nodes and a raw edge list.
///
/// Self-edges (`u == v`) and out-of-range endpoints are dropped; every other
/// edge is symmetrized and parallel duplicates are coalesced by summing weights.
/// A node left with no neighbours receives a single self-loop `(u, 1.0)` with
/// `degree = 1.0`. Each row's neighbour indices come out sorted ascending (the
/// `BTreeMap` ordering), matching the kernel's expected CSR layout.
fn build_graph(n: usize, edges: &[(usize, usize, f64)]) -> CodeGraph {
    let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for &(u, v, w) in edges {
        if u == v || u >= n || v >= n || w <= 0.0 {
            continue;
        }
        *adj[u].entry(v).or_insert(0.0) += w;
        *adj[v].entry(u).or_insert(0.0) += w;
    }
    for (u, row) in adj.iter_mut().enumerate() {
        if row.is_empty() {
            row.insert(u, 1.0);
        }
    }

    let mut indptr = Vec::with_capacity(n + 1);
    let mut indices = Vec::new();
    let mut weights = Vec::new();
    let mut degree = vec![0.0f64; n];
    indptr.push(0i32);
    for (u, row) in adj.iter().enumerate() {
        let mut d = 0.0;
        for (&v, &w) in row {
            indices.push(v as i32);
            weights.push(w);
            d += w;
        }
        degree[u] = d;
        indptr.push(indices.len() as i32);
    }

    let node_ids: Vec<String> = (0..n).map(|i| format!("n{i:04}")).collect();
    let index = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();
    CodeGraph {
        indptr,
        indices,
        weights,
        degree,
        node_ids,
        index,
    }
}

/// Dense `P·r` for the row-major transition matrix `p` (length `n*n`).
fn mat_vec(p: &[f64], r: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; n];
    for (i, slot) in out.iter_mut().enumerate() {
        let base = i * n;
        let mut acc = 0.0;
        for (j, &rj) in r.iter().enumerate() {
            acc += p[base + j] * rj;
        }
        *slot = acc;
    }
    out
}

/// L1 distance between two equal-length vectors.
fn l1(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// Iterate `r ← α·s + (1−α)·P·r` from `init` until the successive-iterate L1
/// change is negligible (or a hard cap). Used to demonstrate uniqueness: the
/// fixed point is the same regardless of starting vector.
fn converge(p: &[f64], s: &[f64], alpha: f64, init: &[f64]) -> Vec<f64> {
    let n = s.len();
    let mut r = init.to_vec();
    for _ in 0..100_000 {
        let pr = mat_vec(p, &r, n);
        let nxt: Vec<f64> = (0..n)
            .map(|i| alpha * s[i] + (1.0 - alpha) * pr[i])
            .collect();
        let delta = l1(&nxt, &r);
        r = nxt;
        if delta <= 1e-13 {
            break;
        }
    }
    r
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// A randomly generated theorem case: a valid graph, valid `α`/`ε`, a
/// normalized seed distribution (`‖s‖₁ = 1`), and a single seed node for the
/// forward-push invariant.
#[derive(Debug, Clone)]
struct Case {
    g: CodeGraph,
    alpha: f64,
    eps: f64,
    seed: Vec<f64>,
    seed_node: usize,
}

/// Generate a `Case`. Graphs stay small (`n ≤ 12`) so the dense exact solve and
/// power iteration are cheap; `α ∈ [0.15, 0.9]` and `ε ∈ [1e-4, 1e-2]` keep the
/// forward-push work bound `1/(α·ε)` modest while still exercising the maths.
fn case_strategy() -> impl Strategy<Value = Case> {
    (2usize..=12)
        .prop_flat_map(|n| {
            let edges = prop::collection::vec((0..n, 0..n, 0.1f64..5.0), 0..=2 * n + 1);
            let seed = prop::collection::vec(0.0f64..=1.0, n)
                .prop_filter("seed must carry positive mass", |v| {
                    v.iter().sum::<f64>() > 1e-9
                });
            (
                Just(n),
                edges,
                0.15f64..0.9f64,
                1e-4f64..1e-2f64,
                seed,
                0..n,
            )
        })
        .prop_map(|(n, edges, alpha, eps, seed_raw, seed_node)| {
            let g = build_graph(n, &edges);
            let total: f64 = seed_raw.iter().sum();
            let seed: Vec<f64> = seed_raw.iter().map(|x| x / total).collect();
            Case {
                g,
                alpha,
                eps,
                seed,
                seed_node,
            }
        })
}

/// Generate a connected, non-bipartite (ergodic) graph for the `α→0` endpoint.
///
/// Every graph contains the triangle `0-1-2` (an odd cycle → non-bipartite) and
/// a spanning path `2-3-…-(n-1)` (→ connected), plus random extra edges. The
/// symmetrized random walk on such a graph is ergodic, so its unique stationary
/// distribution is degree-proportional and `r(α)` converges to it as `α→0`.
/// Capped at `n ≤ 8` so the spectral gap stays bounded away from 1 and the
/// `α=1e-6` PPR vector is within `1e-3` of `π`.
fn ergodic_graph() -> impl Strategy<Value = CodeGraph> {
    (3usize..=8)
        .prop_flat_map(|n| {
            let extra = prop::collection::vec((0..n, 0..n, 0.1f64..5.0), 0..=n);
            (Just(n), extra)
        })
        .prop_map(|(n, extra)| {
            let mut edges = vec![(0, 1, 1.0), (1, 2, 1.0), (0, 2, 1.0)];
            for i in 3..n {
                edges.push((i - 1, i, 1.0));
            }
            edges.extend(extra);
            build_graph(n, &edges)
        })
}

// ---------------------------------------------------------------------------
// Theorems
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// **P-T1** existence/uniqueness of `r = α·s + (1−α)·P·r`.
    ///
    /// Existence: the closed-form solve returns an `r` that satisfies the fixed
    /// point. Uniqueness: fixed-point iteration from two unrelated starting
    /// vectors (uniform, all-zero) converges to that same `r` — the map is an
    /// L1-contraction with factor `1−α < 1`, so the fixed point is unique.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_t1_existence_uniqueness(case in case_strategy()) {
        let p = transition_matrix(&case.g);
        let r = personalized_pagerank_exact(&p, &case.seed, case.alpha).unwrap();

        // Existence: r solves r = α·s + (1−α)·P·r.
        let pr = mat_vec(&p, &r, case.seed.len());
        let mapped: Vec<f64> = (0..case.seed.len())
            .map(|i| case.alpha * case.seed[i] + (1.0 - case.alpha) * pr[i])
            .collect();
        let residual = l1(&r, &mapped);
        prop_assert!(residual < 1e-6, "fixed-point residual {residual} too large");

        // Uniqueness: arbitrary initial vectors converge to the same fixed point.
        let n = case.seed.len();
        let from_uniform = converge(&p, &case.seed, case.alpha, &vec![1.0 / n as f64; n]);
        let from_zero = converge(&p, &case.seed, case.alpha, &vec![0.0; n]);
        prop_assert!(l1(&r, &from_uniform) < 1e-6, "init=uniform diverged");
        prop_assert!(l1(&r, &from_zero) < 1e-6, "init=zero diverged");
    }

    /// **P-T2** geometric convergence at rate `(1−α)`, independent of `n`.
    ///
    /// Mirrors the Python `test_geometric_convergence_rate`: from `r₀ = s`, the
    /// L1 error after `t` power steps is `≤ (1−α)^t · ‖r₀ − r*‖₁`.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_t2_geometric_convergence(case in case_strategy()) {
        let n = case.seed.len();
        let p = transition_matrix(&case.g);
        let r_star = personalized_pagerank_exact(&p, &case.seed, case.alpha).unwrap();

        let mut r = case.seed.clone();
        let err0 = l1(&r, &r_star);
        for t in 1..=6 {
            let pr = mat_vec(&p, &r, n);
            r = (0..n)
                .map(|i| case.alpha * case.seed[i] + (1.0 - case.alpha) * pr[i])
                .collect();
            let err = l1(&r, &r_star);
            let bound = (1.0 - case.alpha).powi(t) * err0;
            prop_assert!(
                err <= bound + 1e-9,
                "iter {t}: err {err} exceeds geometric bound {bound}"
            );
        }
    }

    /// **P-T3** mass conservation `‖r‖₁ = ‖s‖₁` and non-negativity.
    ///
    /// The seed is L1-normalized to 1, `P` is column-stochastic, so the exact
    /// PPR vector must sum to 1 and stay non-negative.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_t3_mass_conservation(case in case_strategy()) {
        let p = transition_matrix(&case.g);
        let r = personalized_pagerank_exact(&p, &case.seed, case.alpha).unwrap();
        let mass: f64 = r.iter().sum();
        prop_assert!((mass - 1.0).abs() < 1e-9, "‖r‖₁ = {mass}, expected 1");
        prop_assert!(
            r.iter().all(|&v| v >= -1e-9),
            "PPR vector must be non-negative: {r:?}"
        );
    }

    /// **P-T4** endpoint limits.
    ///
    /// `α=1 ⇒ r = s` (no diffusion). `α→0 ⇒ r → π`, the degree-proportional
    /// stationary distribution, which the transition matrix fixes (`P·π = π`).
    /// Uses ergodic graphs so the `α→0` limit is the unique `π`.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_t4_endpoints(g in ergodic_graph()) {
        let n = g.n();
        let p = transition_matrix(&g);

        // α = 1 ⇒ r = s.
        let mut s = vec![0.0f64; n];
        s[0] = 1.0;
        let r_one = personalized_pagerank_exact(&p, &s, 1.0).unwrap();
        prop_assert!(l1(&r_one, &s) < 1e-9, "α=1 must return the seed");

        // π = degree / Σ degree is stationary: P·π = π.
        let dsum: f64 = g.degree.iter().sum();
        let pi: Vec<f64> = g.degree.iter().map(|d| d / dsum).collect();
        let p_pi = mat_vec(&p, &pi, n);
        prop_assert!(l1(&p_pi, &pi) < 1e-9, "π must be stationary (P·π = π)");

        // α → 0 ⇒ r → π.
        let r_tiny = personalized_pagerank_exact(&p, &s, 1e-6).unwrap();
        let max_abs = r_tiny
            .iter()
            .zip(&pi)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        prop_assert!(
            max_abs < 1e-3,
            "α→0 must approach stationary π; max|r − π| = {max_abs}"
        );
    }

    /// **P-T5** forward-push invariant + work bound.
    ///
    /// T5a: `ppr(seed) = p + ppr(residual)` (linear-algebra identity). T5b:
    /// `‖ppr(seed) − p‖₁ ≤ ‖residual‖₁` at termination. T5c: work `Σ d_u ≤
    /// 1/(α·ε)`, independent of graph size.
    ///
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_t5_forward_push_invariant_and_work_bound(case in case_strategy()) {
        let n = case.g.n();
        let p = transition_matrix(&case.g);
        let push =
            approximate_ppr_push(&case.g, &[(case.seed_node as i32, 1.0)], case.alpha, case.eps)
                .unwrap();

        // T5c: work bound (repo-size-independent).
        let bound = 1.0 / (case.alpha * case.eps);
        prop_assert!(
            push.work <= bound + 1e-9,
            "work {} exceeds bound {bound}",
            push.work
        );

        // Reconstruct dense p / residual vectors from the sparse push result.
        let mut s = vec![0.0f64; n];
        s[case.seed_node] = 1.0;
        let mut p_vec = vec![0.0f64; n];
        for (&node, &m) in &push.estimate {
            p_vec[node as usize] = m;
        }
        let mut resid_vec = vec![0.0f64; n];
        for (&node, &m) in &push.residual {
            resid_vec[node as usize] = m;
        }

        // T5a: ppr(seed) = p + ppr(residual).
        let lhs = personalized_pagerank_exact(&p, &s, case.alpha).unwrap();
        let ppr_resid = personalized_pagerank_exact(&p, &resid_vec, case.alpha).unwrap();
        let rhs: Vec<f64> = p_vec.iter().zip(&ppr_resid).map(|(a, b)| a + b).collect();
        let invariant = l1(&lhs, &rhs);
        prop_assert!(invariant < 1e-7, "T5a invariant L1 {invariant} too large");

        // T5b: ‖ppr(seed) − p‖₁ ≤ ‖residual‖₁.
        let approx_err = l1(&lhs, &p_vec);
        let resid_mass: f64 = resid_vec.iter().sum();
        prop_assert!(
            approx_err <= resid_mass + 1e-7,
            "T5b ‖ppr−p‖₁ {approx_err} exceeds residual mass {resid_mass}"
        );
    }
}
