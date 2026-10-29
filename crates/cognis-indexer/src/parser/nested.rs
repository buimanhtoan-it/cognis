//! Shared nested-scope extractor for brace languages with `Outer.Inner.method`
//! qualified names. Backs both the C# and Java extractors (mirrors the
//! identical `_walk`/`_emit` structure in `parsers/csharp.py` + `parsers/java.py`).

use cognis_core::{Symbol, SymbolKind};
use tree_sitter::Node;

use super::mk_symbol;
use super::normalize::make_symbol_id;
use super::support::{line_end, line_start, node_text};

/// Per-language configuration for the generic walker.
pub(super) struct NestedConfig {
    /// `(node_kind, emitted SymbolKind, signature keyword)` for type decls.
    pub type_decls: &'static [(&'static str, SymbolKind, &'static str)],
    /// Node kinds emitted as methods (do not descend into their bodies).
    pub method_decls: &'static [&'static str],
    /// Node kinds treated as doc comments.
    pub comment_kinds: &'static [&'static str],
    /// Delimiters that terminate a method signature, tried in order.
    pub sig_delims: &'static [&'static str],
    /// Whether `///` is a doc-comment prefix (C# XML docs).
    pub triple_slash: bool,
}

pub(super) fn extract(
    root: Node,
    src: &[u8],
    file_path: &str,
    module: &str,
    lang: &str,
    label: &str,
    cfg: &NestedConfig,
) -> Vec<Symbol> {
    let ctx = Ctx {
        src,
        file_path,
        module,
        lang,
        label,
        cfg,
    };
    let mut out = Vec::new();
    walk(&ctx, root, &[], &mut out);
    out
}

struct Ctx<'a> {
    src: &'a [u8],
    file_path: &'a str,
    module: &'a str,
    lang: &'a str,
    label: &'a str,
    cfg: &'a NestedConfig,
}

fn name_of<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn walk(ctx: &Ctx, node: Node, scope: &[String], out: &mut Vec<Symbol>) {
    let kind = node.kind();

    if let Some(&(_, sym_kind, keyword)) = ctx.cfg.type_decls.iter().find(|(k, _, _)| *k == kind) {
        let mut child_scope = scope.to_vec();
        if let Some(name) = name_of(node, ctx.src).map(str::to_string) {
            let sig = format!("{keyword} {name}");
            emit(ctx, node, scope, &name, sym_kind, sig, out);
            child_scope.push(name);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(ctx, child, &child_scope, out);
        }
        return;
    }

    if ctx.cfg.method_decls.contains(&kind) {
        if let Some(name) = name_of(node, ctx.src).map(str::to_string) {
            let sig = extract_signature(node, ctx.src, ctx.cfg.sig_delims);
            emit(ctx, node, scope, &name, SymbolKind::Method, sig, out);
        }
        return; // do not descend into method bodies
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(ctx, child, scope, out);
    }
}

fn emit(
    ctx: &Ctx,
    node: Node,
    scope: &[String],
    name: &str,
    kind: SymbolKind,
    signature: String,
    out: &mut Vec<Symbol>,
) {
    let qual = if scope.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", scope.join("."), name)
    };
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, qual);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &qual, body_text);
    out.push(mk_symbol(
        id,
        kind,
        name.to_string(),
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(signature),
        extract_docstring(ctx, node),
        body_text,
    ));
}

fn extract_signature(node: Node, src: &[u8], delims: &[&str]) -> String {
    let full = node_text(node, src);
    let mut cut = full.len();
    for d in delims {
        if let Some(idx) = full.find(d) {
            cut = cut.min(idx);
        }
    }
    full[..cut].trim().chars().take(256).collect()
}

fn extract_docstring(ctx: &Ctx, node: Node) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let siblings: Vec<Node> = parent.children(&mut cursor).collect();
    let idx = siblings.iter().position(|c| *c == node)?;
    let mut doc_lines: Vec<String> = Vec::new();
    let mut j = idx as isize - 1;
    while j >= 0 && ctx.cfg.comment_kinds.contains(&siblings[j as usize].kind()) {
        let text = node_text(siblings[j as usize], ctx.src).trim().to_string();
        if ctx.cfg.triple_slash && text.starts_with("///") {
            doc_lines.insert(0, text[3..].trim().to_string());
        } else if let Some(rest) = text.strip_prefix("//") {
            doc_lines.insert(0, rest.trim().to_string());
        } else if text.starts_with("/*") {
            let inner = text
                .trim_start_matches("/*")
                .trim_end_matches("*/")
                .trim_matches('*')
                .trim();
            let cleaned: Vec<String> = inner
                .lines()
                .map(|l| l.trim_start_matches([' ', '*']).trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            for (i, line) in cleaned.into_iter().enumerate() {
                doc_lines.insert(i, line);
            }
        }
        j -= 1;
    }
    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
}
