//! Retrieval hit — the shared result type for the retrieval mesh.
//!
//! Mirrors the Python `cognis_retrieval.base.Hit` dataclass field-for-field so
//! the Rust engine produces the same hit shape on the same DB (Requirement
//! 4.2). It lives in `cognis-core` — the dependency-neutral foundation — rather
//! than `cognis-retrieval` because the `cognis-store` `SymbolStore` trait
//! returns `Hit` from its `fts_search` / `vec_search` primitives, and
//! `cognis-retrieval` already depends on `cognis-store`. Defining `Hit` here
//! lets `store`, `retrieval` and `csar` share one type without a dependency
//! cycle; `cognis-retrieval` (Task 5.1) re-exports it so the design's stated
//! home still surfaces it.

use serde::{Deserialize, Serialize};

/// A single retrieval result from any layer.
///
/// Field-compatible with the Python `Hit` dataclass: `symbol_id`, `score`,
/// `layer`, `reason`, `evidence`. `score` follows the engine convention
/// "higher is better" (the lexical layer inverts FTS5's negative BM25 rank).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Matching symbol id (`<lang>:<path>:<qname>@<hash>`).
    pub symbol_id: String,
    /// Layer-specific relevance score (higher is better).
    pub score: f64,
    /// Producing layer: `"lexical"` | `"semantic"` | `"structural"` | `"csar"`.
    pub layer: String,
    /// Short human-readable explanation of why this symbol matched.
    pub reason: String,
    /// Layer-specific payload, e.g. `{"snippet": "..."}` for lexical hits.
    /// Defaults to a JSON object so the shape round-trips like the Python
    /// dataclass's `field(default_factory=dict)`.
    #[serde(default = "empty_evidence")]
    pub evidence: serde_json::Value,
}

fn empty_evidence() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl Hit {
    /// Construct a hit, defaulting `evidence` to an empty JSON object (matches
    /// the Python dataclass default).
    pub fn new(
        symbol_id: impl Into<String>,
        score: f64,
        layer: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Hit {
            symbol_id: symbol_id.into(),
            score,
            layer: layer.into(),
            reason: reason.into(),
            evidence: empty_evidence(),
        }
    }

    /// Builder-style setter for the layer-specific evidence payload.
    pub fn with_evidence(mut self, evidence: serde_json::Value) -> Self {
        self.evidence = evidence;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_json_roundtrip() {
        let h = Hit::new("py:m.py:foo@abc12345", 1.5, "lexical", "match")
            .with_evidence(serde_json::json!({"snippet": "fo«o»"}));
        let json = serde_json::to_string(&h).unwrap();
        let back: Hit = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn evidence_defaults_to_empty_object() {
        let h: Hit =
            serde_json::from_str(r#"{"symbol_id":"a","score":0.0,"layer":"lexical","reason":"r"}"#)
                .unwrap();
        assert_eq!(h.evidence, serde_json::json!({}));
    }
}
