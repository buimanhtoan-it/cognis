//! CSAR solver parity test (rust-engine-migration, task 4.2 / Req 4.3, 4.4).
//!
//! Asserts the pure-Rust CSAR crate matches the Python oracle
//! (`cognis_retrieval.csar`) on the same graphs — Property 2:
//!
//! * `approximate_ppr_push` — estimate `L1 < 1e-9` vs Python (the carried-over
//!   proven kernel; the same bit-exact accumulation order), plus identical work
//!   bound shape.
//! * `personalized_pagerank_exact` / `_power` — score vectors agree with the
//!   Python solvers within a tight tolerance, and the forward-push estimate
//!   tracks both (solver agreement).
//! * `diffuse_seed_hits` — identical ranked symbol ids, scores within tolerance,
//!   and the same `on_path`/`seed` evidence flags (Req 4.4 contract shape).
//!
//! The oracle is captured in `tests/fixtures/solver_parity_golden.json`, so
//! this runs under plain `cargo test` with no Python runtime (mirroring
//! `cognis-store`'s parity tests). The golden is checked in as frozen oracle
//! output; there is no Python toolchain to regenerate it.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use cognis_csar::{
    approximate_ppr_push, diffuse_seed_hits, personalized_pagerank_exact,
    personalized_pagerank_power, transition_matrix, CodeGraph, Hit,
};
use serde_json::Value;

/// Req 4.3: CSAR estimate parity `L1 < 1e-9`. The Rust kernel reproduces the
/// Python push order so this is comfortably met.
const ESTIMATE_L1_TOL: f64 = 1e-9;
/// Exact/power solvers use different arithmetic (dense linear solve / iteration)
/// than Python's numpy path, so a looser-but-still-tight numeric tolerance.
const SOLVER_L1_TOL: f64 = 1e-6;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_golden() -> Value {
    let path = fixtures_dir().join("solver_parity_golden.json");
    assert!(
        path.exists(),
        "missing golden {path:?}; it is a checked-in frozen oracle fixture"
    );
    let text = fs::read_to_string(&path).expect("read golden");
    serde_json::from_str(&text).expect("parse golden json")
}

fn i32_vec(v: &Value) -> Vec<i32> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_i64().expect("int") as i32)
        .collect()
}

fn f64_vec(v: &Value) -> Vec<f64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_f64().expect("float"))
        .collect()
}

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_str().expect("string").to_string())
        .collect()
}

fn build_graph(case: &Value) -> CodeGraph {
    let node_ids = str_vec(&case["node_ids"]);
    let index = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();
    CodeGraph {
        indptr: i32_vec(&case["indptr"]),
        indices: i32_vec(&case["indices"]),
        weights: f64_vec(&case["weights"]),
        degree: f64_vec(&case["degree"]),
        node_ids,
        index,
    }
}

fn l1(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "length mismatch for L1");
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

/// Dense vector from a sparse `{node -> mass}` golden estimate map.
fn dense_from_map(map: &Value, n: usize) -> Vec<f64> {
    let mut v = vec![0.0; n];
    for (k, val) in map.as_object().expect("object") {
        let node: usize = k.parse().expect("node index key");
        v[node] = val.as_f64().expect("float");
    }
    v
}

#[test]
fn forward_push_matches_python_oracle() {
    let golden = load_golden();
    let cases = golden["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());

    for case in cases {
        let label = case["label"].as_str().unwrap_or("?");
        let g = build_graph(case);
        let alpha = case["alpha"].as_f64().unwrap();
        let eps = case["eps"].as_f64().unwrap();

        let seed: Vec<(i32, f64)> = case["push_seed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                let p = pair.as_array().unwrap();
                (p[0].as_i64().unwrap() as i32, p[1].as_f64().unwrap())
            })
            .collect();

        let push = approximate_ppr_push(&g, &seed, alpha, eps).expect("push");

        // Estimate parity: L1 < 1e-9 (Req 4.3).
        let n = g.n();
        let mut rust_est = vec![0.0; n];
        for (&node, &mass) in &push.estimate {
            rust_est[node as usize] = mass;
        }
        let py_est = dense_from_map(&case["push_estimate"], n);
        let diff = l1(&rust_est, &py_est);
        assert!(
            diff < ESTIMATE_L1_TOL,
            "[{label}] push estimate L1 {diff} exceeds {ESTIMATE_L1_TOL}"
        );

        // Work + pushes identical (same push order).
        assert_eq!(
            push.pushes,
            case["push_pushes"].as_i64().unwrap(),
            "[{label}] push count diverges"
        );
        assert!(
            (push.work - case["push_work"].as_f64().unwrap()).abs() < 1e-9,
            "[{label}] work diverges"
        );
        // Work bound T5c.
        assert!(
            push.work <= 1.0 / (alpha * eps) + 1e-9,
            "[{label}] work bound"
        );
    }
}

#[test]
fn exact_and_power_match_python_and_each_other() {
    let golden = load_golden();
    for case in golden["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap_or("?");
        let g = build_graph(case);
        let n = g.n();
        let alpha = case["alpha"].as_f64().unwrap();
        let tol = case["tol"].as_f64().unwrap();
        let max_iter = case["max_iter"].as_u64().unwrap() as usize;

        let seed: Vec<(i32, f64)> = case["push_seed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                let p = pair.as_array().unwrap();
                (p[0].as_i64().unwrap() as i32, p[1].as_f64().unwrap())
            })
            .collect();
        let mut s = vec![0.0; n];
        for &(node, mass) in &seed {
            s[node as usize] = mass;
        }

        let p = transition_matrix(&g);
        let exact = personalized_pagerank_exact(&p, &s, alpha).expect("exact");
        let (power, _iters) =
            personalized_pagerank_power(&p, &s, alpha, tol, max_iter).expect("power");

        let py_exact = f64_vec(&case["exact"]);
        let py_power = f64_vec(&case["power"]);

        assert!(
            l1(&exact, &py_exact) < SOLVER_L1_TOL,
            "[{label}] exact vs Python L1 {} exceeds tol",
            l1(&exact, &py_exact)
        );
        assert!(
            l1(&power, &py_power) < SOLVER_L1_TOL,
            "[{label}] power vs Python L1 {} exceeds tol",
            l1(&power, &py_power)
        );
        // Solver agreement: exact ≈ power, and both conserve mass (T3).
        assert!(
            l1(&exact, &power) < SOLVER_L1_TOL,
            "[{label}] exact vs power"
        );
        let seed_mass: f64 = s.iter().sum();
        assert!(
            (exact.iter().sum::<f64>() - seed_mass).abs() < 1e-6,
            "[{label}] exact mass not conserved"
        );

        // Forward-push tracks the exact solution (looser bound — it is an
        // eps-approximation).
        let eps = case["eps"].as_f64().unwrap();
        let push = approximate_ppr_push(&g, &seed, alpha, eps).expect("push");
        let mut approx = vec![0.0; n];
        for (&node, &mass) in &push.estimate {
            approx[node as usize] = mass;
        }
        assert!(
            l1(&exact, &approx) < 1e-3,
            "[{label}] forward-push does not track exact"
        );
    }
}

#[test]
fn diffuse_seed_hits_matches_python_oracle() {
    let golden = load_golden();
    for case in golden["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap_or("?");
        let g = build_graph(case);
        let alpha = case["alpha"].as_f64().unwrap();
        let eps = case["eps"].as_f64().unwrap();
        let k = case["k"].as_u64().unwrap() as usize;

        // Rebuild the per-layer seed hits exactly as the golden recorded them.
        let hits_per_layer: Vec<Vec<Hit>> = case["diffuse_hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| {
                layer
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pair| {
                        let p = pair.as_array().unwrap();
                        Hit::new(
                            p[0].as_str().unwrap(),
                            p[1].as_f64().unwrap(),
                            "lexical",
                            "seed",
                        )
                    })
                    .collect()
            })
            .collect();

        let rust_hits = diffuse_seed_hits(&g, &hits_per_layer, k, alpha, eps).expect("diffuse");
        let py_hits = case["diffuse_out"].as_array().unwrap();

        assert_eq!(
            rust_hits.len(),
            py_hits.len(),
            "[{label}] diffuse hit count diverges"
        );

        // Map symbol_id -> (score, on_path, seed) for the Python side.
        let py_by_id: HashMap<String, (f64, bool, bool)> = py_hits
            .iter()
            .map(|h| {
                (
                    h["symbol_id"].as_str().unwrap().to_string(),
                    (
                        h["score"].as_f64().unwrap(),
                        h["on_path"].as_bool().unwrap(),
                        h["seed"].as_bool().unwrap(),
                    ),
                )
            })
            .collect();

        // Ranked symbol-id order is identical (Property 2 ordering).
        let rust_ids: Vec<&str> = rust_hits.iter().map(|h| h.symbol_id.as_str()).collect();
        let py_ids: Vec<&str> = py_hits
            .iter()
            .map(|h| h["symbol_id"].as_str().unwrap())
            .collect();
        assert_eq!(rust_ids, py_ids, "[{label}] diffuse ranking order diverges");

        for h in &rust_hits {
            let (py_score, py_on_path, py_seed) = py_by_id
                .get(&h.symbol_id)
                .copied()
                .unwrap_or_else(|| panic!("[{label}] symbol {} missing in oracle", h.symbol_id));
            assert!(
                (h.score - py_score).abs() < ESTIMATE_L1_TOL,
                "[{label}] {} score {} vs Python {py_score}",
                h.symbol_id,
                h.score
            );
            assert_eq!(h.layer, "csar");
            assert_eq!(
                h.evidence["on_path"].as_bool().unwrap(),
                py_on_path,
                "[{label}] {} on_path flag diverges",
                h.symbol_id
            );
            assert_eq!(
                h.evidence["seed"].as_bool().unwrap(),
                py_seed,
                "[{label}] {} seed flag diverges",
                h.symbol_id
            );
            // Contract shape (Req 4.4): on_path:bool ∧ ppr_score:f64 present.
            assert!(h.evidence["ppr_score"].as_f64().is_some());
        }
    }
}
