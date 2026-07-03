//! Differential parity harness test (rust-engine-migration, task 9.1 /
//! Requirements 10.3, 4.1, 4.2). Drives `cognis_eval::parity` to assert design
//! **Property 2** — kernel parity vs the Python oracle — across the lexical
//! (FTS5), semantic (vec KNN), RRF and CSAR surfaces.
//!
//! ## Three modes, increasing strength — never fabricate
//!
//! Mirroring the offline discipline of `onnx_parity` / `index_parity`, this test
//! runs whatever is genuinely available and *skips* (printing why, returning
//! `Ok`) the rest rather than inventing oracle results:
//!
//! 1. **Rust-vs-Rust determinism** (always, fully offline): the harness runs the
//!    same queries against two copies of the checked-in Python-built fixture DB
//!    and asserts every surface is byte-identical and the CSAR estimate L1 is
//!    *exactly* 0. This proves the engine is deterministic and the harness
//!    mechanics are sound.
//! 2. **Rust-vs-Python-oracle** (offline, when the captured goldens are
//!    present): the Rust engine runs on the Python-built DB and its lexical /
//!    semantic results are compared against the Python engine's *recorded*
//!    outputs (the `cognis-store` golden JSON). This is the real Property-2 gate
//!    that needs no Python runtime at test time.
//! 3. **Python-build vs Rust-build** (opt-in): two real DBs of the same repo —
//!    one from each indexer — supplied via `COGNIS_PARITY_PY_DB` and
//!    `COGNIS_PARITY_RS_DB`. The strongest differential; skipped when either DB
//!    is absent.
//!
//! The Python-built fixture DB and the goldens live in `cognis-store`'s test
//! fixtures, checked in as frozen oracle output (there is no Python toolchain to
//! regenerate them). They are reached by a workspace-relative path, the same way
//! `index_parity` reaches the baseline JSON; when they are absent the affected
//! mode skips.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cognis_core::Hit;
use cognis_eval::parity::{
    lexical_hit_sets, semantic_topk, DifferentialHarness, QueryCase, SurfaceParity,
};
use cognis_store::{Database, SymbolStore};
use serde_json::Value;

const DIM: usize = 384;

// Fixture symbol ids (from the checked-in Python-built fixture DB).
const ALPHA: &str = "python:src/mod.py:mod.alpha_beta@hash0001";
const GAMMA: &str = "python:src/mod.py:mod.gamma_handler@hash0002";
const DELTA: &str = "python:src/mod.py:mod.delta_request@hash0003";

/// Workspace root = two levels up from this crate's manifest dir
/// (`crates/cognis-eval` → repo root).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory holding `cognis-store`'s Python-built fixtures + goldens.
fn store_fixtures_dir() -> PathBuf {
    workspace_root()
        .join("crates")
        .join("cognis-store")
        .join("tests")
        .join("fixtures")
}

/// The checked-in Python-built UCKG, if present.
fn python_fixture_db() -> Option<PathBuf> {
    let p = store_fixtures_dir().join("python_uckg.db");
    p.is_file().then_some(p)
}

/// Copy a DB into `tmp` and open it (WAL would otherwise litter sidecars next to
/// the checked-in file).
fn open_temp_copy(tmp: &tempfile::TempDir, src: &Path, name: &str) -> Database {
    let dst = tmp.path().join(name);
    fs::copy(src, &dst).expect("copy fixture db");
    Database::open(&dst).expect("open uckg")
}

/// A 384-d query embedding matching fixture symbol `idx`'s stored vector
/// (`vec[j] = idx + 0.001 * j`), so semantic search has a well-defined nearest
/// neighbour.
fn symbol_query_vec(idx: usize) -> Vec<f32> {
    (0..DIM).map(|j| idx as f32 + 0.001 * j as f32).collect()
}

/// Cases exercising all four surfaces on the fixture DB.
fn fixture_cases() -> Vec<QueryCase> {
    vec![
        QueryCase {
            name: "tokens+alpha-vec+delta-seed".to_string(),
            lexical: Some("tokens".to_string()),
            semantic: Some(symbol_query_vec(0)),
            seeds: vec![(DELTA.to_string(), 1.0)],
            k: 10,
        },
        QueryCase {
            name: "handler+gamma-vec+alpha-seed".to_string(),
            lexical: Some("handler".to_string()),
            semantic: Some(symbol_query_vec(1)),
            seeds: vec![(ALPHA.to_string(), 1.0), (GAMMA.to_string(), 0.5)],
            k: 5,
        },
        QueryCase {
            name: "gamma-OR-delta lexical-only".to_string(),
            lexical: Some("gamma OR delta".to_string()),
            k: 10,
            ..Default::default()
        },
    ]
}

// ---------------------------------------------------------------------------
// Mode 1 — Rust-vs-Rust determinism (always runs, fully offline).
// ---------------------------------------------------------------------------

#[test]
fn rust_vs_rust_determinism_on_shared_db() {
    let Some(src) = python_fixture_db() else {
        eprintln!(
            "SKIP differential parity (determinism): fixture {:?} not found \
             (checked-in frozen oracle fixture).",
            store_fixtures_dir().join("python_uckg.db")
        );
        return;
    };

    // Two independent copies + handles → genuinely separate engine runs.
    let tmp = tempfile::tempdir().unwrap();
    let a = open_temp_copy(&tmp, &src, "a.db");
    let b = open_temp_copy(&tmp, &src, "b.db");

    let harness = DifferentialHarness::new(&a, &b);
    let cases = fixture_cases();
    let reports = harness.run_cases(&cases).expect("run cases");

    // Every surface must match, and the CSAR L1 must be *exactly* 0 (identical
    // inputs ⇒ bit-identical diffusion). Surface the first divergence verbatim.
    let mut mismatches = Vec::new();
    let mut csar_checked = 0usize;
    let mut surfaces_checked = 0usize;
    for r in &reports {
        mismatches.extend(r.mismatches());
        if r.lexical.is_some() {
            surfaces_checked += 1;
        }
        if r.semantic.is_some() {
            surfaces_checked += 1;
        }
        if r.rrf.is_some() {
            surfaces_checked += 1;
        }
        if let Some((l1, verdict)) = &r.csar {
            assert!(verdict.is_match(), "[{}] csar: {:?}", r.name, verdict);
            assert_eq!(
                *l1, 0.0,
                "[{}] determinism: CSAR L1 must be exactly 0, got {l1:.3e}",
                r.name
            );
            csar_checked += 1;
            surfaces_checked += 1;
        }
    }
    assert!(
        mismatches.is_empty(),
        "determinism divergences:\n{}",
        mismatches.join("\n")
    );
    assert!(csar_checked >= 1, "expected at least one CSAR comparison");
    assert!(
        surfaces_checked >= 6,
        "expected the cases to exercise lexical/semantic/rrf/csar surfaces"
    );

    a.close_thread_connection();
    b.close_thread_connection();
}

// ---------------------------------------------------------------------------
// Mode 2 — Rust engine vs the captured Python oracle (offline gate).
// ---------------------------------------------------------------------------

fn load_golden(name: &str) -> Option<Value> {
    let path = store_fixtures_dir().join(name);
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn hits_from_ids(ids: &[&str], layer: &str) -> Vec<Hit> {
    ids.iter()
        .map(|id| Hit::new(*id, 0.0, layer, "oracle"))
        .collect()
}

#[test]
fn rust_engine_reproduces_python_oracle_outputs() {
    let Some(src) = python_fixture_db() else {
        eprintln!("SKIP differential parity (oracle): fixture python_uckg.db not found.");
        return;
    };
    let fts_golden = load_golden("fts_parity_golden.json");
    let vec_golden = load_golden("vec_parity_golden.json");
    if fts_golden.is_none() && vec_golden.is_none() {
        eprintln!(
            "SKIP differential parity (oracle): no captured Python goldens in {:?} \
             (checked-in frozen oracle fixtures).",
            store_fixtures_dir()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let db = open_temp_copy(&tmp, &src, "oracle.db");

    let mut checked = 0usize;

    // Lexical (FTS5) hit sets must equal the recorded Python oracle sets.
    if let Some(golden) = fts_golden {
        let k = golden["k"].as_u64().expect("golden.k") as usize;
        for case in golden["cases"].as_array().expect("fts cases") {
            let query = case["query"].as_str().expect("query");
            let expected_ids: Vec<&str> = case["expected_ids"]
                .as_array()
                .expect("expected_ids")
                .iter()
                .map(|v| v.as_str().expect("id"))
                .collect();
            let actual = db.fts_search(query, k).expect("fts_search");
            let expected = hits_from_ids(&expected_ids, "lexical");
            let verdict = lexical_hit_sets(&actual, &expected);
            assert!(
                verdict.is_match(),
                "lexical oracle divergence for query {query:?}: {:?}",
                verdict.reason()
            );
            checked += 1;
        }
    }

    // Semantic (vec KNN) top-k ordering must equal the recorded Python ordering.
    if let Some(golden) = vec_golden {
        let k = golden["k"].as_u64().expect("golden.k") as usize;
        for case in golden["cases"].as_array().expect("vec cases") {
            let name = case["name"].as_str().unwrap_or("<unnamed>");
            let q: Vec<f32> = case["query"]
                .as_array()
                .expect("query")
                .iter()
                .map(|v| v.as_f64().expect("component") as f32)
                .collect();
            let expected_ids: Vec<&str> = case["expected"]
                .as_array()
                .expect("expected")
                .iter()
                .map(|e| e["symbol_id"].as_str().expect("symbol_id"))
                .collect();
            let actual = db.vec_search(&q, k).expect("vec_search");
            let expected = hits_from_ids(&expected_ids, "semantic");
            let verdict = semantic_topk(&actual, &expected);
            assert!(
                verdict.is_match(),
                "semantic oracle divergence for case {name:?}: {:?}",
                verdict.reason()
            );
            checked += 1;
        }
    }

    assert!(
        checked >= 1,
        "oracle goldens present but exercised no case — golden likely empty"
    );
    db.close_thread_connection();
}

// ---------------------------------------------------------------------------
// Mode 3 — Python-build vs Rust-build, two real DBs (opt-in via env).
// ---------------------------------------------------------------------------

fn env_db(var: &str) -> Option<PathBuf> {
    let raw = std::env::var(var).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let p = PathBuf::from(raw);
    p.is_file().then_some(p)
}

#[test]
fn python_build_vs_rust_build_differential() {
    let (Some(py), Some(rs)) = (env_db("COGNIS_PARITY_PY_DB"), env_db("COGNIS_PARITY_RS_DB"))
    else {
        eprintln!(
            "SKIP differential parity (two real DBs): set COGNIS_PARITY_PY_DB and \
             COGNIS_PARITY_RS_DB to a Python-built and a Rust-built UCKG of the same \
             repo to run the full Python-build-vs-Rust-build gate."
        );
        return;
    };

    // Copy both so the test never mutates the supplied files (WAL sidecars).
    let tmp = tempfile::tempdir().unwrap();
    let a = open_temp_copy(&tmp, &py, "py.db");
    let b = open_temp_copy(&tmp, &rs, "rs.db");
    let harness = DifferentialHarness::new(&a, &b);

    // Lexical: a handful of generic identifier-ish tokens. Without an embedder
    // we cannot synthesise query embeddings for arbitrary DBs, so the semantic /
    // RRF surfaces are out of scope for this mode (honest: no faked vectors).
    let lexical_queries = ["request", "client", "session", "auth", "json"];
    let mut mismatches = Vec::new();
    for q in lexical_queries {
        let verdict = harness.compare_lexical(q, 20).expect("compare_lexical");
        if let SurfaceParity::Mismatch(m) = verdict {
            mismatches.push(format!("lexical {q:?}: {m}"));
        }
    }

    // CSAR: seed from a couple of symbol ids actually present in the Python DB
    // (taken from its CSR node set so they resolve in both graphs).
    let py_graph = a.build_code_graph(None).expect("py build_code_graph");
    let seeds: Vec<(String, f64)> = py_graph
        .node_ids
        .iter()
        .take(2)
        .map(|id| (id.clone(), 1.0))
        .collect();
    if !seeds.is_empty() {
        let (l1, verdict) = harness.compare_csar(&seeds).expect("compare_csar");
        eprintln!("two-DB differential: CSAR estimate L1 = {l1:.3e}");
        if let SurfaceParity::Mismatch(m) = verdict {
            mismatches.push(format!("csar: {m}"));
        }
    }

    // Sanity: the DBs actually had content to compare.
    let py_symbols: BTreeSet<String> = py_graph.node_ids.iter().cloned().collect();
    assert!(
        !py_symbols.is_empty(),
        "Python DB has no symbols — differential would be vacuous"
    );

    assert!(
        mismatches.is_empty(),
        "Python-build vs Rust-build divergences:\n{}",
        mismatches.join("\n")
    );

    a.close_thread_connection();
    b.close_thread_connection();
}
