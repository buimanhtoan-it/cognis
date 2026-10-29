//! Resident CSR code graph — the compressed adjacency CSAR and CSIA diffuse over.
//!
//! Mirrors the symmetrized, weighted graph the Python
//! `cognis_retrieval.csar.CodeGraph` / `build_code_graph` produce, and adds
//! directed forward and reverse-impact CSR projections (`ImpactGraph`) for
//! inter-procedural complexity propagation and Code Spreading-Impact Analysis.

use std::collections::{BTreeMap, HashMap};

use crate::models::{Edge, EdgeKind};

/// A symmetrized or directed weighted code graph in CSR form.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeGraph {
    /// Row pointers, length `n + 1`. Row `u` spans `indptr[u]..indptr[u + 1]`.
    pub indptr: Vec<i32>,
    /// Neighbour node indices, length `nnz`; sorted ascending within each row.
    pub indices: Vec<i32>,
    /// Edge weights aligned with `indices` (parallel edges summed).
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

    /// Construct a CSR [`CodeGraph`] from an ordered `node_ids` list and
    /// weighted directed `(u_idx, v_idx, weight)` triples.
    ///
    /// When `symmetrize` is `false`, edges are stored strictly in the given
    /// `(u -> v)` orientation; isolated nodes (`out_degree == 0`) still receive
    /// a unit self-loop `(u, 1.0)` so the transition matrix $P = A D^{-1}$
    /// remains column-stochastic and Forward-Push Theorems T1–T5 hold.
    pub fn from_weighted_triples(
        node_ids: Vec<String>,
        triples: &[(usize, usize, f64)],
        symmetrize: bool,
    ) -> Self {
        let n = node_ids.len();
        let mut index = HashMap::with_capacity(n);
        for (i, id) in node_ids.iter().enumerate() {
            index.insert(id.clone(), i);
        }

        let mut acc: Vec<BTreeMap<i32, f64>> = vec![BTreeMap::new(); n];
        for &(u, v, w) in triples {
            if u >= n || v >= n || u == v || w <= 0.0 {
                continue;
            }
            *acc[u].entry(v as i32).or_insert(0.0) += w;
            if symmetrize {
                *acc[v].entry(u as i32).or_insert(0.0) += w;
            }
        }

        let mut indptr = Vec::with_capacity(n + 1);
        indptr.push(0);
        let mut indices = Vec::new();
        let mut weights = Vec::new();
        let mut degree = Vec::with_capacity(n);

        for (u, neighbors) in acc.iter().enumerate() {
            if neighbors.is_empty() {
                indices.push(u as i32);
                weights.push(1.0);
                degree.push(1.0);
            } else {
                let mut d = 0.0f64;
                for (&v, &w) in neighbors {
                    indices.push(v);
                    weights.push(w);
                    d += w;
                }
                degree.push(d);
            }
            indptr.push(indices.len() as i32);
        }

        CodeGraph {
            indptr,
            indices,
            weights,
            degree,
            node_ids,
            index,
        }
    }
}

/// Multi-channel directed CSR bundle used by CSIA (Code Spreading-Impact
/// Analysis) and inter-procedural complexity propagation.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactGraph {
    /// Forward dependency graph (`caller -> callee`, `importer -> imported`).
    pub forward: CodeGraph,
    /// Reverse structural graph (`callee -> caller`, `target -> test`, `handler -> route`)
    /// weighted by structural conductance.
    pub reverse_structural: CodeGraph,
    /// Reverse complexity-amplification graph (`callee -> caller`) where each
    /// reverse call edge is boosted by $\beta^{\ell(caller \to callee)}$.
    pub reverse_complexity: CodeGraph,
    /// Symmetrized graph for CSAR context diffusion compatibility.
    pub symmetrized: CodeGraph,
}

impl ImpactGraph {
    /// Build all four CSR projections in a single pass over `node_ids` and `edges`.
    /// `edge_loop_depths` maps `(src_id, dst_id)` to the enclosing loop depth $\ell \in [0, 4]$.
    pub fn build(
        node_ids: Vec<String>,
        edges: &[Edge],
        edge_loop_depths: &HashMap<(String, String), u8>,
    ) -> Self {
        let n = node_ids.len();
        let mut index = HashMap::with_capacity(n);
        for (i, id) in node_ids.iter().enumerate() {
            index.insert(id.as_str(), i);
        }

        let mut fwd_triples = Vec::with_capacity(edges.len());
        let mut rev_struct_triples = Vec::with_capacity(edges.len());
        let mut rev_cplx_triples = Vec::with_capacity(edges.len());
        let mut sym_triples = Vec::with_capacity(edges.len());

        for edge in edges {
            if edge.dst_missing() || edge.confidence <= 0.0 {
                continue;
            }
            let (Some(&u), Some(&v)) = (
                index.get(edge.src_id.as_str()),
                index.get(edge.dst_id.as_str()),
            ) else {
                continue;
            };
            if u == v {
                continue;
            }
            let conf = edge.confidence;
            fwd_triples.push((u, v, conf));
            sym_triples.push((u, v, conf));

            let struct_gamma = match edge.kind {
                EdgeKind::Calls => 0.95,
                EdgeKind::Inherits => 0.90,
                EdgeKind::Implements => 0.85,
                EdgeKind::RoutesTo => 0.95,
                EdgeKind::Tests => 1.00,
                EdgeKind::Reads | EdgeKind::Writes => 0.80,
                EdgeKind::Imports => 0.45,
            };
            rev_struct_triples.push((v, u, conf * struct_gamma));

            let cplx_gamma = match edge.kind {
                EdgeKind::Calls | EdgeKind::RoutesTo => 1.0,
                EdgeKind::Tests => 0.8,
                EdgeKind::Inherits | EdgeKind::Implements => 0.5,
                EdgeKind::Reads | EdgeKind::Writes => 0.6,
                EdgeKind::Imports => 0.05,
            };
            let loop_depth = edge_loop_depths
                .get(&(edge.src_id.clone(), edge.dst_id.clone()))
                .copied()
                .or_else(|| {
                    edge.meta
                        .get("loop_depth")
                        .and_then(|val| val.as_u64())
                        .map(|d| d as u8)
                })
                .unwrap_or(0)
                .min(4);
            let loop_boost = 4.0f64.powi(i32::from(loop_depth));
            rev_cplx_triples.push((v, u, conf * cplx_gamma * loop_boost));
        }

        let forward = CodeGraph::from_weighted_triples(node_ids.clone(), &fwd_triples, false);
        let reverse_structural =
            CodeGraph::from_weighted_triples(node_ids.clone(), &rev_struct_triples, false);
        let reverse_complexity =
            CodeGraph::from_weighted_triples(node_ids.clone(), &rev_cplx_triples, false);
        let symmetrized = CodeGraph::from_weighted_triples(node_ids, &sym_triples, true);

        ImpactGraph {
            forward,
            reverse_structural,
            reverse_complexity,
            symmetrized,
        }
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

    #[test]
    fn impact_graph_reverses_edges_and_boosts_loop_depth() {
        let node_ids = vec!["caller".to_string(), "callee".to_string()];
        let edges = vec![Edge {
            src_id: "caller".into(),
            dst_id: "callee".into(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            meta: serde_json::Value::Null,
        }];
        let mut loop_depths = HashMap::new();
        loop_depths.insert(("caller".to_string(), "callee".to_string()), 2u8);

        let ig = ImpactGraph::build(node_ids, &edges, &loop_depths);
        // Forward: caller (0) -> callee (1)
        assert_eq!(ig.forward.neighbors(0).0, &[1]);
        // Reverse complexity: callee (1) -> caller (0) with 4^2 = 16.0 weight
        let (rev_nbrs, rev_w) = ig.reverse_complexity.neighbors(1);
        assert_eq!(rev_nbrs, &[0]);
        assert!((rev_w[0] - 16.0).abs() < 1e-9);
    }
}
