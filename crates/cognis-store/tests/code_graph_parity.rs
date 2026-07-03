//! CSR code-graph parity test (rust-engine-migration, task 3.5 / Req 4.3).
//!
//! Asserts `SymbolStore::build_code_graph` produces a resident CSR graph
//! **matching the Python `cognis_retrieval.csar.build_code_graph` oracle** on
//! the same DB — Property 2: same node set/ordering, identical
//! `indptr`/`indices`/`degree`, `weights` within a tight L1 tolerance, symmetry,
//! and self-loops on isolated nodes.
//!
//! The oracle is captured in `tests/fixtures/code_graph_parity_golden.json` from
//! the Python builder run against the checked-in fixture `python_uckg.db`,
//! flattening its adjacency list into the same CSR layout. Capturing the
//! golden lets this run under plain `cargo test` with no Python runtime,
//! mirroring `tests/fts_parity.rs` / `tests/vec_parity.rs`. The golden and the
//! fixture DB are checked in as frozen oracle output; there is no toolchain in
//! this repo to regenerate them.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use cognis_store::{CodeGraph, Database, SymbolStore};
use serde_json::Value;

/// Tight L1 tolerance on the symmetrized weight / degree vectors (Req 4.3:
/// `L1 < 1e-9`). The arithmetic is the same accumulation order on both sides,
/// so this is comfortably met; the tolerance guards float-formatting drift.
const WEIGHT_L1_TOL: f64 = 1e-9;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the fixture DB into a temp dir so the test never mutates the checked-in
/// file (WAL mode would otherwise create sidecar files next to it).
fn open_temp_copy(tmp: &tempfile::TempDir, db_name: &str) -> Database {
    let src = fixtures_dir().join(db_name);
    assert!(
        src.exists(),
        "missing fixture {src:?}; it is a checked-in frozen oracle fixture"
    );
    let dst = tmp.path().join(db_name);
    fs::copy(&src, &dst).expect("copy fixture");
    Database::open(&dst).expect("open Python-built DB")
}

fn load_golden() -> Value {
    let path = fixtures_dir().join("code_graph_parity_golden.json");
    assert!(
        path.exists(),
        "missing golden {path:?}; it is a checked-in frozen oracle fixture"
    );
    let text = fs::read_to_string(&path).expect("read golden");
    serde_json::from_str(&text).expect("parse golden json")
}

fn json_str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_str().expect("string").to_string())
        .collect()
}

fn json_i32_vec(v: &Value) -> Vec<i32> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_i64().expect("int") as i32)
        .collect()
}

fn json_f64_vec(v: &Value) -> Vec<f64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_f64().expect("float"))
        .collect()
}

/// L1 distance between two equal-length f64 vectors.
fn l1(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "length mismatch for L1");
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// Assert the graph is undirected: every `(u -> v, w)` has a matching
/// `(v -> u, w)` with the same weight. Self-loops are their own mirror.
fn assert_symmetric(g: &CodeGraph) {
    let mut seen: BTreeMap<(i32, i32), f64> = BTreeMap::new();
    for u in 0..g.n() {
        let (idx, w) = g.neighbors(u);
        for (j, &v) in idx.iter().enumerate() {
            seen.insert((u as i32, v), w[j]);
        }
    }
    for (&(u, v), &w) in &seen {
        let back = seen.get(&(v, u)).copied();
        assert!(
            back.is_some_and(|bw| (bw - w).abs() < WEIGHT_L1_TOL),
            "edge ({u},{v},{w}) has no symmetric counterpart"
        );
    }
}

#[test]
fn build_code_graph_matches_python_oracle() {
    let golden = load_golden();
    let db_name = golden["db"].as_str().expect("golden.db");
    let cases = golden["cases"].as_array().expect("golden.cases");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, db_name);

    for case in cases {
        // `edge_kinds`: null → None; else a kind whitelist.
        let kinds: Option<Vec<String>> = match &case["edge_kinds"] {
            Value::Null => None,
            other => Some(json_str_vec(other)),
        };
        let kinds_ref = kinds.as_deref();
        let label = format!("{:?}", case["edge_kinds"]);

        let g = db.build_code_graph(kinds_ref).expect("build_code_graph");

        // Node set + ordering identical (node index ↔ symbol id parity).
        let exp_nodes = json_str_vec(&case["node_ids"]);
        assert_eq!(
            g.node_ids, exp_nodes,
            "node_ids diverge (edge_kinds={label})"
        );
        // The inverse index is consistent with node_ids.
        for (i, id) in g.node_ids.iter().enumerate() {
            assert_eq!(g.index.get(id), Some(&i), "index inverse for {id}");
        }

        // CSR structure identical: indptr + indices (sorted per row, same nnz).
        let exp_indptr = json_i32_vec(&case["indptr"]);
        let exp_indices = json_i32_vec(&case["indices"]);
        assert_eq!(g.indptr, exp_indptr, "indptr diverge (edge_kinds={label})");
        assert_eq!(
            g.indices, exp_indices,
            "indices diverge (edge_kinds={label})"
        );

        // Weights + degree within a tight L1 tolerance (Req 4.3).
        let exp_weights = json_f64_vec(&case["weights"]);
        let exp_degree = json_f64_vec(&case["degree"]);
        assert!(
            l1(&g.weights, &exp_weights) < WEIGHT_L1_TOL,
            "weights L1 {} exceeds tol (edge_kinds={label})",
            l1(&g.weights, &exp_weights)
        );
        assert!(
            l1(&g.degree, &exp_degree) < WEIGHT_L1_TOL,
            "degree L1 {} exceeds tol (edge_kinds={label})",
            l1(&g.degree, &exp_degree)
        );

        // Structural invariants the oracle guarantees: CSR shape, symmetry,
        // and a self-loop (degree 1.0) for every isolated node.
        assert_eq!(g.indptr.len(), g.n() + 1);
        assert_eq!(g.indices.len(), g.nnz());
        assert_eq!(g.weights.len(), g.nnz());
        assert_eq!(g.degree.len(), g.n());
        assert_symmetric(&g);
        for u in 0..g.n() {
            let (idx, w) = g.neighbors(u);
            if idx == [u as i32] {
                assert!(
                    (w[0] - 1.0).abs() < WEIGHT_L1_TOL && (g.degree[u] - 1.0).abs() < WEIGHT_L1_TOL,
                    "isolated node {u} should carry a unit self-loop"
                );
            }
        }
    }

    db.close_thread_connection();
}
