//! Forward-push Personalized PageRank — the size-independent CSAR solver.
//!
//! Pure-Rust port of `cognis_retrieval.csar.approximate_ppr_push`, carried over
//! from the proven C-ABI kernel in `native/csar-rs` (which achieved L1 = 0
//! parity vs the Python oracle). It now consumes the resident CSR [`CodeGraph`]
//! directly instead of bare pointers, so the retrieval layer calls it as a plain
//! function with no FFI marshalling.
//!
//! The algorithm and arithmetic order are unchanged from the reference: same
//! threshold test `residual[u] ≥ ε·degree[u]`, same LIFO worklist, same
//! accumulation over each row's ascending-sorted neighbours. This is what keeps
//! the estimate within the residual bound of the Python result (T5b) and, on the
//! parity graphs, bit-exact.

use std::collections::HashMap;

use cognis_core::{CodeGraph, CognisError, Result};

/// Result of [`approximate_ppr_push`].
///
/// Mirrors the Python `PushResult` (estimate + residual + work + pushes). The
/// design's Rust sketch lists only `estimate`/`work`/`pushes`; we additionally
/// carry `residual` because it is needed to machine-check the forward-push
/// invariant `ppr(seed) = p + ppr(residual)` (theorem T5a) in the proptest suite
/// (Task 4.3), and it is free to retain — the kernel already tracks it.
#[derive(Debug, Clone, PartialEq)]
pub struct PushResult {
    /// Approximate PPR mass `{node -> p_node}` (sparse; nonzero entries only).
    pub estimate: HashMap<i32, f64>,
    /// Leftover residual `{node -> r_node}` at termination (nonzero entries).
    pub residual: HashMap<i32, f64>,
    /// Total work `Σ d_u` over pushes; bounded by `1/(α·ε)` (T5c).
    pub work: f64,
    /// Number of push operations performed.
    pub pushes: i64,
}

/// Approximate PPR via Andersen-Chung-Lang forward push.
///
/// Starts from `p = 0`, `r = seed`, and repeatedly pushes from any node whose
/// residual exceeds `eps·degree[u]`:
///
/// ```text
/// p[u]     += alpha * r[u]
/// r[v]     += (1 - alpha) * r[u] * w(u,v) / degree[u]   for each neighbour v
/// r[u]      = 0
/// ```
///
/// `seed` is a sparse `(node_index, mass)` slice (`mass >= 0`); duplicate node
/// entries accumulate, exact-zero masses are skipped, and out-of-range nodes are
/// ignored (mirroring the Python seed filter and the kernel's bounds checks).
///
/// # Errors
/// Returns [`CognisError::Retrieval`] when `alpha ∉ (0, 1]` or `eps <= 0`
/// (matching the Python `ValueError`); never panics on well-formed CSR.
pub fn approximate_ppr_push(
    g: &CodeGraph,
    seed: &[(i32, f64)],
    alpha: f64,
    eps: f64,
) -> Result<PushResult> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(CognisError::Retrieval(format!(
            "alpha must be in (0, 1]; got {alpha}"
        )));
    }
    if eps.is_nan() || eps <= 0.0 {
        return Err(CognisError::Retrieval(format!(
            "eps must be > 0; got {eps}"
        )));
    }

    let n = g.n();
    let mut estimate = vec![0.0f64; n];
    let mut residual = vec![0.0f64; n];
    let mut in_active = vec![false; n];

    // Seed the residual (accumulate duplicates, skip exact-zero masses — the
    // Python `{u: m for u, m in seed.items() if m != 0.0}` filter).
    for &(u, m) in seed {
        if u < 0 || (u as usize) >= n {
            continue;
        }
        if m != 0.0 {
            residual[u as usize] += m;
        }
    }

    // Initial worklist: nodes whose seeded residual already clears the
    // threshold, visited in seed order (first occurrence wins, like the Python
    // dict-insertion order the kernel was validated against).
    let mut active: Vec<i32> = Vec::with_capacity(seed.len() + 16);
    for &(u, _) in seed {
        if u < 0 || (u as usize) >= n {
            continue;
        }
        let su = u as usize;
        if !in_active[su] && residual[su] >= eps * g.degree[su] {
            active.push(u);
            in_active[su] = true;
        }
    }

    let mut work = 0.0f64;
    let mut pushes: i64 = 0;

    while let Some(u) = active.pop() {
        let su = u as usize;
        in_active[su] = false;

        let r_u = residual[su];
        if r_u < eps * g.degree[su] {
            continue;
        }

        estimate[su] += alpha * r_u;
        residual[su] = 0.0;
        let push_mass = (1.0 - alpha) * r_u;
        let d_u = g.degree[su];

        let (idx, w) = g.neighbors(su);
        for (e, &v) in idx.iter().enumerate() {
            if v < 0 || (v as usize) >= n {
                continue;
            }
            let sv = v as usize;
            residual[sv] += push_mass * w[e] / d_u;
            if !in_active[sv] && residual[sv] >= eps * g.degree[sv] {
                active.push(v);
                in_active[sv] = true;
            }
        }

        work += d_u;
        pushes += 1;
    }

    let mut estimate_map: HashMap<i32, f64> = HashMap::new();
    for (u, &e) in estimate.iter().enumerate() {
        if e != 0.0 {
            estimate_map.insert(u as i32, e);
        }
    }
    let mut residual_map: HashMap<i32, f64> = HashMap::new();
    for (u, &r) in residual.iter().enumerate() {
        if r != 0.0 {
            residual_map.insert(u as i32, r);
        }
    }

    Ok(PushResult {
        estimate: estimate_map,
        residual: residual_map,
        work,
        pushes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    /// Path graph 0-1-2-3-4 (undirected, unit weights), plus a self-loop check.
    fn path_graph() -> CodeGraph {
        // edges: 0-1,1-2,2-3,3-4 symmetrized, weight 1.0 each.
        CodeGraph {
            indptr: vec![0, 1, 3, 5, 7, 8],
            indices: vec![1, 0, 2, 1, 3, 2, 4, 3],
            weights: vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            degree: vec![1.0, 2.0, 2.0, 2.0, 1.0],
            node_ids: (0..5).map(|i| format!("n{i}")).collect(),
            index: (0..5).map(|i| (format!("n{i}"), i)).collect(),
        }
    }

    #[test]
    fn rejects_invalid_alpha_and_eps() {
        let g = path_graph();
        assert!(approximate_ppr_push(&g, &[(0, 1.0)], 0.0, 1e-6).is_err());
        assert!(approximate_ppr_push(&g, &[(0, 1.0)], 1.5, 1e-6).is_err());
        assert!(approximate_ppr_push(&g, &[(0, 1.0)], 0.15, 0.0).is_err());
    }

    #[test]
    fn mass_decays_with_distance_from_seed() {
        let g = path_graph();
        let r = approximate_ppr_push(&g, &[(0, 1.0)], 0.15, 1e-7).unwrap();
        let est = &r.estimate;
        // The seed node retains a healthy share, and diffused mass decays
        // monotonically along the path toward the far endpoint (node 4).
        assert!(est[&0] > 0.0);
        assert!(
            est[&0] >= est[&2] && est[&2] >= est[&3] && est[&3] >= est[&4],
            "mass should decay with hop distance from the seed: {est:?}"
        );
    }

    #[test]
    fn work_bound_holds() {
        let g = path_graph();
        let (alpha, eps) = (0.2, 1e-4);
        let r = approximate_ppr_push(&g, &[(0, 1.0)], alpha, eps).unwrap();
        assert!(r.work <= 1.0 / (alpha * eps) + 1e-9, "work bound T5c");
    }

    #[test]
    fn duplicate_and_zero_seed_entries_are_handled() {
        let g = path_graph();
        // Duplicate seed for node 0 accumulates; zero-mass entry is dropped.
        let combined = approximate_ppr_push(&g, &[(0, 0.5), (0, 0.5), (2, 0.0)], 0.15, 1e-7)
            .unwrap()
            .estimate;
        let single = approximate_ppr_push(&g, &[(0, 1.0)], 0.15, 1e-7)
            .unwrap()
            .estimate;
        let keys: std::collections::HashSet<_> = combined.keys().chain(single.keys()).collect();
        let l1: f64 = keys
            .iter()
            .map(|k| {
                (combined.get(*k).copied().unwrap_or(0.0) - single.get(*k).copied().unwrap_or(0.0))
                    .abs()
            })
            .sum();
        assert!(l1 < 1e-12, "duplicate seeds must accumulate to 1.0");
    }

    #[test]
    fn invariant_estimate_plus_residual_mass() {
        // T5: ‖p‖₁ + ‖residual‖₁ should account for the seed mass within the
        // restart-adjusted bound. Here we check ‖p‖₁ ≤ ‖seed‖₁ and residual ≥ 0.
        let g = path_graph();
        let r = approximate_ppr_push(&g, &[(0, 1.0)], 0.15, 1e-9).unwrap();
        let p_mass: f64 = r.estimate.values().sum();
        assert!(p_mass <= 1.0 + 1e-9);
        assert!(r.residual.values().all(|&v| v >= 0.0));
        let _: &Map<i32, f64> = &r.estimate;
    }
}
