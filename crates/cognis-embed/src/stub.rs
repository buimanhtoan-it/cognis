//! `stub` backend — deterministic zero-vector embedder.
//!
//! Mirrors the Python `VoyageEmbedder` MVP stub (returns zero vectors). It runs
//! with no model, no network, and no optional dependency, so it is the safe
//! degradation target and the fixture every embedder test can build on. Because
//! every vector is `embedding_dim` long, it still satisfies the `symbol_vec`
//! dimension contract (Requirement 2.3) even though the vectors carry no signal.

use cognis_core::Result;

use crate::Embedder;

/// Fixed-dimension embedder that returns the zero vector for any input.
#[derive(Debug, Clone, Copy)]
pub struct StubEmbedder {
    dim: usize,
}

impl StubEmbedder {
    /// Create a stub embedder producing `dim`-length zero vectors.
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Embedder for StubEmbedder {
    fn embedding_dim(&self) -> usize {
        self.dim
    }

    fn embed_text(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; self.dim])
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; self.dim]).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_text_is_zero_vector_of_dim() {
        let e = StubEmbedder::new(8);
        assert_eq!(e.embed_text("hello").unwrap(), vec![0.0_f32; 8]);
    }

    #[test]
    fn embed_batch_one_vector_per_input_in_order() {
        let e = StubEmbedder::new(4);
        let texts = vec!["a".to_string(), "bb".to_string(), "ccc".to_string()];
        let out = e.embed_batch(&texts).unwrap();
        assert_eq!(out.len(), 3);
        for v in &out {
            assert_eq!(v, &vec![0.0_f32; 4]);
        }
    }

    #[test]
    fn empty_batch_yields_empty() {
        let e = StubEmbedder::new(384);
        assert!(e.embed_batch(&[]).unwrap().is_empty());
    }

    #[test]
    fn zero_dim_is_empty_vector() {
        let e = StubEmbedder::new(0);
        assert_eq!(e.embedding_dim(), 0);
        assert!(e.embed_text("x").unwrap().is_empty());
    }
}
