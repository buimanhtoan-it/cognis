//! Property-based test for RoutesTo exact matching (Task 9.5).
//!
//! Feature: non-code-artifact-coverage, Property 12: RoutesTo edges match routes to handlers exactly
//!
//! Validates: Requirements 6.1, 6.2, 6.5, 6.6
//!
//! ## The property
//!
//! *For any* set of Route symbols and code handler symbols, the
//! `RoutesToResolver` emits **exactly one** `EdgeKind::RoutesTo` edge
//! (Route → handler) for each code handler whose declared route string is
//! byte-for-byte, case-sensitive equal to a Route symbol's non-empty route
//! string, and emits **no** edge when there is no such match or when the route
//! string is empty or whitespace-only.
//!
//! ## How it is driven
//!
//! Rather than generate raw source and hope to re-scan it, the test generates a
//! **known model** — a list of route entries, each pairing one `Route` symbol
//! with a chosen number of code handler symbols and a variant that fixes whether
//! those handlers are supposed to match. Because the model is known up front, the
//! exact expected `(route_id → handler_id)` edge set is computable, so the
//! assertions are exact rather than a re-implementation of the resolver.
//!
//! Identifiers are made globally unique by folding the entry index `i` (and the
//! handler index `j`) into every route string and symbol id, so each emitted edge
//! maps to exactly one model element and no route string of one entry can be
//! declared by a handler of another. Route strings use a safe charset
//! (`/{stem}/r{i}`, stem `[a-z]{1,4}`) so no quote, backslash, or whitespace can
//! terminate a handler's quoted literal early.
//!
//! The five variants exercise every clause of Property 12:
//!
//! - **Matched**: route name = the route string, `n` handlers declare it
//!   verbatim → exactly `n` edges, one per handler (Req 6.1, 6.5).
//! - **NoHandler**: route name = the route string, but its handlers declare a
//!   *different* literal → no edge (Req 6.2).
//! - **CaseMismatch**: route name is the UPPERCASED route string while its
//!   handlers declare the lowercase form → no edge (case-sensitive, no
//!   normalization; Req 6.1).
//! - **Empty**: route name is empty; handlers declare the (real) route string →
//!   no edge (Req 6.6).
//! - **Whitespace**: route name is whitespace-only; handlers declare the (real)
//!   route string → no edge (Req 6.6).

use std::collections::BTreeSet;

use cognis_core::{EdgeKind, Symbol, SymbolKind};
use cognis_indexer::resolver::RoutesToResolver;
use proptest::prelude::*;

/// Which flavour of route entry to build — see the module docs for the exact
/// clause each one exercises.
#[derive(Debug, Clone)]
enum Variant {
    /// Route name == route string; handlers declare it verbatim → match.
    Matched,
    /// Route name == route string; handlers declare a different literal → miss.
    NoHandler,
    /// Route name == UPPERCASED route string; handlers declare lowercase → miss.
    CaseMismatch,
    /// Route name empty; handlers declare the real route string → miss.
    Empty,
    /// Route name whitespace-only; handlers declare the real route string → miss.
    Whitespace(String),
}

/// One generated route entry: an identifier stem, its match variant, and how
/// many handler symbols to attach.
#[derive(Debug, Clone)]
struct EntryBp {
    stem: String,
    variant: Variant,
    handler_count: usize,
}

/// A code handler `Symbol` (non-artifact language) whose body declares `literal`
/// as a `/`-prefixed quoted route string, exactly as a real Go/JS handler would.
fn handler(id: &str, name: &str, literal: &str) -> Symbol {
    let body = format!("http.HandleFunc(\"{literal}\", {name})\n");
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: format!("go:{name}"),
        language: "go".to_string(),
        module: "server".to_string(),
        file_path: "server.go".to_string(),
        line_start: 1,
        line_end: 3,
        signature: None,
        docstring: None,
        content_hash: "h".to_string(),
        body_excerpt: Some(body),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// An HTML `Route` symbol whose route string (`name`) is exactly `route_name`.
/// Tagged `language == "html"` so the resolver never treats it as a handler.
fn route(id: &str, route_name: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Route,
        name: route_name.to_string(),
        qualified_name: format!("html:{id}"),
        language: "html".to_string(),
        module: "web/index".to_string(),
        file_path: "web/index.html".to_string(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: "h".to_string(),
        // The route source text has no `/`-prefixed literal, so even if the
        // resolver did not skip artifact languages this could not self-register
        // as a handler.
        body_excerpt: Some(String::new()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

fn variant() -> impl Strategy<Value = Variant> {
    prop_oneof![
        Just(Variant::Matched),
        Just(Variant::NoHandler),
        Just(Variant::CaseMismatch),
        Just(Variant::Empty),
        prop::sample::select(vec![
            "   ".to_string(),
            "\t".to_string(),
            "\n \t".to_string(),
        ])
        .prop_map(Variant::Whitespace),
    ]
}

fn entry() -> impl Strategy<Value = EntryBp> {
    ("[a-z]{1,4}", variant(), 0usize..4).prop_map(|(stem, variant, handler_count)| EntryBp {
        stem,
        variant,
        handler_count,
    })
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 12.
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 12: RoutesTo edges match routes to handlers exactly
    #[test]
    fn routes_to_matches_handlers_exactly(
        entries in prop::collection::vec(entry(), 1..6),
    ) {
        let mut symbols: Vec<Symbol> = Vec::new();
        // Expected edges as (src_id = route id, dst_id = handler id) pairs.
        let mut expected: BTreeSet<(String, String)> = BTreeSet::new();

        for (i, e) in entries.iter().enumerate() {
            // Globally unique route string for this entry (safe charset).
            let route_str = format!("/{}/r{}", e.stem, i);

            // The Route symbol's route string depends on the variant.
            let route_name = match &e.variant {
                Variant::Matched | Variant::NoHandler => route_str.clone(),
                Variant::CaseMismatch => route_str.to_uppercase(),
                Variant::Empty => String::new(),
                Variant::Whitespace(ws) => ws.clone(),
            };
            let route_id = format!("html:web/index.html:route{i}@{}", i);
            symbols.push(route(&route_id, &route_name));

            for j in 0..e.handler_count {
                let handler_id = format!("go:server{i}.go:h{i}_{j}");
                let handler_name = format!("h{i}_{j}");
                // The literal each handler declares. Only `Matched` declares the
                // route symbol's exact (non-empty, case-identical) name.
                let literal = match &e.variant {
                    // A distinct, non-matching literal per handler.
                    Variant::NoHandler => format!("/no/match{i}_{j}"),
                    // Every other variant declares the real (lowercase) route
                    // string; the *route symbol* is what fails to match.
                    _ => route_str.clone(),
                };
                symbols.push(handler(&handler_id, &handler_name, &literal));

                // Only the Matched variant is expected to yield an edge.
                if matches!(e.variant, Variant::Matched) {
                    expected.insert((route_id.clone(), handler_id));
                }
            }
        }

        let edges = RoutesToResolver.resolve(&symbols);

        // Every emitted edge is a well-formed RoutesTo edge at confidence 1.0,
        // directed Route(src) → handler(dst) (Req 6.1/6.4).
        for edge in &edges {
            prop_assert_eq!(edge.kind, EdgeKind::RoutesTo, "only RoutesTo edges expected");
            prop_assert_eq!(edge.confidence, 1.0, "RoutesTo confidence must be exactly 1.0");
            prop_assert!(!edge.ambiguous, "exact route match is never ambiguous");
        }

        // The emitted edge set equals the expected set exactly: one edge per
        // matching handler (Req 6.1, 6.5), and none otherwise — no match
        // (Req 6.2), case-different (Req 6.1), or empty/whitespace route
        // (Req 6.6) all contribute zero.
        let actual: BTreeSet<(String, String)> = edges
            .iter()
            .map(|e| (e.src_id.clone(), e.dst_id.clone()))
            .collect();
        prop_assert_eq!(
            &actual,
            &expected,
            "RoutesTo edge set must match the known model exactly"
        );

        // No duplicate edges (dedup by (src, dst, kind) holds).
        prop_assert_eq!(edges.len(), expected.len(), "no duplicate edges expected");
    }
}
