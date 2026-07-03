//! Embedding parity test (rust-engine-migration, task 6.2 / Requirement 7.2).
//!
//! Asserts the `onnx-local` backend (bge-small via `ort`) reproduces the
//! `sentence-transformers` reference embeddings within tolerance — the
//! P-PAR-EMB property: `∀ text: cos(embed_rust, embed_py) ≥ 1 - tol`. We require
//! cosine ≥ 0.999 per case (design Risks: "assert cosine ≈ 1.0").
//!
//! ## Offline / graceful skip
//!
//! This whole file only compiles under `--features onnx` (the backend itself is
//! feature-gated), so a plain `cargo test` has no parity test to run and stays
//! green. Even with the feature on, the test **skips** (prints why and returns
//! Ok) when the model assets or the reference fixture are absent — neither can be
//! produced in an offline environment without downloading the model. It never
//! fabricates parity numbers: no assets ⇒ no assertion, just a skip.
//!
//! ## Inputs
//!
//! 1. The ONNX model + tokenizer ship as checked-in assets at
//!    `assets/models/bge-small-en-v1.5/{model.onnx,tokenizer.json,pooling.json}`
//!    (or set `COGNIS_ONNX_MODEL_DIR` to wherever you put them).
//! 2. The `sentence-transformers` reference vectors are a frozen oracle capture
//!    at `crates/cognis-embed/tests/fixtures/bge_parity_golden.json`. There is
//!    no Python toolchain to regenerate them.
//! 3. Run with the backend compiled in:
//!    `cargo test -p cognis-embed --features onnx --test onnx_parity -- --nocapture`

#![cfg(feature = "onnx")]

use std::path::PathBuf;

use cognis_core::Config;
use cognis_embed::Embedder;
use cognis_embed::OnnxEmbedder;
use serde_json::Value;

/// Cosine-similarity floor for a passing parity case (design: cosine ≈ 1.0).
const COSINE_MIN: f64 = 0.999;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Resolve the model asset dir the same way the backend does, so the test and
/// the production factory look in the same place.
fn model_dir(model: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("COGNIS_ONNX_MODEL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let leaf = model.rsplit('/').next().unwrap_or(model);
    PathBuf::from("assets").join("models").join(leaf)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[test]
fn onnx_local_matches_sentence_transformers_reference() {
    let cfg = Config::default();
    let model = cfg.embedder.model.clone();
    let dir = model_dir(&model);

    // --- graceful skip: assets absent (offline) -------------------------------
    if !dir.join("model.onnx").exists() || !dir.join("tokenizer.json").exists() {
        eprintln!(
            "SKIP onnx parity: model assets not found in {dir:?} \
             (expected the checked-in assets under assets/models/)."
        );
        return;
    }
    let golden_path = fixtures_dir().join("bge_parity_golden.json");
    if !golden_path.exists() {
        eprintln!(
            "SKIP onnx parity: reference fixture {golden_path:?} not found \
             (frozen oracle capture)."
        );
        return;
    }

    // --- real parity assertion -------------------------------------------------
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("read golden"))
            .expect("parse golden json");
    let cases = golden["cases"].as_array().expect("golden.cases array");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let dim = cfg.embedder.dim as usize;
    let embedder = OnnxEmbedder::from_model_dir(&dir, dim).expect("load onnx embedder");

    let mut worst = 1.0_f64;
    for case in cases {
        let text = case["text"].as_str().expect("case.text");
        let reference: Vec<f32> = case["embedding"]
            .as_array()
            .expect("case.embedding array")
            .iter()
            .map(|v| v.as_f64().expect("embedding float") as f32)
            .collect();
        assert_eq!(reference.len(), dim, "reference dim mismatch for {text:?}");

        let got = embedder.embed_text(text).expect("embed");
        assert_eq!(got.len(), dim, "rust embedding dim mismatch for {text:?}");

        let cos = cosine(&got, &reference);
        worst = worst.min(cos);
        assert!(
            cos >= COSINE_MIN,
            "cosine parity below floor for {text:?}: cos={cos:.6} < {COSINE_MIN}"
        );
    }
    eprintln!(
        "onnx parity OK: {} cases, worst cosine {worst:.6} (>= {COSINE_MIN})",
        cases.len()
    );
}
