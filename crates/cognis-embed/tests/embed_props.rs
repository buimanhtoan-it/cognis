//! Property tests for the embedding seam (Task 6.1).
//!
//! These exercise the two universal properties the slice must hold across all
//! inputs, complementing the example-based unit tests in the crate.

use cognis_core::{Config, Hit};
use cognis_embed::{build_embedder, build_reranker, NoOpReranker, Reranker};
use proptest::prelude::*;

fn stub_config(dim: u32) -> Config {
    let mut c = Config::default();
    c.embedder.backend = "stub".into();
    c.embedder.dim = dim;
    c
}

fn arb_hit() -> impl Strategy<Value = Hit> {
    (any::<String>(), any::<f64>(), 0usize..4).prop_map(|(id, score, layer_idx)| {
        let layer = ["lexical", "semantic", "structural", "csar"][layer_idx];
        Hit::new(id, score, layer, "prop")
    })
}

proptest! {
    /// Validates: Requirements 7.1
    ///
    /// The `stub` backend built via the shared factory yields one zero vector
    /// of exactly `embedder.dim` length for every text, for both `embed_text`
    /// and `embed_batch` — the dimension contract the `symbol_vec` table relies
    /// on, independent of input content.
    #[test]
    fn stub_embeddings_are_zero_vectors_of_configured_dim(
        dim in 0u32..1024,
        texts in proptest::collection::vec(any::<String>(), 0..16),
    ) {
        let emb = build_embedder(&stub_config(dim)).unwrap();
        let d = dim as usize;
        prop_assert_eq!(emb.embedding_dim(), d);

        for t in &texts {
            let v = emb.embed_text(t).unwrap();
            prop_assert_eq!(v.len(), d);
            prop_assert!(v.iter().all(|&x| x == 0.0));
        }

        let batch = emb.embed_batch(&texts).unwrap();
        prop_assert_eq!(batch.len(), texts.len());
        for v in &batch {
            prop_assert_eq!(v.len(), d);
            prop_assert!(v.iter().all(|&x| x == 0.0));
        }
    }

    /// Validates: Requirements 7.3
    ///
    /// With `reranker.enabled = false` the factory returns the pass-through, and
    /// the pass-through is the identity on the hit list for any query and any
    /// hits — so the retrieval flow is byte-unchanged versus having no reranker.
    #[test]
    fn noop_reranker_is_identity_on_hits(
        q in any::<String>(),
        hits in proptest::collection::vec(arb_hit(), 0..32),
    ) {
        // Built from a disabled-reranker config…
        let rr = build_reranker(&Config::default()).unwrap();
        prop_assert_eq!(rr.rerank(&q, hits.clone()).unwrap(), hits.clone());
        // …and the concrete type directly.
        prop_assert_eq!(NoOpReranker.rerank(&q, hits.clone()).unwrap(), hits);
    }
}
