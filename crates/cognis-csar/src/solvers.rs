//! Exact and power-iteration PPR solvers (dense, for small graphs / verification).
//!
//! Pure-Rust ports of `cognis_retrieval.csar.transition_matrix`,
//! `personalized_pagerank_exact` and `personalized_pagerank_power`. They build
//! the dense column-stochastic transition matrix `P = A·D⁻¹` and solve the PPR
//! equation either in closed form (`r = α·(I − (1−α)P)⁻¹·s`, via Gaussian
//! elimination with partial pivoting — no linear-algebra dependency, intended
//! for the small graphs the parity/agreement tests use) or by power iteration
//! (`r ← α·s + (1−α)P·r`, geometric convergence at rate `1 − α`, T2).
//!
//! These exist to cross-check the forward-push kernel: on any graph the three
//! solvers must agree within tolerance, and both exact and power conserve mass
//! (`‖r‖₁ = ‖s‖₁`, T3). They are not on the hot retrieval path.

use cognis_core::{CodeGraph, CognisError, Result};

/// Default L1 convergence threshold for power iteration (mirrors Python).
pub const DEFAULT_TOL: f64 = 1e-10;
/// Default hard iteration cap for power iteration (mirrors Python).
pub const DEFAULT_MAX_ITER: usize = 1000;

/// Dense column-stochastic transition matrix `P = A·D⁻¹`, stored row-major as a
/// length-`n*n` vector (`P[i*n + j]`). Column `j` is node `j`'s out-distribution
/// and sums to 1. `P[i*n + j] = A[i, j] / degree[j]`.
///
/// Mirrors the Python `transition_matrix`: edge `(u, v, w)` contributes
/// `w / degree[u]` to row `v`, column `u` (mass leaving `u` toward `v`).
pub fn transition_matrix(g: &CodeGraph) -> Vec<f64> {
    let n = g.n();
    let mut p = vec![0.0f64; n * n];
    for u in 0..n {
        let d_u = g.degree[u];
        if d_u <= 0.0 {
            continue;
        }
        let (idx, w) = g.neighbors(u);
        for (e, &v) in idx.iter().enumerate() {
            let v = v as usize;
            p[v * n + u] += w[e] / d_u;
        }
    }
    p
}

/// Solve the PPR equation exactly: `r = α·(I − (1−α)P)⁻¹·s`.
///
/// `matrix` is the dense column-stochastic `P` from [`transition_matrix`]
/// (length `n*n`, row-major); `seed` is the length-`n` seed vector `s`.
///
/// # Errors
/// Returns [`CognisError::Retrieval`] when `alpha ∉ (0, 1]`, on a
/// dimension mismatch, or if the operator `I − (1−α)P` is singular (it is
/// provably non-singular for `alpha ∈ (0, 1]`, so this guards numerics only).
pub fn personalized_pagerank_exact(matrix: &[f64], seed: &[f64], alpha: f64) -> Result<Vec<f64>> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(CognisError::Retrieval(format!(
            "alpha must be in (0, 1]; got {alpha}"
        )));
    }
    let n = seed.len();
    if matrix.len() != n * n {
        return Err(CognisError::Retrieval(format!(
            "matrix has {} entries; expected {} for seed of length {n}",
            matrix.len(),
            n * n
        )));
    }

    // Build the operator M = I - (1 - alpha) * P and rhs b = alpha * s, then
    // solve M r = b by Gaussian elimination with partial pivoting.
    let mut m = vec![0.0f64; n * n];
    for i in 0..n {
        for j in 0..n {
            let ident = if i == j { 1.0 } else { 0.0 };
            m[i * n + j] = ident - (1.0 - alpha) * matrix[i * n + j];
        }
    }
    let b: Vec<f64> = seed.iter().map(|&s| alpha * s).collect();
    gaussian_solve(m, b, n)
}

/// Solve `M x = b` (dense, row-major, size `n`) by Gaussian elimination with
/// partial pivoting. Consumes `m`/`b`.
fn gaussian_solve(mut m: Vec<f64>, mut b: Vec<f64>, n: usize) -> Result<Vec<f64>> {
    for col in 0..n {
        // Partial pivot: largest-magnitude entry in this column at/below the
        // diagonal.
        let mut pivot = col;
        let mut best = m[col * n + col].abs();
        for row in (col + 1)..n {
            let v = m[row * n + col].abs();
            if v > best {
                best = v;
                pivot = row;
            }
        }
        if best == 0.0 {
            return Err(CognisError::Retrieval(
                "singular operator in exact PPR solve".to_string(),
            ));
        }
        if pivot != col {
            for j in 0..n {
                m.swap(col * n + j, pivot * n + j);
            }
            b.swap(col, pivot);
        }
        // Eliminate below the pivot.
        let diag = m[col * n + col];
        for row in (col + 1)..n {
            let factor = m[row * n + col] / diag;
            if factor == 0.0 {
                continue;
            }
            for j in col..n {
                m[row * n + j] -= factor * m[col * n + j];
            }
            b[row] -= factor * b[col];
        }
    }
    // Back-substitution.
    let mut x = vec![0.0f64; n];
    for row in (0..n).rev() {
        let mut acc = b[row];
        for j in (row + 1)..n {
            acc -= m[row * n + j] * x[j];
        }
        x[row] = acc / m[row * n + row];
    }
    Ok(x)
}

/// Solve the PPR equation by power iteration `r ← α·s + (1−α)P·r`.
///
/// Returns `(r, iterations)`. Converges geometrically at rate `1 − α` (T2),
/// independent of `n`; stops when the successive-iterate L1 change `≤ tol` or
/// after `max_iter` steps.
///
/// # Errors
/// Returns [`CognisError::Retrieval`] when `alpha ∉ (0, 1]` or on a dimension
/// mismatch between `matrix` and `seed`.
pub fn personalized_pagerank_power(
    matrix: &[f64],
    seed: &[f64],
    alpha: f64,
    tol: f64,
    max_iter: usize,
) -> Result<(Vec<f64>, usize)> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(CognisError::Retrieval(format!(
            "alpha must be in (0, 1]; got {alpha}"
        )));
    }
    let n = seed.len();
    if matrix.len() != n * n {
        return Err(CognisError::Retrieval(format!(
            "matrix has {} entries; expected {} for seed of length {n}",
            matrix.len(),
            n * n
        )));
    }

    let mut r = seed.to_vec();
    let mut iterations = 0usize;
    for step in 1..=max_iter {
        iterations = step;
        // nxt = alpha * s + (1 - alpha) * (P @ r)
        let mut nxt = vec![0.0f64; n];
        for i in 0..n {
            let mut acc = 0.0f64;
            let base = i * n;
            for j in 0..n {
                acc += matrix[base + j] * r[j];
            }
            nxt[i] = alpha * seed[i] + (1.0 - alpha) * acc;
        }
        let delta: f64 = nxt.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        r = nxt;
        if delta <= tol {
            break;
        }
    }
    Ok((r, iterations))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::approximate_ppr_push;

    /// Triangle 0-1-2 plus tail 2-3 (undirected, unit weights).
    fn small_graph() -> CodeGraph {
        // edges: 0-1, 1-2, 2-0, 2-3
        CodeGraph {
            indptr: vec![0, 2, 4, 7, 8],
            indices: vec![1, 2, 0, 2, 0, 1, 3, 2],
            weights: vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            degree: vec![2.0, 2.0, 3.0, 1.0],
            node_ids: (0..4).map(|i| format!("n{i}")).collect(),
            index: (0..4).map(|i| (format!("n{i}"), i)).collect(),
        }
    }

    fn l1(a: &[f64], b: &[f64]) -> f64 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
    }

    #[test]
    fn transition_matrix_columns_are_stochastic() {
        let g = small_graph();
        let n = g.n();
        let p = transition_matrix(&g);
        for j in 0..n {
            let col_sum: f64 = (0..n).map(|i| p[i * n + j]).sum();
            assert!((col_sum - 1.0).abs() < 1e-12, "column {j} must sum to 1");
        }
    }

    #[test]
    fn exact_and_power_agree_and_conserve_mass() {
        let g = small_graph();
        let p = transition_matrix(&g);
        let mut s = vec![0.0; g.n()];
        s[0] = 1.0;
        let alpha = 0.15;

        let exact = personalized_pagerank_exact(&p, &s, alpha).unwrap();
        let (power, iters) = personalized_pagerank_power(&p, &s, alpha, 1e-12, 1000).unwrap();

        assert!(l1(&exact, &power) < 1e-8, "exact vs power disagree");
        assert!(
            iters < 1000,
            "power iteration should converge well before cap"
        );
        // Mass conservation (T3): ‖r‖₁ = ‖s‖₁ = 1.
        assert!((exact.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!((power.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn forward_push_approximates_exact() {
        let g = small_graph();
        let p = transition_matrix(&g);
        let mut s = vec![0.0; g.n()];
        s[0] = 1.0;
        let alpha = 0.15;

        let exact = personalized_pagerank_exact(&p, &s, alpha).unwrap();
        let push = approximate_ppr_push(&g, &[(0, 1.0)], alpha, 1e-9).unwrap();
        let mut approx = vec![0.0; g.n()];
        for (&node, &mass) in &push.estimate {
            approx[node as usize] = mass;
        }
        assert!(
            l1(&exact, &approx) < 1e-3,
            "forward push should track exact"
        );
    }

    #[test]
    fn rejects_invalid_alpha() {
        let g = small_graph();
        let p = transition_matrix(&g);
        let s = vec![1.0, 0.0, 0.0, 0.0];
        assert!(personalized_pagerank_exact(&p, &s, 0.0).is_err());
        assert!(personalized_pagerank_power(&p, &s, 1.5, 1e-9, 10).is_err());
    }
}
