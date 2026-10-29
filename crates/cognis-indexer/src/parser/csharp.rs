//! C# extractor — mirror of `parsers/csharp.py` (via the shared nested walker).
//!
//! `class`/`struct`/`record`/`enum` → `kind="class"`, `interface` →
//! `kind="interface"`, `method`/`constructor` → `kind="method"`. Nested types
//! qualify as `Outer.Inner.method`.

use cognis_core::{Symbol, SymbolKind};
use tree_sitter::Node;

use super::nested::{self, NestedConfig};

const CFG: NestedConfig = NestedConfig {
    type_decls: &[
        ("class_declaration", SymbolKind::Class, "class"),
        ("struct_declaration", SymbolKind::Class, "struct"),
        ("record_declaration", SymbolKind::Class, "record"),
        (
            "record_struct_declaration",
            SymbolKind::Class,
            "record struct",
        ),
        ("interface_declaration", SymbolKind::Interface, "interface"),
        ("enum_declaration", SymbolKind::Class, "enum"),
    ],
    method_decls: &["method_declaration", "constructor_declaration"],
    comment_kinds: &["comment"],
    sig_delims: &["{", "=>", ";"],
    triple_slash: true,
};

pub(super) fn extract(
    root: Node,
    src: &[u8],
    file_path: &str,
    module: &str,
    lang: &str,
    label: &str,
) -> Vec<Symbol> {
    nested::extract(root, src, file_path, module, lang, label, &CFG)
}
