//! Shared tree-sitter node helpers used by every language parser.
//!
//! These mirror the small `_node_text` / `_find_child` / `_module_from_path`
//! helpers duplicated across the Python parser modules.

use tree_sitter::Node;

/// Max length of `Symbol::body_excerpt` (design: 1 500 chars).
pub const BODY_EXCERPT_LIMIT: usize = 1500;

/// Decode the source slice spanned by `node` (lossless on valid UTF-8).
pub fn node_text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

/// 1-based start line of `node` (tree-sitter rows are 0-based).
pub fn line_start(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

/// 1-based end line of `node`.
pub fn line_end(node: Node) -> u32 {
    node.end_position().row as u32 + 1
}

/// First direct child whose `kind()` is in `types`, or `None`.
pub fn find_child<'a>(node: Node<'a>, types: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| types.contains(&c.kind()));
    found
}

/// `src/auth/jwt.ts` → `src/auth/jwt` (strip dir-normalized extension).
///
/// Matches `_module_from_path`: forward-slash normalize, drop the final
/// extension, rejoin with `/`.
pub fn module_from_path(file_path: &str) -> String {
    let norm = file_path.replace('\\', "/");
    let parts: Vec<&str> = norm.split('/').collect();
    let last = parts[parts.len() - 1];
    let stem = match last.rfind('.') {
        Some(idx) if idx > 0 => &last[..idx],
        _ => last,
    };
    if parts.len() > 1 {
        let mut out = parts[..parts.len() - 1].join("/");
        out.push('/');
        out.push_str(stem);
        out
    } else {
        stem.to_string()
    }
}

/// Truncate `s` to at most `max` bytes, respecting char boundaries.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Body excerpt: raw text truncated to [`BODY_EXCERPT_LIMIT`].
pub fn body_excerpt(text: &str) -> String {
    truncate_chars(text, BODY_EXCERPT_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_from_nested_path() {
        assert_eq!(module_from_path("src/auth/jwt.ts"), "src/auth/jwt");
        assert_eq!(module_from_path("main.go"), "main");
        assert_eq!(module_from_path("a\\b\\c.cs"), "a/b/c");
    }

    #[test]
    fn truncate_respects_boundaries() {
        let s = "héllo";
        let t = truncate_chars(s, 2);
        assert!(s.starts_with(&t));
    }
}
