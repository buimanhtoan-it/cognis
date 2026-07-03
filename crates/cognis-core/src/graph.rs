//! Resident CSR code graph — the compressed adjacency CSAR diffuses over.
//!
//! Mirrors the symmetrized, weighted graph the Python
//! `cognis_retrieval.csar.CodeGraph` / `build_code_graph` produce, but stored in
//! **Compressed Sparse Row (CSR)** form — the exact layout the proven
//! forward-push kernel consumes (`native/csar-rs`: `indptr[n+1]`,
//! `indices[nnz]`, `weights[nnz]`, `degree[n]`, with each row's neighbours
//! sorted ascending so the LIFO push order matches the oracle).
//!
//! It lives in `cognis-core` — the dependency-neutral foundation — rather than
//! `cognis-csar` for the same reason [`crate::Hit`] does: `cognis-store`'s
//! `SymbolStore::build_code_graph` *produces* a `CodeGraph` (Task 3.5) and
//! `cognis-csar`'s solvers (Task 4.2) *consume* one. Defining it here lets
//! `store` and `csar` share a single type with no dependency cycle; `cognis-csar`
//! re-exports it so the design's stated home still surfaces it.
//!
//! The kernel only ever touches the `i32`/`f64` arrays; `node_ids` / `index`
//! exist purely at the boundary to map between symbol ids and node indices.

use std::collections::HashMap;

/// A symmetrized, weighted code graph in CSR form.
///
/// Built once per index epoch and held **resident** between queries (the
/// condition for the end-to-end solver win — design Data Models → CSR graph).
/// Invariants (all established by [`build_code_graph`]-style construction):
///
/// * `indptr.len() == n + 1`, `indptr[0] == 0`, `indptr[n] == nnz`, ascending.
/// * `indices.len() == weights.len() == nnz`; each row
///   `indices[indptr[u]..indptr[u+1]]` is **sorted ascending** and free of
///   duplicates (parallel edges are coalesced by summing their weights).
/// * The graph is **undirected**: if `v` is a neighbour of `u` with weight `w`,
///   then `u` is a neighbour of `v` with the same `w`.
/// * `degree.len() == n`; `degree[u]` is the weighted column sum of row `u`.
/// * An **isolated** node carries a single self-loop `(u, 1.0)` with
///   `degree[u] == 1.0`, keeping the transition matrix column-stochastic.
/// * `node_ids.len() == n`; `node_ids[i]` is node `i`'s symbol id and
///   `index[&node_ids[i]] == i`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeGraph {
    /// Row pointers, length `n + 1`. Row `u` spans `indptr[u]..indptr[u + 1]`.
    pub indptr: Vec<i32>,
    /// Neighbour node indices, length `nnz`; sorted ascending within each row.
    pub indices: Vec<i32>,
    /// Edge weights aligned with `indices` (symmetrized, parallel edges summed).
    pub weights: Vec<f64>,
    /// Weighted degree (column sum) per node, length `n`.
    pub degree: Vec<f64>,
    /// Node index → symbol id (`<lang>:<path>:<qname>@<hash>`), length `n`.
    pub node_ids: Vec<String>,
    /// Inverse map: symbol id → node index.
    pub index: HashMap<String, usize>,
}

impl CodeGraph {
    /// Number of nodes.
    pub fn n(&self) -> usize {
        self.node_ids.len()
    }

    /// Number of stored edges (CSR non-zeros), counting self-loops.
    pub fn nnz(&self) -> usize {
        self.indices.len()
    }

    /// True when the graph has no nodes.
    pub fn is_empty(&self) -> bool {
        self.node_ids.is_empty()
    }

    /// The `(indices, weights)` slices for node `u`'s neighbours, or empty
    /// slices when `u` is out of range. Both slices have equal length and the
    /// indices are sorted ascending.
    pub fn neighbors(&self, u: usize) -> (&[i32], &[f64]) {
        if u >= self.n() {
            return (&[], &[]);
        }
        let start = self.indptr[u] as usize;
        let end = self.indptr[u + 1] as usize;
        (&self.indices[start..end], &self.weights[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CodeGraph {
        // 0 <-> 1 (w 0.9), 1 <-> 2 (w 0.8), node 3 isolated (self-loop).
        CodeGraph {
            indptr: vec![0, 1, 3, 4, 5],
            indices: vec![1, 0, 2, 1, 3],
            weights: vec![0.9, 0.9, 0.8, 0.8, 1.0],
            degree: vec![0.9, 1.7, 0.8, 1.0],
            node_ids: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            index: [("a", 0), ("b", 1), ("c", 2), ("d", 3)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn dimensions_and_accessors() {
        let g = sample();
        assert_eq!(g.n(), 4);
        assert_eq!(g.nnz(), 5);
        assert!(!g.is_empty());
        assert_eq!(g.indptr.len(), g.n() + 1);
        assert_eq!(g.indices.len(), g.nnz());
        assert_eq!(g.weights.len(), g.nnz());
        assert_eq!(g.degree.len(), g.n());
    }

    #[test]
    fn neighbors_returns_sorted_row_slices() {
        let g = sample();
        let (idx, w) = g.neighbors(1);
        assert_eq!(idx, &[0, 2]);
        assert_eq!(w, &[0.9, 0.8]);
        // Isolated node 3 has a single self-loop.
        assert_eq!(g.neighbors(3), (&[3i32][..], &[1.0f64][..]));
        // Out-of-range yields empty slices, never panics.
        assert_eq!(g.neighbors(99), (&[][..], &[][..]));
    }
}
