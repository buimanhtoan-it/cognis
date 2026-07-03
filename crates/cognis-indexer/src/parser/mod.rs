//! Parser stage of the indexer pipeline (Task 8.1).
//!
//! Extracts [`Symbol`]s from source via tree-sitter for TypeScript/JavaScript,
//! Python, Go, C#, and Java (Requirement 9.1). The stage is **fault-tolerant
//! per file**: if a language is unsupported, the grammar fails to produce a
//! tree, or structured extraction yields nothing for non-empty source, the file
//! falls back to a single textual `module` symbol so the batch is never aborted
//! (Requirement 9.4).
//!
//! Symbol ids follow `<lang>:<file_path>:<qualified_name>@<short_hash>` and
//! field shapes mirror the Python parsers so the Rust indexer round-trips the
//! same UCKG rows (Requirement 9.2 parity).

mod csharp;
mod generic;
mod go;
mod java;
mod nested;
mod normalize;
mod python;
mod support;
mod typescript;

pub use normalize::{content_hash, make_symbol_id, normalize_body};

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use generic::GenericSpec;
use support::{body_excerpt, module_from_path};

/// Rust: functions/structs/enums/traits/modules; `impl` blocks scope methods.
static RUST_SPEC: GenericSpec = GenericSpec {
    defs: &[
        ("function_item", SymbolKind::Function),
        ("struct_item", SymbolKind::Class),
        ("enum_item", SymbolKind::Class),
        ("union_item", SymbolKind::Class),
        ("trait_item", SymbolKind::Interface),
        ("mod_item", SymbolKind::Module),
    ],
    scopes: &["impl_item"],
    line_comment: "//",
};

/// C: functions (name via declarator chain) + struct/enum/union types.
static C_SPEC: GenericSpec = GenericSpec {
    defs: &[
        ("function_definition", SymbolKind::Function),
        ("struct_specifier", SymbolKind::Class),
        ("enum_specifier", SymbolKind::Class),
        ("union_specifier", SymbolKind::Class),
    ],
    scopes: &[],
    line_comment: "//",
};

/// C++: C plus classes and namespaces (which scope their members).
static CPP_SPEC: GenericSpec = GenericSpec {
    defs: &[
        ("function_definition", SymbolKind::Function),
        ("class_specifier", SymbolKind::Class),
        ("struct_specifier", SymbolKind::Class),
        ("enum_specifier", SymbolKind::Class),
        ("namespace_definition", SymbolKind::Module),
    ],
    scopes: &[],
    line_comment: "//",
};

/// Ruby: methods (incl. singleton), classes, modules.
static RUBY_SPEC: GenericSpec = GenericSpec {
    defs: &[
        ("method", SymbolKind::Method),
        ("singleton_method", SymbolKind::Method),
        ("class", SymbolKind::Class),
        ("module", SymbolKind::Module),
    ],
    scopes: &[],
    line_comment: "#",
};

/// PHP: functions, methods, classes, interfaces, traits.
static PHP_SPEC: GenericSpec = GenericSpec {
    defs: &[
        ("function_definition", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("trait_declaration", SymbolKind::Class),
    ],
    scopes: &[],
    line_comment: "//",
};

/// A resolved language target: which tree-sitter grammar to load plus the
/// id-prefix (`lang`) and `Symbol::language` label the Python parsers used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// Lower-case language id (`Symbol::language`), e.g. `"typescript"`.
    pub label: &'static str,
    /// Short id prefix used in symbol ids, e.g. `"ts"`, `"py"`, `"go"`.
    pub lang: &'static str,
    /// Which grammar variant to load (TS vs TSX matters for JSX files).
    grammar: Grammar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grammar {
    Typescript,
    Tsx,
    Python,
    Go,
    CSharp,
    Java,
    Rust,
    C,
    Cpp,
    Ruby,
    Php,
}

/// Map a file path's extension to a [`Language`], or `None` if unsupported.
///
/// JavaScript (`.js`/`.jsx`/`.mjs`/`.cjs`) is parsed with the TypeScript grammar
/// (a syntactic superset of JS) — satisfying the TS/JS coverage in Req 9.1.
pub fn language_for_path(path: &str) -> Option<Language> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let l = match ext {
        "ts" | "mts" | "cts" => Language {
            label: "typescript",
            lang: "ts",
            grammar: Grammar::Typescript,
        },
        "tsx" => Language {
            label: "typescript",
            lang: "ts",
            grammar: Grammar::Tsx,
        },
        "js" | "mjs" | "cjs" => Language {
            label: "javascript",
            lang: "js",
            grammar: Grammar::Typescript,
        },
        "jsx" => Language {
            label: "javascript",
            lang: "js",
            grammar: Grammar::Tsx,
        },
        "py" | "pyi" => Language {
            label: "python",
            lang: "py",
            grammar: Grammar::Python,
        },
        "go" => Language {
            label: "go",
            lang: "go",
            grammar: Grammar::Go,
        },
        "cs" => Language {
            label: "csharp",
            lang: "cs",
            grammar: Grammar::CSharp,
        },
        "java" => Language {
            label: "java",
            lang: "java",
            grammar: Grammar::Java,
        },
        "rs" => Language {
            label: "rust",
            lang: "rs",
            grammar: Grammar::Rust,
        },
        "c" | "h" => Language {
            label: "c",
            lang: "c",
            grammar: Grammar::C,
        },
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => Language {
            label: "cpp",
            lang: "cpp",
            grammar: Grammar::Cpp,
        },
        "rb" => Language {
            label: "ruby",
            lang: "rb",
            grammar: Grammar::Ruby,
        },
        "php" | "phtml" => Language {
            label: "php",
            lang: "php",
            grammar: Grammar::Php,
        },
        _ => return None,
    };
    Some(l)
}

impl Language {
    fn ts_language(&self) -> tree_sitter::Language {
        match self.grammar {
            Grammar::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::Go => tree_sitter_go::LANGUAGE.into(),
            Grammar::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Grammar::Java => tree_sitter_java::LANGUAGE.into(),
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
            Grammar::C => tree_sitter_c::LANGUAGE.into(),
            Grammar::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Grammar::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Grammar::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }
}

/// Outcome of parsing one file. The parser never returns an error: failures are
/// expressed as a textual-fallback symbol with a [`ParseStatus`].
#[derive(Debug, Clone)]
pub struct ParseOutput {
    /// Extracted (or fallback) symbols. Empty only for empty/whitespace source.
    pub symbols: Vec<Symbol>,
    /// Per-file parse outcome (`ok` / `partial` / `failed`).
    pub status: ParseStatus,
    /// Detected language label, or `None` for unsupported extensions.
    pub language: Option<&'static str>,
    /// True when the result came from the textual fallback path.
    pub fell_back: bool,
}

/// Parse `source` for `file_path` into symbols. Fault-tolerant: never panics,
/// never aborts; on any failure produces a textual fallback (Requirement 9.4).
///
/// `file_path` should be repo-relative with forward slashes.
pub fn parse_source(file_path: &str, source: &str) -> ParseOutput {
    let Some(language) = language_for_path(file_path) else {
        // Unsupported extension → textual fallback so the file still indexes.
        return textual_fallback(file_path, source, "txt", "text", None);
    };
    let detected = Some(language.label);

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language.ts_language()).is_err() {
        return textual_fallback(file_path, source, language.lang, language.label, detected);
    }

    let Some(tree) = parser.parse(source.as_bytes(), None) else {
        // Grammar could not produce a tree at all → textual fallback.
        return textual_fallback(file_path, source, language.lang, language.label, detected);
    };

    let src = source.as_bytes();
    let module = module_from_path(file_path);
    let root = tree.root_node();

    let symbols = match language.grammar {
        Grammar::Typescript | Grammar::Tsx => {
            typescript::extract(root, src, file_path, &module, language.lang, language.label)
        }
        Grammar::Python => {
            python::extract(root, src, file_path, &module, language.lang, language.label)
        }
        Grammar::Go => go::extract(root, src, file_path, &module, language.lang, language.label),
        Grammar::CSharp => {
            csharp::extract(root, src, file_path, &module, language.lang, language.label)
        }
        Grammar::Java => {
            java::extract(root, src, file_path, &module, language.lang, language.label)
        }
        Grammar::Rust => generic::extract(
            root,
            src,
            file_path,
            &module,
            language.lang,
            language.label,
            &RUST_SPEC,
        ),
        Grammar::C => generic::extract(
            root,
            src,
            file_path,
            &module,
            language.lang,
            language.label,
            &C_SPEC,
        ),
        Grammar::Cpp => generic::extract(
            root,
            src,
            file_path,
            &module,
            language.lang,
            language.label,
            &CPP_SPEC,
        ),
        Grammar::Ruby => generic::extract(
            root,
            src,
            file_path,
            &module,
            language.lang,
            language.label,
            &RUBY_SPEC,
        ),
        Grammar::Php => generic::extract(
            root,
            src,
            file_path,
            &module,
            language.lang,
            language.label,
            &PHP_SPEC,
        ),
    };

    if symbols.is_empty() {
        if source.trim().is_empty() {
            // Empty file: nothing to index, but not a failure.
            return ParseOutput {
                symbols: Vec::new(),
                status: ParseStatus::Ok,
                language: Some(language.label),
                fell_back: false,
            };
        }
        // Non-empty source produced no structured symbols. If the tree has
        // errors the parse genuinely failed → textual fallback; otherwise it is
        // a "partial" parse (recovered but nothing of interest), matching the
        // Python pipeline's `parse_status="partial"`.
        if root.has_error() {
            return textual_fallback(file_path, source, language.lang, language.label, detected);
        }
        return ParseOutput {
            symbols: Vec::new(),
            status: ParseStatus::Partial,
            language: Some(language.label),
            fell_back: false,
        };
    }

    // Structured symbols extracted. A recovered-but-errored tree is still
    // surfaced as `partial` for visibility (the writer records it on `file`).
    let status = if root.has_error() {
        ParseStatus::Partial
    } else {
        ParseStatus::Ok
    };
    ParseOutput {
        symbols,
        status,
        language: Some(language.label),
        fell_back: false,
    }
}

/// Build a single coarse `module` symbol spanning the whole file. Used when
/// structured parsing is impossible, so the file remains searchable and the
/// batch continues (Requirement 9.4).
fn textual_fallback(
    file_path: &str,
    source: &str,
    lang: &str,
    label: &str,
    detected: Option<&'static str>,
) -> ParseOutput {
    if source.trim().is_empty() {
        return ParseOutput {
            symbols: Vec::new(),
            status: ParseStatus::Failed,
            language: detected,
            fell_back: true,
        };
    }
    let module = module_from_path(file_path);
    let name = module.rsplit('/').next().unwrap_or(&module).to_string();
    let line_count = source.lines().count().max(1) as u32;
    let qualified_name = format!("{lang}:{file_path}:{name}");
    let symbol = Symbol {
        id: make_symbol_id(lang, file_path, &name, source),
        kind: SymbolKind::Module,
        name,
        qualified_name,
        language: label.to_string(),
        module,
        file_path: file_path.to_string(),
        line_start: 1,
        line_end: line_count,
        signature: None,
        docstring: None,
        content_hash: content_hash(source),
        body_excerpt: Some(body_excerpt(source)),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    };
    ParseOutput {
        symbols: vec![symbol],
        status: ParseStatus::Failed,
        language: detected,
        fell_back: true,
    }
}

/// Shared symbol constructor used by the per-language extractors. Fills the
/// parser-stage fields; resolver/enricher/writer (Task 8.2) own the rest
/// (`updated_at`, `risk_score`, `semantic_summary`, …).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mk_symbol(
    id: String,
    kind: SymbolKind,
    name: String,
    qualified_name: String,
    language: &str,
    module: &str,
    file_path: &str,
    line_start: u32,
    line_end: u32,
    signature: Option<String>,
    docstring: Option<String>,
    body_text: &str,
) -> Symbol {
    Symbol {
        id,
        kind,
        name,
        qualified_name,
        language: language.to_string(),
        module: module.to_string(),
        file_path: file_path.to_string(),
        line_start,
        line_end,
        signature,
        docstring,
        content_hash: content_hash(body_text),
        body_excerpt: Some(body_excerpt(body_text)),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

#[cfg(test)]
mod generic_lang_tests {
    use super::parse_source;

    fn names(file: &str, src: &str) -> Vec<String> {
        let out = parse_source(file, src);
        out.symbols.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn rust_functions_structs_traits_impls() {
        let src = r#"
fn free_fn() {}
struct Point { x: i32 }
impl Point { fn dist(&self) -> i32 { 0 } }
trait Shape { fn area(&self); }
"#;
        let n = names("src/geo.rs", src);
        for want in ["free_fn", "Point", "dist", "Shape"] {
            assert!(n.contains(&want.to_string()), "rust missing {want}: {n:?}");
        }
    }

    #[test]
    fn c_functions_and_structs() {
        let src = "int add(int a, int b) { return a + b; }\nstruct Node { int v; };\n";
        let n = names("src/util.c", src);
        assert!(n.contains(&"add".to_string()), "c missing add: {n:?}");
        assert!(n.contains(&"Node".to_string()), "c missing Node: {n:?}");
    }

    #[test]
    fn cpp_classes_methods_functions() {
        let src = "class Widget { public: void render() {} };\nint main() { return 0; }\n";
        let n = names("src/app.cpp", src);
        for want in ["Widget", "render", "main"] {
            assert!(n.contains(&want.to_string()), "cpp missing {want}: {n:?}");
        }
    }

    #[test]
    fn ruby_classes_and_methods() {
        let src = "class Greeter\n  def hello\n    puts 'hi'\n  end\nend\n\ndef top\nend\n";
        let n = names("lib/greeter.rb", src);
        for want in ["Greeter", "hello", "top"] {
            assert!(n.contains(&want.to_string()), "ruby missing {want}: {n:?}");
        }
    }

    #[test]
    fn php_classes_methods_functions() {
        let src = "<?php\nclass Foo {\n  public function bar() {}\n}\nfunction baz() {}\n";
        let n = names("src/Foo.php", src);
        for want in ["Foo", "bar", "baz"] {
            assert!(n.contains(&want.to_string()), "php missing {want}: {n:?}");
        }
    }

    #[test]
    fn cpp_method_is_qualified_by_class() {
        let src = "class A { public: void run() {} };\n";
        let out = parse_source("a.cpp", src);
        let run = out
            .symbols
            .iter()
            .find(|s| s.name == "run")
            .expect("run method");
        assert!(
            run.qualified_name.contains("A.run"),
            "expected A.run qualified name, got {}",
            run.qualified_name
        );
    }
}
