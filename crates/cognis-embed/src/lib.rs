//! cognis-embed — swappable embeddings (Requirement 7).
//!
//! The embedding seam every entry point shares. Retrieval's semantic layer and
//! the indexer both obtain their embedder through the single [`build_embedder`]
//! factory (mirroring the Python `cognis_indexer.registry.build_embedder`), and
//! their reranker through [`build_reranker`] — so the backend is chosen once,
//! from `.cognis/config.yaml` (`embedder.backend` / `reranker.*`), with no
//! `if backend == ...` chains duplicated across call sites (Requirement 7.1).
//!
//! This slice (Task 6.1) lands the [`Embedder`] / [`Reranker`] traits, both
//! factories, the `stub` backend (deterministic zero-vector embedder), and the
//! [`NoOpReranker`] pass-through used whenever `reranker.enabled = false`
//! (Requirement 7.3 — flow byte-unchanged). The production `onnx-local`
//! backend (bge-small via `ort`) lands in Task 6.2.

pub use cognis_core::Result;
use cognis_core::{CognisError, Config, Hit};

mod reranker;
mod stub;

#[cfg(feature = "_onnx")]
mod onnx;

pub use reranker::NoOpReranker;
pub use stub::StubEmbedder;

#[cfg(feature = "_onnx")]
pub use onnx::OnnxEmbedder;

/// An embedding backend: turns text into fixed-dimension dense vectors.
///
/// Field-compatible with the Python `Embedder` protocol
/// (`embed_text` / `embed_batch` / `embedding_dim`). Every vector a backend
/// returns has length [`Embedder::embedding_dim`], so callers can size the
/// `symbol_vec` table from a single source of truth (Requirement 2.3).
pub trait Embedder {
    /// Dimension of every vector this backend produces.
    fn embedding_dim(&self) -> usize;

    /// Embed a single text into one `embedding_dim`-length vector.
    fn embed_text(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts, returning one vector per input in order.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// A reranker: reorders a fused candidate set with a stronger relevance model
/// before the capsule composer trims to the token budget.
///
/// Mirrors the Python `Reranker` protocol. The default [`NoOpReranker`] returns
/// hits unchanged so the engine's call shape is uniform — call sites always
/// call `rerank(...)` without branching on `reranker.enabled` themselves.
pub trait Reranker {
    /// Reorder `hits` for query `q`. The pass-through implementation returns
    /// them unchanged.
    fn rerank(&self, q: &str, hits: Vec<Hit>) -> Result<Vec<Hit>>;
}

/// The single embedder factory every entry point calls (mirror of
/// `registry.build_embedder`). Selects the backend from `cfg.embedder.backend`.
///
/// Supported backends:
/// - `"stub"` — deterministic zero-vector embedder of `cfg.embedder.dim`
///   dimensions (no model, no I/O); used for tests and as a degradation target.
/// - `"local"` / `"onnx-local"` — production bge-small via ONNX Runtime
///   (`ort`). `"local"` is the default `.cognis/config.yaml` backend id (mirror
///   of the Python engine's `embedder.backend: local`); it is an alias for the
///   native `onnx-local` backend so a default config selects the real embedder.
///   Only built when the crate is compiled with `--features onnx`; otherwise
///   these ids return a clear error telling the caller to rebuild with the
///   feature (callers degrade to no embedder — semantic search off — rather
///   than failing).
///
/// Any other backend id is rejected with a clear error so callers can choose
/// their own degradation policy (Requirement 7.1).
pub fn build_embedder(cfg: &Config) -> Result<Box<dyn Embedder>> {
    match cfg.embedder.backend.as_str() {
        "stub" => Ok(Box::new(StubEmbedder::new(cfg.embedder.dim as usize))),
        // "local" is the Python-config default id; treat it as the native
        // ONNX backend so an out-of-the-box config gets real embeddings.
        "local" | "onnx-local" => build_onnx_embedder(cfg),
        other => Err(CognisError::Model(format!(
            "unsupported embedder backend {other:?}; available: [\"stub\", \"local\", \"onnx-local\"]"
        ))),
    }
}

/// Construct the `onnx-local` backend (bge-small via `ort`) — Requirement 7.2.
#[cfg(feature = "_onnx")]
fn build_onnx_embedder(cfg: &Config) -> Result<Box<dyn Embedder>> {
    let dir = onnx::resolve_model_dir(&cfg.embedder.model);
    let emb = OnnxEmbedder::from_model_dir(&dir, cfg.embedder.dim as usize)?;
    Ok(Box::new(emb))
}

/// Stand-in when the crate is built without the `onnx` feature: the id is known
/// but the backend wasn't compiled in, so report exactly how to enable it
/// rather than pretending the backend is merely unknown.
#[cfg(not(feature = "_onnx"))]
fn build_onnx_embedder(_cfg: &Config) -> Result<Box<dyn Embedder>> {
    Err(CognisError::Model(
        "embedder backend \"onnx-local\" was not compiled in; rebuild cognis-embed \
         with `--features onnx` (ONNX Runtime via `ort`) to enable it, or use \"stub\""
            .into(),
    ))
}

/// The single reranker factory every entry point calls (mirror of
/// `build_reranker`). Returns a [`NoOpReranker`] whenever
/// `cfg.reranker.enabled` is `false`, so the flow is byte-unchanged versus
/// having no reranker at all (Requirement 7.3).
///
/// In this slice the only backend is the pass-through; the cross-encoder
/// backend is registered alongside the ONNX work. An enabled reranker with an
/// unknown backend is rejected rather than silently ignored.
pub fn build_reranker(cfg: &Config) -> Result<Box<dyn Reranker>> {
    if !cfg.reranker.enabled {
        return Ok(Box::new(NoOpReranker));
    }
    Err(CognisError::Model(format!(
        "unsupported reranker backend {:?}; only the pass-through (reranker.enabled=false) \
         is available in this slice",
        cfg.reranker.backend
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_backend(backend: &str, dim: u32) -> Config {
        let mut c = Config::default();
        c.embedder.backend = backend.into();
        c.embedder.dim = dim;
        c
    }

    #[test]
    fn factory_builds_stub_with_configured_dim() {
        let cfg = cfg_with_backend("stub", 384);
        let emb = build_embedder(&cfg).unwrap();
        assert_eq!(emb.embedding_dim(), 384);
        assert_eq!(emb.embed_text("anything").unwrap(), vec![0.0_f32; 384]);
    }

    #[test]
    fn factory_rejects_unknown_backend() {
        let cfg = cfg_with_backend("totally-unknown", 384);
        assert!(matches!(build_embedder(&cfg), Err(CognisError::Model(_))));
    }

    #[cfg(not(feature = "_onnx"))]
    #[test]
    fn onnx_local_without_feature_reports_how_to_enable() {
        let cfg = cfg_with_backend("onnx-local", 384);
        match build_embedder(&cfg) {
            Err(CognisError::Model(msg)) => {
                assert!(
                    msg.contains("--features onnx"),
                    "message guides rebuild: {msg}"
                );
            }
            Err(other) => panic!("expected Model error, got {other:?}"),
            Ok(_) => panic!("expected onnx-local to be unavailable without the feature"),
        }
    }

    #[cfg(not(feature = "_onnx"))]
    #[test]
    fn local_is_an_alias_for_the_onnx_backend() {
        // The default `.cognis/config.yaml` ships `embedder.backend: local`. It
        // must route to the native ONNX backend (not be rejected as unknown),
        // so an out-of-the-box config gets real embeddings once the feature is
        // compiled in — and degrades with the same rebuild hint when it isn't.
        let cfg = cfg_with_backend("local", 384);
        match build_embedder(&cfg) {
            Err(CognisError::Model(msg)) => {
                assert!(
                    msg.contains("--features onnx"),
                    "message guides rebuild: {msg}"
                );
            }
            Err(other) => panic!("expected Model error, got {other:?}"),
            Ok(_) => panic!("expected local/onnx unavailable without the feature"),
        }
    }

    #[test]
    fn default_config_backend_is_local_alias() {
        // Guard against the factory and the config default drifting apart.
        assert_eq!(Config::default().embedder.backend, "local");
    }

    #[test]
    fn reranker_factory_returns_noop_when_disabled() {
        let cfg = Config::default(); // reranker.enabled = false by default
        let rr = build_reranker(&cfg).unwrap();
        let hits = vec![
            Hit::new("a", 3.0, "lexical", "m"),
            Hit::new("b", 2.0, "lexical", "m"),
        ];
        assert_eq!(rr.rerank("q", hits.clone()).unwrap(), hits);
    }

    #[test]
    fn reranker_factory_rejects_enabled_unknown_backend() {
        let mut cfg = Config::default();
        cfg.reranker.enabled = true;
        assert!(matches!(build_reranker(&cfg), Err(CognisError::Model(_))));
    }
}
