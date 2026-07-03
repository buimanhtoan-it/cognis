//! TypeScript/JavaScript extractor — mirror of `parsers/typescript.py`.
//!
//! Covers `function_declaration`, arrow/function-expression `const` declarations,
//! `class_declaration` (+ methods), `interface_declaration`, ALL_CAPS consts,
//! and `export` / `export default` wrappers. The TS grammar is a superset of JS,
//! so `.js`/`.jsx` files route here too (Req 9.1).

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
    let ctx = Ctx {
        src,
        file_path,
        module,
        lang,
        label,
    };
    let mut out = Vec::new();
    walk(&ctx, root, &mut out);
    out
}

struct Ctx<'a> {
    src: &'a [u8],
    file_path: &'a str,
    module: &'a str,
    lang: &'a str,
    label: &'a str,
}

fn walk(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(s) = handle_function(ctx, node) {
                out.push(s);
            }
            return;
        }
        "class_declaration" | "abstract_class_declaration" => {
            handle_class(ctx, node, out);
            return;
        }
        "interface_declaration" => {
            if let Some(s) = handle_interface(ctx, node) {
                out.push(s);
            }
            return;
        }
        "variable_declaration" | "lexical_declaration" => {
            handle_variable_declaration(ctx, node, out);
            return;
        }
        "export_statement" => {
            handle_export(ctx, node, out);
            return;
        }
        "export_default_declaration" => {
            handle_export_default(ctx, node, out);
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(ctx, child, out);
    }
}

fn handle_export(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(s) = handle_function(ctx, child) {
                    out.push(s);
                }
            }
            "class_declaration" | "abstract_class_declaration" => handle_class(ctx, child, out),
            "interface_declaration" => {
                if let Some(s) = handle_interface(ctx, child) {
                    out.push(s);
                }
            }
            "variable_declaration" | "lexical_declaration" => {
                handle_variable_declaration(ctx, child, out)
            }
            _ => {}
        }
    }
}

fn handle_export_default(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(s) = handle_function(ctx, child) {
                    out.push(s);
                }
            }
            "class_declaration" | "abstract_class_declaration" => handle_class(ctx, child, out),
            _ => {}
        }
    }
}

fn get_identifier<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    find_child(
        node,
        &["identifier", "type_identifier", "property_identifier"],
    )
    .map(|n| node_text(n, src))
}

fn extract_signature(node: Node, src: &[u8]) -> String {
    let full = node_text(node, src);
    for delim in ["{", "=>", ";"] {
        if let Some(idx) = full.find(delim) {
            let mut sig = full[..idx].trim().to_string();
            if delim == "=>" {
                sig.push_str(" =>");
            }
            return sig.chars().take(256).collect();
        }
    }
    full.chars().take(256).collect()
}

fn extract_docstring(node: Node, src: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let siblings: Vec<Node> = parent.children(&mut cursor).collect();
    let idx = siblings.iter().position(|c| *c == node)?;
    if idx == 0 {
        return None;
    }
    let prev = siblings[idx - 1];
    if prev.kind() != "comment" {
        return None;
    }
    let text = node_text(prev, src).trim().to_string();
    if text.starts_with("/**") || text.starts_with("/*") {
        let body = if let Some(stripped) = text.strip_prefix("/**") {
            stripped
        } else {
            text.strip_prefix("/*").unwrap_or(&text)
        };
        let body = body.trim_end_matches("*/").trim();
        let lines: Vec<String> = body
            .lines()
            .map(|l| l.trim_start_matches([' ', '*']).trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        return Some(lines.join("\n"));
    }
    if let Some(rest) = text.strip_prefix("//") {
        return Some(rest.trim().to_string());
    }
    None
}

fn is_all_caps(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    name == name.to_uppercase() && name.chars().any(|c| c.is_alphabetic())
}

fn handle_function(ctx: &Ctx, node: Node) -> Option<Symbol> {
    let name = get_identifier(node, ctx.src)?.to_string();
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
    Some(mk_symbol(
        id,
        SymbolKind::Function,
        name,
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(extract_signature(node, ctx.src)),
        extract_docstring(node, ctx.src),
        body_text,
    ))
}

fn handle_class(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let Some(name) = get_identifier(node, ctx.src).map(str::to_string) else {
        return;
    };
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
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
        extract_docstring(node, ctx.src),
        body_text,
    ));

    if let Some(class_body) = find_child(node, &["class_body"]) {
        let mut cursor = class_body.walk();
        for m in class_body.children(&mut cursor) {
            if m.kind() == "method_definition" {
                if let Some(s) = handle_method(ctx, m, &name) {
                    out.push(s);
                }
            }
        }
    }
}

fn handle_method(ctx: &Ctx, node: Node, class_name: &str) -> Option<Symbol> {
    let name = get_identifier(node, ctx.src)?.to_string();
    let body_text = node_text(node, ctx.src);
    let qual = format!("{class_name}.{name}");
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, qual);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &qual, body_text);
    Some(mk_symbol(
        id,
        SymbolKind::Method,
        name,
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(extract_signature(node, ctx.src)),
        extract_docstring(node, ctx.src),
        body_text,
    ))
}

fn handle_interface(ctx: &Ctx, node: Node) -> Option<Symbol> {
    let name = get_identifier(node, ctx.src)?.to_string();
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
    Some(mk_symbol(
        id,
        SymbolKind::Interface,
        name.clone(),
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(node),
        line_end(node),
        Some(format!("interface {name}")),
        extract_docstring(node, ctx.src),
        body_text,
    ))
}

fn handle_variable_declaration(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let mut top = node.walk();
    let is_const_kw = node.children(&mut top).any(|c| c.kind() == "const");

    let mut cursor = node.walk();
    for declarator in node.children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = find_child(declarator, &["identifier"]) else {
            continue;
        };
        let name = node_text(name_node, ctx.src).to_string();

        let mut dcur = declarator.walk();
        let value = declarator.children(&mut dcur).find(|c| {
            matches!(
                c.kind(),
                "arrow_function" | "function_expression" | "function" | "generator_function"
            )
        });

        let body_text = node_text(node, ctx.src);
        let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
        let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);

        if value.is_some() {
            out.push(mk_symbol(
                id,
                SymbolKind::Function,
                name,
                qualified_name,
                ctx.label,
                ctx.module,
                ctx.file_path,
                line_start(node),
                line_end(node),
                Some(extract_signature(declarator, ctx.src)),
                extract_docstring(node, ctx.src),
                body_text,
            ));
        } else if is_const_kw && is_all_caps(&name) {
            let sig: String = body_text.chars().take(256).collect();
            out.push(mk_symbol(
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
                extract_docstring(node, ctx.src),
                body_text,
            ));
        }
    }
}
