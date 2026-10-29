//! `NoOpReranker` — the default pass-through reranker (Null Object).
//!
//! Used whenever `reranker.enabled = false`. It returns the input hits
//! unchanged so enabling/disabling reranking never changes the engine's call
//! shape: the capsule composer always calls `rerank(...)`. With the pass-through
//! the fused order flows through byte-for-byte identical to having no reranker
//! at all (Requirement 7.3 / Property: pass-through is the identity on hits).

use cognis_core::{Hit, Result};

use crate::Reranker;

/// Pass-through reranker: returns hits unchanged (no reordering, no truncation).
///
/// The Rust [`Reranker`] trait takes no `k` (truncation to the token budget is
/// the capsule composer's job), so the pass-through is the exact identity on the
/// hit list — the strongest form of "flow byte-unchanged".
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpReranker;

impl Reranker for NoOpReranker {
    fn rerank(&self, _q: &str, hits: Vec<Hit>) -> Result<Vec<Hit>> {
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits() -> Vec<Hit> {
        vec![
            Hit::new("a", 3.0, "lexical", "m"),
            Hit::new("b", 2.0, "semantic", "m"),
            Hit::new("c", 1.0, "csar", "m"),
        ]
    }

    #[test]
    fn passthrough_returns_hits_unchanged() {
        let input = hits();
        let out = NoOpReranker.rerank("any query", input.clone()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn passthrough_preserves_empty() {
        assert!(NoOpReranker.rerank("q", Vec::new()).unwrap().is_empty());
    }
}
