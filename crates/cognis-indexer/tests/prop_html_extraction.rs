//! Property-based test for HTML route / JS function extraction (Task 5.2).
//!
//! Feature: non-code-artifact-coverage, Property 10: HTML route and function extraction
//!
//! Validates: Requirements 4.1, 4.2, 4.5
//!
//! ## The property
//!
//! *For any* HTML/embedded-JS document, the extractor emits one
//! `SymbolKind::Route` per **distinct** route literal beginning with `/`
//! (name = the exact route string, searchable text contains the route string)
//! and one `SymbolKind::Function` per named JS function (name = the identifier,
//! searchable text contains the function name).
//!
//! ## How it is driven
//!
//! Rather than generate raw HTML text and hope to scan it back, the test
//! generates a **known model** — a set of route strings and a set of named JS
//! function declarations — and renders it into a valid HTML document. Because the
//! model is known up front, the exact expected distinct-route set and
//! function-name set are computable, so the assertions are exact (not a
//! re-implementation of the scanner).
//!
//! To keep the oracle exact, every route string and function name is made
//! **globally unique by construction** by folding the generation index into a
//! suffix: route `i` renders as `/{stem}/r{i}` and function `i` renders as
//! `{stem}_f{i}`. Uniqueness guarantees each emitted symbol maps to exactly one
//! model element, so the distinct-route dedupe and the per-function emission are
//! both checked exactly.
//!
//! Generated content is constrained to the **well-supported subset** the
//! extractor documents (`crates/cognis-indexer/src/parser/artifact/html.rs`):
//!
//! - Routes use a safe charset — they begin with `/` and otherwise contain only
//!   `[a-z0-9_/]`, so no quote, backslash, or whitespace can terminate the
//!   literal early or split the value. Each route is placed either in an HTML
//!   attribute value (`href="…"`) or in an embedded-JS `fetch('…')` call, the two
//!   documented route positions.
//! - Functions are plain named declarations (`function name() { }`) in a
//!   `<script>` block — the canonical named-function form.

use std::collections::BTreeSet;

use cognis_core::{Symbol, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Fixed repo-relative path fed to the extractor.
const FILE: &str = "web/index.html";

/// A simple identifier stem: lowercase letters only, so with the appended
/// unique suffix it can never collide with a JS keyword.
fn ident_stem() -> impl Strategy<Value = String> {
    "[a-z]{1,5}"
}

/// One route blueprint: a lowercase stem plus a placement choice
/// (`true` = HTML attribute value, `false` = embedded-JS `fetch()` call).
fn route_bp() -> impl Strategy<Value = (String, bool)> {
    (ident_stem(), any::<bool>())
}

/// Render the known model to a valid HTML document.
///
/// `route_strs[i]` is placed in an attribute value when `placements[i]` is true,
/// otherwise in a `<script>` `fetch()` call. Every `func_names` entry is rendered
/// as a named function declaration in the same `<script>` block.
fn render(route_strs: &[String], placements: &[bool], func_names: &[String]) -> String {
    let mut html = String::from("<!DOCTYPE html>\n<html>\n<body>\n");

    // Attribute-value routes live in the document body.
    for (i, (route, in_attr)) in route_strs.iter().zip(placements).enumerate() {
        if *in_attr {
            html.push_str(&format!("<a href=\"{route}\">link{i}</a>\n"));
        }
    }

    html.push_str("</body>\n<script>\n");

    // Named function declarations, one per model function.
    for name in func_names {
        html.push_str(&format!("function {name}() {{ }}\n"));
    }

    // Embedded-JS routes live in fetch() calls.
    for (route, in_attr) in route_strs.iter().zip(placements) {
        if !*in_attr {
            html.push_str(&format!("fetch('{route}');\n"));
        }
    }

    html.push_str("</script>\n</html>\n");
    html
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 10.
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 10: HTML route and function extraction
    #[test]
    fn html_maps_routes_and_named_functions(
        routes in prop::collection::vec(route_bp(), 1..6),
        funcs in prop::collection::vec(ident_stem(), 1..6),
    ) {
        // Build the globally-unique known model. Folding the index into the
        // suffix guarantees uniqueness even when two stems collide.
        let route_strs: Vec<String> = routes
            .iter()
            .enumerate()
            .map(|(i, (stem, _))| format!("/{stem}/r{i}"))
            .collect();
        let placements: Vec<bool> = routes.iter().map(|(_, in_attr)| *in_attr).collect();
        let func_names: Vec<String> = funcs
            .iter()
            .enumerate()
            .map(|(i, stem)| format!("{stem}_f{i}"))
            .collect();

        let expected_routes: BTreeSet<&str> = route_strs.iter().map(String::as_str).collect();
        let expected_funcs: BTreeSet<&str> = func_names.iter().map(String::as_str).collect();
        // Unique by construction, so the model sets are the full lists.
        prop_assert_eq!(expected_routes.len(), route_strs.len());
        prop_assert_eq!(expected_funcs.len(), func_names.len());

        let src = render(&route_strs, &placements, &func_names);
        let out = extract_artifact(ArtifactKind::Html, FILE, &src);

        // Routes + functions are present, so the file is not routed to the
        // whole-file textual fallback.
        prop_assert!(!out.fell_back, "document with routes/functions must not fall back:\n{src}");

        let route_syms: Vec<&Symbol> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Route)
            .collect();
        let func_syms: Vec<&Symbol> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();

        // --- One Route per distinct route literal (Req 4.1). ---
        prop_assert_eq!(
            route_syms.len(),
            expected_routes.len(),
            "expected exactly one Route per distinct route literal:\n{}",
            src
        );
        let route_names: BTreeSet<&str> = route_syms.iter().map(|s| s.name.as_str()).collect();
        prop_assert_eq!(
            route_names,
            expected_routes.clone(),
            "Route symbol names must be exactly the distinct route strings"
        );

        // Each Route symbol's name is the exact route string and its searchable
        // text contains that route string (Req 4.1, 4.5).
        for route in &route_strs {
            let sym = route_syms
                .iter()
                .find(|s| &s.name == route)
                .expect("a Route symbol for each route string");
            let text = sym.body_excerpt.as_deref().unwrap_or("");
            prop_assert!(
                text.contains(route.as_str()),
                "Route text {text:?} must contain route string {route:?}"
            );
        }

        // --- One Function per named JS function (Req 4.2). ---
        prop_assert_eq!(
            func_syms.len(),
            expected_funcs.len(),
            "expected exactly one Function per named JS function:\n{}",
            src
        );
        let fn_names: BTreeSet<&str> = func_syms.iter().map(|s| s.name.as_str()).collect();
        prop_assert_eq!(
            fn_names,
            expected_funcs.clone(),
            "Function symbol names must be exactly the declared function identifiers"
        );

        // Each Function symbol's name is the exact identifier and its searchable
        // text contains that identifier (Req 4.2, 4.5).
        for name in &func_names {
            let sym = func_syms
                .iter()
                .find(|s| &s.name == name)
                .expect("a Function symbol for each declaration");
            let text = sym.body_excerpt.as_deref().unwrap_or("");
            prop_assert!(
                text.contains(name.as_str()),
                "Function text {text:?} must contain function name {name:?}"
            );
        }

        // Every emitted symbol is valid (line_end >= line_start >= 1, etc.).
        for s in &out.symbols {
            s.validate().expect("emitted artifact symbol must be valid");
        }
    }
}
