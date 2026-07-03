//! Embedding-latency bench — the `ort` vs `candle` decision harness (Task 6.3).
//!
//! Open Question 1 (design.md) asks which embedding runtime cognis should ship:
//! `ort` (ONNX Runtime) or `candle` (pure-Rust). The decision is recorded in
//! `docs/decisions/ADR-0001-embedding-runtime.md`; the *speed* axis of that ADR
//! is labelled **conjectured** because the model assets cannot be downloaded in
//! the offline CI/dev environment. This bench is the runnable methodology that
//! turns that conjecture into an **empirically-supported (n=…)** result the day
//! the assets are present — it does **not** fabricate numbers.
//!
//! ## What it measures
//!
//! Wall-clock latency of the production `onnx-local` backend (bge-small via
//! `ort`) for two regimes that match how the indexer and the semantic retrieval
//! layer actually call it:
//!   - `embed_text` — single short query (the retrieval read path).
//!   - `embed_batch/32` — a batch of documents (the indexer write path).
//!
//! ## How to get a candle comparison
//!
//! When a `candle-local` backend is added (see ADR-0001 "Re-evaluation
//! triggers"), register it here behind a `candle` feature with the same two
//! `bench_function` calls and identical inputs, so the two runtimes are compared
//! apples-to-apples on this machine. Record the result with its evidence tier in
//! `docs/native-core-rust.md` and the CHANGELOG (evidence discipline).
//!
//! ## Offline / graceful skip
//!
//! Compiles only under `--features onnx` (enforced by `required-features` in
//! Cargo.toml). Even then it **skips** (prints why, runs no Criterion group)
//! when the model assets are absent — so `cargo bench -p cognis-embed
//! --features onnx` is green offline and only produces numbers with real assets.
//!
//! Run (needs the checked-in model assets under assets/models/):
//!   cargo bench -p cognis-embed --features onnx

#![cfg(feature = "onnx")]

use std::path::PathBuf;

use cognis_core::Config;
use cognis_embed::{Embedder, OnnxEmbedder};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

/// Resolve the model asset dir the same way the backend/parity test do.
fn model_dir(model: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("COGNIS_ONNX_MODEL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let leaf = model.rsplit('/').next().unwrap_or(model);
    PathBuf::from("assets").join("models").join(leaf)
}

/// A small, representative corpus: one short query plus a 32-doc batch of
/// code-ish sentences (the shape the indexer embeds per file batch).
fn sample_batch() -> Vec<String> {
    let seeds = [
        "fn build_code_graph(store: &Store) -> CodeGraph",
        "class RetrievalLayer: def search(self, query, k)",
        "reciprocal rank fusion of bm25 and dense vectors",
        "personalized pagerank forward push on a call graph",
    ];
    (0..32)
        .map(|i| format!("{} // variant {i}", seeds[i % seeds.len()]))
        .collect()
}

fn bench_embed(c: &mut Criterion) {
    let cfg = Config::default();
    let dir = model_dir(&cfg.embedder.model);

    // Graceful skip: no assets ⇒ no fabricated timings, just a note.
    if !dir.join("model.onnx").exists() || !dir.join("tokenizer.json").exists() {
        eprintln!(
            "SKIP embed_latency bench: model assets not found in {dir:?} \
             (expected the checked-in assets under assets/models/)."
        );
        return;
    }

    let dim = cfg.embedder.dim as usize;
    let embedder = OnnxEmbedder::from_model_dir(&dir, dim).expect("load onnx embedder");
    let batch = sample_batch();
    let query = "where is the http retry/backoff policy configured?".to_string();

    let mut group = c.benchmark_group("onnx-local");
    group.bench_function("embed_text", |b| {
        b.iter(|| embedder.embed_text(&query).expect("embed_text"));
    });
    group.bench_function("embed_batch/32", |b| {
        b.iter_batched(
            || batch.clone(),
            |texts| embedder.embed_batch(&texts).expect("embed_batch"),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_embed);
criterion_main!(benches);
