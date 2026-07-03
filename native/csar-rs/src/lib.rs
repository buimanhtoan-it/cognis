//! Native CSAR forward-push Personalized PageRank kernel (Rust, C ABI).
//!
//! This is the Rust port of `cognis_retrieval.csar.approximate_ppr_push` and the
//! single hot computational kernel of CSAR retrieval. It mirrors the reference
//! Python implementation operation-for-operation (same threshold test, same
//! LIFO worklist, same arithmetic order over CSR-sorted neighbours) so results
//! stay within the algorithm's L1 tolerance — see `docs/csar.md` (T5a/T5b/T5c).
//!
//! It exports the *same* C ABI as the earlier C++ slice
//! (`csar_forward_push`, `csar_abi_version`), so the Python ctypes bridge
//! (`cognis_retrieval/_native.py`) and the parity tests load it unchanged: only
//! the producer of `csar_native.dll` changes (Rust instead of C++).
//!
//! Graph is passed in CSR form:
//!   indptr[n+1], indices[nnz], weights[nnz], degree[n]
//! with neighbour lists sorted by node index (as `build_code_graph` produces).

use std::os::raw::c_int;
use std::slice;

const ABI_VERSION: c_int = 1;

/// Forward-push PPR. See the module docs and `csar_native.cpp` for full
/// semantics. Returns the count of nonzero estimate entries written, or `-1`
/// on invalid arguments.
///
/// # Safety
/// All pointers must be valid for the lengths implied by `n`, `nnz` (= the last
/// `indptr` entry), and `ns`. `out_nodes` / `out_vals` must each have capacity
/// for at least `n` elements. This matches what the in-repo loader provides.
#[no_mangle]
pub unsafe extern "C" fn csar_forward_push(
    n: c_int,
    indptr: *const i32,
    indices: *const i32,
    weights: *const f64,
    degree: *const f64,
    seed_nodes: *const i32,
    seed_vals: *const f64,
    ns: c_int,
    alpha: f64,
    eps: f64,
    out_nodes: *mut i32,
    out_vals: *mut f64,
    out_work: *mut f64,
    out_pushes: *mut i64,
) -> c_int {
    if n <= 0 {
        return -1;
    }
    if !(alpha > 0.0 && alpha <= 1.0) {
        return -1;
    }
    if !(eps > 0.0) {
        return -1;
    }

    let n_us = n as usize;
    let indptr = slice::from_raw_parts(indptr, n_us + 1);
    let nnz = indptr[n_us] as usize;
    let indices = slice::from_raw_parts(indices, nnz);
    let weights = slice::from_raw_parts(weights, nnz);
    let degree = slice::from_raw_parts(degree, n_us);

    let mut estimate = vec![0.0f64; n_us];
    let mut residual = vec![0.0f64; n_us];
    let mut in_active = vec![false; n_us];

    // Seed the residual (skip exact-zero masses, mirroring the Python filter).
    let ns_us = if ns > 0 { ns as usize } else { 0 };
    let seeds: &[i32] = if ns_us > 0 && !seed_nodes.is_null() {
        slice::from_raw_parts(seed_nodes, ns_us)
    } else {
        &[]
    };
    let svals: &[f64] = if ns_us > 0 && !seed_vals.is_null() {
        slice::from_raw_parts(seed_vals, ns_us)
    } else {
        &[]
    };
    for i in 0..seeds.len() {
        let u = seeds[i];
        if u < 0 || (u as usize) >= n_us {
            continue;
        }
        let m = svals[i];
        if m != 0.0 {
            residual[u as usize] += m;
        }
    }

    // Initial worklist: residual[u] >= eps * degree[u].
    let mut active: Vec<i32> = Vec::with_capacity(seeds.len() + 16);
    for &u in seeds.iter() {
        if u < 0 || (u as usize) >= n_us {
            continue;
        }
        let su = u as usize;
        if !in_active[su] && residual[su] >= eps * degree[su] {
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
        if r_u < eps * degree[su] {
            continue;
        }

        estimate[su] += alpha * r_u;
        residual[su] = 0.0;
        let push_mass = (1.0 - alpha) * r_u;
        let d_u = degree[su];

        let start = indptr[su] as usize;
        let end = indptr[su + 1] as usize;
        for e in start..end {
            let v = indices[e];
            if v < 0 || (v as usize) >= n_us {
                continue;
            }
            let sv = v as usize;
            residual[sv] += push_mass * weights[e] / d_u;
            if !in_active[sv] && residual[sv] >= eps * degree[sv] {
                active.push(v);
                in_active[sv] = true;
            }
        }

        work += d_u;
        pushes += 1;
    }

    let out_nodes = slice::from_raw_parts_mut(out_nodes, n_us);
    let out_vals = slice::from_raw_parts_mut(out_vals, n_us);
    let mut count: usize = 0;
    for u in 0..n_us {
        let e = estimate[u];
        if e != 0.0 {
            out_nodes[count] = u as i32;
            out_vals[count] = e;
            count += 1;
        }
    }
    if !out_work.is_null() {
        *out_work = work;
    }
    if !out_pushes.is_null() {
        *out_pushes = pushes;
    }
    count as c_int
}

/// ABI probe so the loader can verify the contract it linked against.
#[no_mangle]
pub extern "C" fn csar_abi_version() -> c_int {
    ABI_VERSION
}
