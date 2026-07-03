//! Generic, table-driven tree-sitter extractor (multi-language support).
//!
//! Most tree-sitter grammars expose declaration nodes with a `name` field (or,
//! for C/C++, a `declarator` chain). Rather than hand-writing a bespoke module
//! per language, a language is described by a small [`GenericSpec`] — a table of
//! `node_kind -> SymbolKind` plus a line-comment prefix — and this walker
//! extracts symbols for every matching node, tracking the enclosing-definition
//! path so nested methods get a qualified name (`Class.method`).
//!
//! This is intentionally simpler than the bespoke TS/Python/Go/C#/Java parsers
//! (which encode language-specific qualified-name and receiver rules). It aims
//! for good-enough symbol coverage across many languages; a file that yields no
//! symbols still falls back to the textual `module` symbol (Requirement 9.4).

use cognis_core::{Symbol, SymbolKind};
use tree_sitter::Node;

use super::mk_symbol;
use super::normalize::make_symbol_id;
use super::support::{line_end, line_start, node_text};

/// Per-language extraction table.
pub(super) struct GenericSpec {
    /// tree-sitter node kinds we treat as definitions, and their [`SymbolKind`].
    pub defs: &'static [(&'static str, SymbolKind)],
    /// Node kinds that introduce a qualified-name scope for their descendants
    /// (e.g. a class/module) — their name is pushed onto the path.
    pub scopes: &'static [&'static str],
    /// Line-comment prefix for the preceding-comment docstring (e.g. `//`, `#`).
    pub line_comment: &'static str,
}

struct Ctx<'a> {
    src: &'a [u8],
    file_path: &'a str,
    module: &'a str,
    lang: &'a str,
    label: &'a str,
    spec: &'a GenericSpec,
}

pub(super) fn extract(
    root: Node,
    src: &[u8],
    file_path: &str,
    module: &str,
    lang: &str,
    label: &str,
    spec: &GenericSpec,
) -> Vec<Symbol> {
    let ctx = Ctx {
        src,
        file_path,
        module,
        lang,
        label,
        spec,
    };
    let mut out = Vec::new();
    walk(root, &[], &ctx, &mut out);
    out
}

/// Recursively walk `node`'s children, emitting a symbol for each definition
/// node and threading the enclosing-scope name path through nested definitions.
fn walk(node: Node, path: &[String], ctx: &Ctx, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        let def = ctx.spec.defs.iter().find(|(k, _)| *k == kind);

        if let Some((_, symbol_kind)) = def {
            if let Some(name) = def_name(child, ctx.src) {
                out.push(build_symbol(ctx, child, &name, path, *symbol_kind));
                // Descend with this definition on the path so nested members
                // (methods) qualify as `Outer.name`.
                let mut child_path = path.to_vec();
                child_path.push(name);
                walk(child, &child_path, ctx, out);
                continue;
            }
        }

        // A scope node (class/module without its own symbol emission path) still
        // pushes its name for descendants; otherwise recurse unchanged.
        if ctx.spec.scopes.contains(&kind) {
            if let Some(name) = def_name(child, ctx.src) {
                let mut child_path = path.to_vec();
                child_path.push(name);
                walk(child, &child_path, ctx, out);
                continue;
            }
        }
        walk(child, path, ctx, out);
    }
}

/// Build a [`Symbol`] for a definition node with qualified-name `path`.
fn build_symbol(ctx: &Ctx, node: Node, name: &str, path: &[String], kind: SymbolKind) -> Symbol {
    let local = if path.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", path.join("."), name)
    };
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, local);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &local, body_text);
    mk_symbol(
        id,
        kind,
        name.to_string(),
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(signature(node, ctx.src)),
        docstring(node, ctx),
        body_text,
    )
}

/// The declaration header (everything before the body `{` or `=`), truncated.
fn signature(node: Node, src: &[u8]) -> String {
    let full = node_text(node, src);
    let end = full
        .find('{')
        .or_else(|| full.find('='))
        .unwrap_or(full.len());
    full[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(256)
        .collect()
}

/// Resolve a definition node's name: the `name` field, else the `declarator`
/// chain (C/C++ functions), else the first identifier-like descendant.
fn def_name(node: Node, src: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(node_text(n, src).to_string());
    }
    if let Some(decl) = node.child_by_field_name("declarator") {
        if let Some(n) = declarator_name(decl, src) {
            return Some(n);
        }
    }
    // Shallow fallback: first identifier-ish direct child.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_name_node(child.kind()) {
            return Some(node_text(child, src).to_string());
        }
    }
    None
}

/// Follow a C/C++ `declarator` chain down to the innermost identifier.
fn declarator_name(node: Node, src: &[u8]) -> Option<String> {
    if is_name_node(node.kind()) {
        return Some(node_text(node, src).to_string());
    }
    if let Some(inner) = node.child_by_field_name("declarator") {
        return declarator_name(inner, src);
    }
    // e.g. function_declarator whose declarator is the identifier.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_name_node(child.kind()) {
            return Some(node_text(child, src).to_string());
        }
        if child.kind().ends_with("declarator") {
            if let Some(n) = declarator_name(child, src) {
                return Some(n);
            }
        }
    }
    None
}

fn is_name_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "constant"
            | "name"
            | "scoped_identifier"
            | "word"
    )
}

/// Collect the run of preceding line comments as a docstring (or `None`).
fn docstring(node: Node, ctx: &Ctx) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let siblings: Vec<Node> = parent.children(&mut cursor).collect();
    let idx = siblings.iter().position(|c| *c == node)?;
    let mut lines: Vec<String> = Vec::new();
    let mut j = idx as isize - 1;
    while j >= 0 && siblings[j as usize].kind() == "comment" {
        let text = node_text(siblings[j as usize], ctx.src).trim().to_string();
        let cleaned = text
            .strip_prefix(ctx.spec.line_comment)
            .unwrap_or(&text)
            .trim()
            .to_string();
        lines.insert(0, cleaned);
        j -= 1;
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}
