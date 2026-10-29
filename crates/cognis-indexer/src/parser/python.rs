//! Python extractor — mirror of `parsers/python.py`.
//!
//! Covers `function_definition` (top-level + nested methods), `class_definition`,
//! `decorated_definition`, and top-level ALL_CAPS assignments (`kind="const"`).

use cognis_core::{Symbol, SymbolKind};
use tree_sitter::Node;

use super::mk_symbol;
use super::normalize::make_symbol_id;
use super::support::{find_child, line_end, line_start, node_text};

pub(super) fn extract(
    root: Node,
    src: &[u8],
    file_path: &str,
    module: &str,
    lang: &str,
    label: &str,
) -> Vec<Symbol> {
    let mut out = Vec::new();
    let ctx = Ctx {
        src,
        file_path,
        module,
        lang,
        label,
    };
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        visit_top_level(&ctx, child, &mut out);
    }
    out
}

struct Ctx<'a> {
    src: &'a [u8],
    file_path: &'a str,
    module: &'a str,
    lang: &'a str,
    label: &'a str,
}

fn visit_top_level(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    match node.kind() {
        "function_definition" | "async_function_definition" => {
            if let Some(s) = handle_function(ctx, node, None, SymbolKind::Function) {
                out.push(s);
            }
        }
        "decorated_definition" => handle_decorated(ctx, node, out),
        "class_definition" => handle_class(ctx, node, out),
        "expression_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "assignment" {
                    if let Some(s) = handle_assignment(ctx, child) {
                        out.push(s);
                    }
                    break;
                }
            }
        }
        _ => {}
    }
}

fn get_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(node_text(n, src));
    }
    find_child(node, &["identifier"]).map(|n| node_text(n, src))
}

fn is_async_function(node: Node) -> bool {
    if node.kind() == "async_function_definition" {
        return true;
    }
    let mut cursor = node.walk();
    let is_async = node.children(&mut cursor).any(|c| c.kind() == "async");
    is_async
}

fn extract_signature(node: Node, src: &[u8], name: &str) -> String {
    let params = node
        .child_by_field_name("parameters")
        .or_else(|| find_child(node, &["parameters"]));
    let prefix = if is_async_function(node) {
        "async def"
    } else {
        "def"
    };
    match params {
        Some(p) => format!("{prefix} {name}{}", node_text(p, src)),
        None => format!("{prefix} {name}()"),
    }
}

fn extract_docstring(node: Node, src: &[u8]) -> Option<String> {
    let body = node
        .child_by_field_name("body")
        .or_else(|| find_child(node, &["block"]))?;
    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        if stmt.kind() == "expression_statement" {
            if let Some(expr) = find_child(stmt, &["string"]) {
                let text = node_text(expr, src);
                return Some(strip_py_quotes(text));
            }
        }
        // The docstring must be the first real statement.
        if !matches!(stmt.kind(), "comment" | "\n" | "pass_statement") {
            break;
        }
    }
    None
}

fn strip_py_quotes(text: &str) -> String {
    for triple in ["\"\"\"", "'''"] {
        if text.starts_with(triple) && text.ends_with(triple) && text.len() >= 6 {
            return text[3..text.len() - 3].trim().to_string();
        }
    }
    if (text.starts_with('"') && text.ends_with('"')
        || text.starts_with('\'') && text.ends_with('\''))
        && text.len() >= 2
    {
        return text[1..text.len() - 1].trim().to_string();
    }
    text.trim().to_string()
}

fn is_all_caps(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    name == name.to_uppercase() && name.chars().any(|c| c.is_alphabetic())
}

fn handle_function(
    ctx: &Ctx,
    node: Node,
    class_prefix: Option<&str>,
    kind: SymbolKind,
) -> Option<Symbol> {
    let name = get_name(node, ctx.src)?.to_string();
    let body_text = node_text(node, ctx.src);
    let qual = match class_prefix {
        Some(c) => format!("{c}.{name}"),
        None => name.clone(),
    };
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, qual);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &qual, body_text);
    let sig = extract_signature(node, ctx.src, &name);
    let doc = extract_docstring(node, ctx.src);
    Some(mk_symbol(
        id,
        kind,
        name,
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(sig),
        doc,
        body_text,
    ))
}

fn handle_class(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let Some(name) = get_name(node, ctx.src).map(str::to_string) else {
        return;
    };
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
    let doc = extract_docstring(node, ctx.src);
    out.push(mk_symbol(
        id,
        SymbolKind::Class,
        name.clone(),
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(format!("class {name}")),
        doc,
        body_text,
    ));

    // Methods from the class body.
    let body = node
        .child_by_field_name("body")
        .or_else(|| find_child(node, &["block"]));
    if let Some(body) = body {
        let mut cursor = body.walk();
        for stmt in body.children(&mut cursor) {
            match stmt.kind() {
                "function_definition" | "async_function_definition" => {
                    if let Some(s) = handle_function(ctx, stmt, Some(&name), SymbolKind::Method) {
                        out.push(s);
                    }
                }
                "decorated_definition" => {
                    let mut inner = stmt.walk();
                    for c in stmt.children(&mut inner) {
                        if matches!(
                            c.kind(),
                            "function_definition" | "async_function_definition"
                        ) {
                            if let Some(s) =
                                handle_function(ctx, c, Some(&name), SymbolKind::Method)
                            {
                                out.push(s);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn handle_decorated(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" | "async_function_definition" => {
                if let Some(s) = handle_function(ctx, child, None, SymbolKind::Function) {
                    out.push(s);
                }
            }
            "class_definition" => handle_class(ctx, child, out),
            _ => {}
        }
    }
}

fn handle_assignment(ctx: &Ctx, node: Node) -> Option<Symbol> {
    let left = node
        .child_by_field_name("left")
        .or_else(|| find_child(node, &["identifier"]))?;
    if left.kind() != "identifier" {
        return None;
    }
    let name = node_text(left, ctx.src).to_string();
    if !is_all_caps(&name) {
        return None;
    }
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
    let sig: String = body_text.chars().take(256).collect();
    Some(mk_symbol(
        id,
        SymbolKind::Const,
        name,
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(sig),
        None,
        body_text,
    ))
}
