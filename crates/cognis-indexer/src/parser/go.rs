//! Go extractor — mirror of `parsers/go.py`.
//!
//! Covers `function_declaration`, `method_declaration` (qualified
//! `ReceiverType.method`), and `type_declaration` → `type_spec` for structs
//! (`kind="class"`) and interfaces (`kind="interface"`).

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
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            "function_declaration" => {
                if let Some(s) = handle_function(&ctx, node) {
                    out.push(s);
                }
            }
            "method_declaration" => {
                if let Some(s) = handle_method(&ctx, node) {
                    out.push(s);
                }
            }
            "type_declaration" => handle_type_declaration(&ctx, node, &mut out),
            _ => {}
        }
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

fn extract_signature(node: Node, src: &[u8]) -> String {
    let full = node_text(node, src);
    match full.find('{') {
        Some(idx) => full[..idx].trim().chars().take(256).collect(),
        None => full.chars().take(256).collect(),
    }
}

fn extract_docstring(node: Node, src: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let siblings: Vec<Node> = parent.children(&mut cursor).collect();
    let idx = siblings.iter().position(|c| *c == node)?;
    let mut doc_lines: Vec<String> = Vec::new();
    let mut j = idx as isize - 1;
    while j >= 0 && matches!(siblings[j as usize].kind(), "comment" | "\n") {
        let sib = siblings[j as usize];
        if sib.kind() == "comment" {
            let text = node_text(sib, src).trim().to_string();
            if let Some(rest) = text.strip_prefix("//") {
                doc_lines.insert(0, rest.trim().to_string());
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

fn receiver_type(method: Node, src: &[u8]) -> Option<String> {
    let receiver = method
        .child_by_field_name("receiver")
        .or_else(|| find_child(method, &["parameter_list"]))?;
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() != "parameter_declaration" {
            continue;
        }
        let type_node = child.child_by_field_name("type").or_else(|| {
            let mut c = child.walk();
            let found = child
                .children(&mut c)
                .find(|n| matches!(n.kind(), "type_identifier" | "pointer_type"));
            found
        });
        let Some(type_node) = type_node else { continue };
        match type_node.kind() {
            "pointer_type" => {
                if let Some(inner) = find_child(type_node, &["type_identifier"]) {
                    return Some(node_text(inner, src).to_string());
                }
            }
            "type_identifier" => return Some(node_text(type_node, src).to_string()),
            _ => {}
        }
    }
    None
}

fn handle_function(ctx: &Ctx, node: Node) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| find_child(node, &["identifier"]))?;
    let name = node_text(name_node, ctx.src).to_string();
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

fn handle_method(ctx: &Ctx, node: Node) -> Option<Symbol> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| find_child(node, &["field_identifier", "identifier"]))?;
    let method_name = node_text(name_node, ctx.src).to_string();
    let recv = receiver_type(node, ctx.src);
    let qual = match &recv {
        Some(r) => format!("{r}.{method_name}"),
        None => method_name.clone(),
    };
    let body_text = node_text(node, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, qual);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &qual, body_text);
    Some(mk_symbol(
        id,
        SymbolKind::Method,
        method_name,
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

fn handle_type_declaration(ctx: &Ctx, node: Node, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_spec" {
            if let Some(s) = handle_type_spec(ctx, child, node) {
                out.push(s);
            }
        }
    }
}

fn handle_type_spec(ctx: &Ctx, spec: Node, decl: Node) -> Option<Symbol> {
    let name_node = spec
        .child_by_field_name("name")
        .or_else(|| find_child(spec, &["type_identifier"]))?;
    let name = node_text(name_node, ctx.src).to_string();

    let mut kind = SymbolKind::Class;
    let type_val = spec.child_by_field_name("type").or_else(|| {
        // Find the type node after the name.
        let mut cursor = spec.walk();
        let mut found_name = false;
        let mut result = None;
        for child in spec.children(&mut cursor) {
            if child == name_node {
                found_name = true;
                continue;
            }
            if found_name && !matches!(child.kind(), "=" | " ") {
                result = Some(child);
                break;
            }
        }
        result
    });
    if let Some(t) = type_val {
        if t.kind() == "interface_type" {
            kind = SymbolKind::Interface;
        }
    }

    let body_text = node_text(decl, ctx.src);
    let qualified_name = format!("{}:{}:{}", ctx.lang, ctx.file_path, name);
    let id = make_symbol_id(ctx.lang, ctx.file_path, &name, body_text);
    Some(mk_symbol(
        id,
        kind,
        name.clone(),
        qualified_name,
        ctx.label,
        ctx.module,
        ctx.file_path,
        line_start(decl),
        line_end(decl),
        Some(format!("type {name}")),
        extract_docstring(decl, ctx.src),
        body_text,
    ))
}
