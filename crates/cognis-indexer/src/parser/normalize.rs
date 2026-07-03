//! AST-text normalizer + content-hash / symbol-id utilities.
//!
//! Rust mirror of `packages/indexer/cognis_indexer/parsers/_normalize.py`. This
//! is the foundation of CP-1 (index idempotency) and CP-2 (symbol id stability
//! under cosmetic edits):
//!
//! - Cosmetic edits (whitespace-only / comment-only) MUST yield the same
//!   `content_hash`.
//! - Structural edits (rename / signature / body change) MUST yield a different
//!   `content_hash`.
//!
//! The normalizer operates on raw text (not tree-sitter nodes) and applies the
//! exact same passes, in the same order, as the Python implementation so the
//! Rust engine produces byte-identical symbol ids on the same input
//! (Requirement 9.2 parity).

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// `short_hash` length (hex chars) appended to a symbol id.
const SHORT_HASH_LEN: usize = 16;

// Compiled lazily once; each mirrors a pattern in `_normalize.py`.
fn block_comment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `/* … */` (TypeScript/Go/C#/Java). `(?s)` = DOTALL so it spans lines.
    RE.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").unwrap())
}

fn triple_double() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?s)""".*?""""#).unwrap())
}

fn triple_single() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)'''.*?'''").unwrap())
}

fn single_line_comment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `// …` (TS/Go/C#/Java) and `# …` (Python), up to end of line.
    RE.get_or_init(|| Regex::new(r"(//|#)[^\n]*").unwrap())
}

fn whitespace() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Return a whitespace- and comment-stripped version of `text`.
///
/// Pass order matches `_normalize.py::normalize_body` exactly: block comments,
/// triple-double, triple-single, single-line, then whitespace collapse + trim.
pub fn normalize_body(text: &str) -> String {
    let text = block_comment().replace_all(text, " ");
    let text = triple_double().replace_all(&text, " ");
    let text = triple_single().replace_all(&text, " ");
    let text = single_line_comment().replace_all(&text, " ");
    let text = whitespace().replace_all(&text, " ");
    text.trim().to_string()
}

/// Return `sha256(normalize(body_text))[:16]` — the `short_hash` id component.
pub fn content_hash(body_text: &str) -> String {
    let normalized = normalize_body(body_text);
    let digest = Sha256::digest(normalized.as_bytes());
    let hex = hex_lower(&digest);
    hex[..SHORT_HASH_LEN].to_string()
}

/// Construct the stable symbol id: `<lang>:<file_path>:<qualified_name>@<short_hash>`.
pub fn make_symbol_id(
    lang: &str,
    file_path: &str,
    qualified_name: &str,
    body_text: &str,
) -> String {
    format!(
        "{lang}:{file_path}:{qualified_name}@{}",
        content_hash(body_text)
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_only_edit_is_stable() {
        let a = "def foo():\n    return 1";
        let b = "def   foo():\n\n        return 1\n";
        assert_eq!(content_hash(a), content_hash(b));
    }

    #[test]
    fn comment_only_edit_is_stable() {
        let a = "fn foo() { return 1; } // old comment";
        let b = "fn foo() { return 1; } // a totally different comment";
        assert_eq!(content_hash(a), content_hash(b));
    }

    #[test]
    fn block_and_docstring_comments_stripped() {
        let a = "/* header */ fn foo() {}";
        let b = "fn foo() {}";
        assert_eq!(content_hash(a), content_hash(b));
        let c = "def f():\n    \"\"\"doc one\"\"\"\n    return 1";
        let d = "def f():\n    \"\"\"doc two changed\"\"\"\n    return 1";
        assert_eq!(content_hash(c), content_hash(d));
    }

    #[test]
    fn structural_edit_changes_hash() {
        assert_ne!(content_hash("fn foo() {}"), content_hash("fn bar() {}"));
    }

    #[test]
    fn symbol_id_shape() {
        let id = make_symbol_id("py", "src/m.py", "m.foo", "def foo(): pass");
        assert!(id.starts_with("py:src/m.py:m.foo@"));
        let hash = id.rsplit('@').next().unwrap();
        assert_eq!(hash.len(), SHORT_HASH_LEN);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
