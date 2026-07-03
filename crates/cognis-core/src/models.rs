//! UCKG data models — Rust mirror of `packages/core/cognis/models.py`.
//!
//! Field names and JSON shapes are kept identical to the pydantic models so the
//! Rust engine round-trips the same UCKG rows (Requirement 2.2). Validation that
//! pydantic did at construction time is provided via [`Symbol::validate`] /
//! [`Edge::validate`] rather than at deserialize time (the DB is trusted input).

use serde::{Deserialize, Serialize};

use crate::CognisError;

/// Allowed `Symbol::kind` values (mirrors `SymbolKind` literal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Interface,
    Route,
    Module,
    Var,
    Const,
}

/// Allowed `Edge::kind` values (mirrors `EdgeKind` literal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Calls,
    Imports,
    Inherits,
    Implements,
    Reads,
    Writes,
    RoutesTo,
    Tests,
}

/// Allowed `FileRecord::parse_status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParseStatus {
    Ok,
    Partial,
    Failed,
}

/// Atomic indexable unit (mirrors `SymbolNode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    /// Stable id: `<lang>:<file_path>:<qualified_name>@<short_hash>`.
    pub id: String,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub language: String,
    pub module: String,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    pub content_hash: String,
    #[serde(default)]
    pub body_excerpt: Option<String>,
    #[serde(default)]
    pub semantic_summary: Option<String>,
    #[serde(default)]
    pub risk_score: f64,
    #[serde(default)]
    pub ambiguous: bool,
    #[serde(default)]
    pub untrusted_flags: Vec<String>,
    pub updated_at: i64,
}

impl Symbol {
    /// Field-level invariants pydantic enforced at construction.
    pub fn validate(&self) -> crate::Result<()> {
        if self.id.is_empty() {
            return Err(CognisError::Model("symbol id must be non-empty".into()));
        }
        if self.line_start < 1 || self.line_end < 1 {
            return Err(CognisError::Model("line numbers are 1-based".into()));
        }
        if self.line_end < self.line_start {
            return Err(CognisError::Model(format!(
                "line_end ({}) must be >= line_start ({})",
                self.line_end, self.line_start
            )));
        }
        if !(0.0..=1.0).contains(&self.risk_score) {
            return Err(CognisError::Model("risk_score must be in [0,1]".into()));
        }
        Ok(())
    }
}

fn default_confidence() -> f64 {
    1.0
}

/// Directed, typed relationship between two symbols (mirrors `Edge`).
/// Composite key `(src_id, dst_id, kind)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub src_id: String,
    pub dst_id: String,
    pub kind: EdgeKind,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Free-form JSON (e.g. `{"dst_missing": true}`).
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl Edge {
    pub fn validate(&self) -> crate::Result<()> {
        if self.src_id.is_empty() || self.dst_id.is_empty() {
            return Err(CognisError::Model(
                "edge endpoints must be non-empty".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(CognisError::Model("confidence must be in [0,1]".into()));
        }
        Ok(())
    }

    /// Whether `meta.dst_missing` is set (the structural-layer filter flag).
    pub fn dst_missing(&self) -> bool {
        self.meta
            .get("dst_missing")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// Enricher-extracted side-effect metadata (mirrors `SymbolAttribute`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolAttribute {
    pub symbol_id: String,
    pub key: String,
    pub value: String,
}

/// Per-file cache row used by the watcher diff path (mirrors `FileRecord`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub language: String,
    pub size_bytes: u64,
    pub content_hash: String,
    pub parsed_at: i64,
    pub parse_status: ParseStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Symbol {
        Symbol {
            id: "py:m.py:foo@abc12345".into(),
            kind: SymbolKind::Function,
            name: "foo".into(),
            qualified_name: "m.foo".into(),
            language: "python".into(),
            module: "m".into(),
            file_path: "src/m.py".into(),
            line_start: 1,
            line_end: 2,
            signature: None,
            docstring: None,
            content_hash: "abc12345".into(),
            body_excerpt: None,
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: vec![],
            updated_at: 1700000000,
        }
    }

    #[test]
    fn symbol_json_roundtrip() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        s.validate().unwrap();
    }

    #[test]
    fn symbol_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&SymbolKind::Function).unwrap(),
            "\"function\""
        );
    }

    #[test]
    fn edge_kind_routes_to_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&EdgeKind::RoutesTo).unwrap(),
            "\"routes_to\""
        );
    }

    #[test]
    fn edge_defaults_and_dst_missing() {
        // confidence defaults to 1.0, meta defaults to null→treated as absent.
        let e: Edge =
            serde_json::from_str(r#"{"src_id":"a","dst_id":"b","kind":"calls"}"#).unwrap();
        assert_eq!(e.confidence, 1.0);
        assert!(!e.dst_missing());
        let e2: Edge = serde_json::from_str(
            r#"{"src_id":"a","dst_id":"b","kind":"imports","meta":{"dst_missing":true}}"#,
        )
        .unwrap();
        assert!(e2.dst_missing());
        e2.validate().unwrap();
    }

    #[test]
    fn symbol_validate_rejects_bad_lines() {
        let mut s = sample();
        s.line_start = 5;
        s.line_end = 2;
        assert!(s.validate().is_err());
    }
}
