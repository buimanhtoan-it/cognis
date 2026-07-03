//! `onnx-local` backend — `bge-small-en-v1.5` native via ONNX Runtime (`ort`).
//!
//! The production [`Embedder`](crate::Embedder) (Task 6.2, Requirement 7.2): run
//! `BAAI/bge-small-en-v1.5` (384-d) with **no Python / PyTorch at runtime**. It
//! pairs the `tokenizers` crate (the model's own BERT WordPiece `tokenizer.json`)
//! with an `ort` inference session over the exported `model.onnx`, then pools the
//! token embeddings and L2-normalises — matching what `sentence-transformers`
//! does for this model so the vectors are parity-comparable (cosine ≈ 1.0).
//!
//! ## Pooling — read from the asset, not hard-coded
//!
//! The task brief says "mean-pooling", but `sentence-transformers` ships
//! `BAAI/bge-small-en-v1.5` with **CLS pooling** (`1_Pooling/config.json` →
//! `pooling_mode_cls_token: true`). Hard-coding the wrong mode would silently
//! destroy parity. So the checked-in `pooling.json` asset carries the model's
//! real pooling decision, and this backend honours it (defaulting to CLS, bge's
//! actual mode, when the file is absent). Both modes are implemented; the asset
//! picks the right one.
//!
//! ## Build / linking
//!
//! Gated behind the `onnx` cargo feature. The feature uses `ort`'s load-dynamic
//! strategy (the ONNX Runtime shared library is resolved at runtime), so the
//! crate builds offline without downloading a native lib. `--features onnx-download`
//! switches to the bundled static download for a self-contained binary.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cognis_core::{CognisError, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use tokenizers::Tokenizer;

use crate::Embedder;

/// File names inside a model asset directory.
const MODEL_FILE: &str = "model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const POOLING_FILE: &str = "pooling.json";

/// Environment variable overriding where model assets live. When unset the
/// directory is derived from the configured model id (see [`resolve_model_dir`]).
const MODEL_DIR_ENV: &str = "COGNIS_ONNX_MODEL_DIR";

/// Hard cap on tokens per sequence (bge-small's positional limit is 512).
const MAX_SEQ_LEN: usize = 512;

/// How token embeddings are reduced to one sentence vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pooling {
    /// Take the `[CLS]` token (position 0). bge-small's actual mode.
    Cls,
    /// Average token embeddings, weighted by the attention mask.
    Mean,
}

/// Pooling + normalisation policy, loaded from `pooling.json` when present.
#[derive(Debug, Clone, Copy)]
struct PoolingConfig {
    pooling: Pooling,
    normalize: bool,
}

impl Default for PoolingConfig {
    fn default() -> Self {
        // bge-small-en-v1.5 ships with CLS pooling + L2 normalisation.
        Self {
            pooling: Pooling::Cls,
            normalize: true,
        }
    }
}

/// Resolve the directory that holds the ONNX model + tokenizer assets.
///
/// Precedence:
/// 1. `COGNIS_ONNX_MODEL_DIR` env var (explicit override).
/// 2. `assets/models/<model-leaf>` next to the running executable — where the
///    release binary ships its bundled model, so a shipped `cognis` finds its
///    model regardless of the working directory.
/// 3. `assets/models/<model-leaf>` relative to the process working directory —
///    the dev/source layout.
///
/// `<model-leaf>` is the part of the configured model id after the last `/`
/// (e.g. `BAAI/bge-small-en-v1.5` → `bge-small-en-v1.5`).
pub(crate) fn resolve_model_dir(model: &str) -> PathBuf {
    if let Ok(dir) = std::env::var(MODEL_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let leaf = model.rsplit('/').next().unwrap_or(model);
    let rel = PathBuf::from("assets").join("models").join(leaf);

    // Next to the executable (shipped layout): <exe_dir>/assets/models/<leaf>.
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let candidate = exe_dir.join(&rel);
        if candidate.exists() {
            return candidate;
        }
    }
    // Fall back to CWD-relative (dev/source layout).
    rel
}

fn ort_err(e: impl std::fmt::Display) -> CognisError {
    CognisError::Model(format!("onnx-local: {e}"))
}

/// Production embedder: bge-small-en-v1.5 via ONNX Runtime.
///
/// `ort`'s [`Session::run`] takes `&mut self`, so the session is held behind a
/// [`Mutex`] to keep the [`Embedder`] trait's `&self` methods usable from shared
/// references (the registry hands out `Box<dyn Embedder>`).
pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    dim: usize,
    pooling: PoolingConfig,
}

impl std::fmt::Debug for OnnxEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxEmbedder")
            .field("dim", &self.dim)
            .field("pooling", &self.pooling)
            .finish_non_exhaustive()
    }
}

impl OnnxEmbedder {
    /// Load the embedder from a model asset directory containing `model.onnx`,
    /// `tokenizer.json`, and (optionally) `pooling.json`.
    ///
    /// `expected_dim` is the dimension the caller (the `symbol_vec` schema)
    /// expects; a mismatch versus the model's actual hidden size is reported as
    /// an error rather than silently producing wrong-width vectors.
    pub fn from_model_dir(dir: &Path, expected_dim: usize) -> Result<Self> {
        let model_path = dir.join(MODEL_FILE);
        let tok_path = dir.join(TOKENIZER_FILE);
        if !model_path.exists() {
            return Err(CognisError::Model(format!(
                "onnx-local: missing {MODEL_FILE} at {model_path:?}; \
                 the model assets ship under assets/models/"
            )));
        }
        if !tok_path.exists() {
            return Err(CognisError::Model(format!(
                "onnx-local: missing {TOKENIZER_FILE} at {tok_path:?}; \
                 the model assets ship under assets/models/"
            )));
        }

        let session = Session::builder()
            .map_err(ort_err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(ort_err)?
            .with_intra_threads(1)
            .map_err(ort_err)?
            .commit_from_file(&model_path)
            .map_err(ort_err)?;

        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| CognisError::Model(format!("onnx-local: tokenizer load: {e}")))?;

        let pooling = load_pooling(&dir.join(POOLING_FILE));

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            dim: expected_dim,
            pooling,
        })
    }

    /// Tokenize `texts` into padded `(input_ids, attention_mask, token_type_ids)`
    /// flat `i64` buffers plus the batch geometry `(batch, seq_len)`.
    fn tokenize(&self, texts: &[String]) -> Result<TokenizedBatch> {
        let inputs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let encodings = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| CognisError::Model(format!("onnx-local: tokenize: {e}")))?;

        let batch = encodings.len();
        let seq = encodings
            .iter()
            .map(tokenizers::Encoding::len)
            .max()
            .unwrap_or(0)
            .min(MAX_SEQ_LEN);

        let mut ids = vec![0i64; batch * seq];
        let mut mask = vec![0i64; batch * seq];
        let mut types = vec![0i64; batch * seq];
        for (b, enc) in encodings.iter().enumerate() {
            let e_ids = enc.get_ids();
            let e_mask = enc.get_attention_mask();
            let e_types = enc.get_type_ids();
            let n = e_ids.len().min(seq);
            for j in 0..n {
                let idx = b * seq + j;
                ids[idx] = i64::from(e_ids[j]);
                mask[idx] = i64::from(e_mask[j]);
                types[idx] = i64::from(e_types[j]);
            }
        }
        Ok(TokenizedBatch {
            ids,
            mask,
            types,
            batch,
            seq,
        })
    }
}

struct TokenizedBatch {
    ids: Vec<i64>,
    mask: Vec<i64>,
    types: Vec<i64>,
    batch: usize,
    seq: usize,
}

impl Embedder for OnnxEmbedder {
    fn embedding_dim(&self) -> usize {
        self.dim
    }

    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_batch(std::slice::from_ref(&text.to_string()))?;
        out.pop()
            .ok_or_else(|| CognisError::Model("onnx-local: empty embedding output".into()))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let tb = self.tokenize(texts)?;
        if tb.seq == 0 {
            // Degenerate: nothing to embed — return zero vectors of the right dim.
            return Ok(texts.iter().map(|_| vec![0.0_f32; self.dim]).collect());
        }
        let shape = [tb.batch, tb.seq];

        let a_ids = TensorRef::from_array_view((shape, tb.ids.as_slice())).map_err(ort_err)?;
        let a_mask = TensorRef::from_array_view((shape, tb.mask.as_slice())).map_err(ort_err)?;
        let a_types = TensorRef::from_array_view((shape, tb.types.as_slice())).map_err(ort_err)?;

        let (out_shape, hidden) = {
            let mut session = self
                .session
                .lock()
                .map_err(|_| CognisError::Model("onnx-local: session lock poisoned".into()))?;
            let outputs = session
                .run(ort::inputs![
                    "input_ids" => a_ids,
                    "attention_mask" => a_mask,
                    "token_type_ids" => a_types,
                ])
                .map_err(ort_err)?;
            let (shape, data) = outputs["last_hidden_state"]
                .try_extract_tensor::<f32>()
                .map_err(ort_err)?;
            // Copy out of the session-owned buffer so the lock can drop.
            (shape.to_vec(), data.to_vec())
        };

        // last_hidden_state: [batch, seq, hidden]
        if out_shape.len() != 3 {
            return Err(CognisError::Model(format!(
                "onnx-local: expected rank-3 last_hidden_state, got shape {out_shape:?}"
            )));
        }
        let h = out_shape[2] as usize;
        if h != self.dim {
            return Err(CognisError::Model(format!(
                "onnx-local: model hidden size {h} != configured embedder.dim {}; \
                 reconcile embedder.dim with the model",
                self.dim
            )));
        }
        let seq = out_shape[1] as usize;

        let mut result = Vec::with_capacity(tb.batch);
        for b in 0..tb.batch {
            let base = b * seq * h;
            let mut vec = match self.pooling.pooling {
                Pooling::Cls => hidden[base..base + h].to_vec(),
                Pooling::Mean => mean_pool(
                    &hidden[base..base + seq * h],
                    &tb.mask[b * tb.seq..],
                    seq,
                    h,
                ),
            };
            if self.pooling.normalize {
                l2_normalize(&mut vec);
            }
            result.push(vec);
        }
        Ok(result)
    }
}

/// Attention-mask-weighted mean over the `seq` token vectors of one sequence.
fn mean_pool(hidden: &[f32], mask: &[i64], seq: usize, h: usize) -> Vec<f32> {
    let mut acc = vec![0.0_f32; h];
    let mut denom = 0.0_f32;
    for j in 0..seq {
        let m = *mask.get(j).unwrap_or(&0);
        if m == 0 {
            continue;
        }
        denom += 1.0;
        let row = &hidden[j * h..j * h + h];
        for (a, &x) in acc.iter_mut().zip(row) {
            *a += x;
        }
    }
    if denom > 0.0 {
        for a in &mut acc {
            *a /= denom;
        }
    }
    acc
}

/// In-place L2 normalisation; a zero vector is left untouched.
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Load `pooling.json` if present; fall back to bge's defaults (CLS + L2) when
/// absent or unparsable so a missing/old asset still embeds sensibly.
fn load_pooling(path: &Path) -> PoolingConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return PoolingConfig::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return PoolingConfig::default();
    };
    let default = PoolingConfig::default();
    let pooling = match value.get("pooling").and_then(serde_json::Value::as_str) {
        Some(s) if s.eq_ignore_ascii_case("mean") => Pooling::Mean,
        Some(s) if s.eq_ignore_ascii_case("cls") => Pooling::Cls,
        _ => default.pooling,
    };
    let normalize = value
        .get("normalize")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default.normalize);
    PoolingConfig { pooling, normalize }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_dir_uses_model_leaf() {
        // No env override: derive from the model id's last path segment.
        std::env::remove_var(MODEL_DIR_ENV);
        let dir = resolve_model_dir("BAAI/bge-small-en-v1.5");
        assert!(dir.ends_with(PathBuf::from("models").join("bge-small-en-v1.5")));
    }

    #[test]
    fn pooling_defaults_to_cls_and_normalize() {
        let c = PoolingConfig::default();
        assert_eq!(c.pooling, Pooling::Cls);
        assert!(c.normalize);
    }

    #[test]
    fn load_pooling_missing_file_is_default() {
        let c = load_pooling(Path::new("does-not-exist-pooling.json"));
        assert_eq!(c.pooling, Pooling::Cls);
        assert!(c.normalize);
    }

    #[test]
    fn mean_pool_masks_padding() {
        // Two tokens, hidden=2. Second token is padding (mask 0) → averaged over
        // the single real token only.
        let hidden = vec![1.0, 3.0, /* pad */ 100.0, 100.0];
        let mask = vec![1_i64, 0];
        let out = mean_pool(&hidden, &mask, 2, 2);
        assert_eq!(out, vec![1.0, 3.0]);
    }

    #[test]
    fn l2_normalize_unit_norm() {
        let mut v = vec![3.0_f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_untouched() {
        let mut v = vec![0.0_f32; 4];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0_f32; 4]);
    }
}
