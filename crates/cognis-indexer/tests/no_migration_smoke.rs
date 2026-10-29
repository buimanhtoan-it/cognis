//! "Population, not migration" smoke test (Task 13.3).
//!
//! Feature: non-code-artifact-coverage — no-migration smoke + constant fixity.
//!
//! Validates:
//! - Requirement 6.3: `RoutesTo` edges use the existing `EdgeKind::RoutesTo`
//!   value with no schema migration.
//! - Requirement 7.5: config `Reads` edges use the existing `EdgeKind::Reads`
//!   value with no schema migration.
//! - Requirement 8.4: SQL `Reads`/`Writes` edges use the existing
//!   `EdgeKind::Reads` / `EdgeKind::Writes` values with no schema migration.
//!
//! ## What this asserts (and what it deliberately does not)
//!
//! This is the *population-only* guarantee: the artifact extractors and the
//! integration-edge resolvers only ever emit **pre-existing** `SymbolKind` /
//! `EdgeKind` variants, and every emitted symbol/edge passes
//! `Symbol::validate` / `Edge::validate`. Concretely:
//!
//! - Every symbol produced by [`extract_artifact`] carries a kind in the
//!   pre-existing set `{Var, Const, Class, Function, Route, Module}` and
//!   validates.
//! - Every edge produced by [`resolve_edges`] carries a kind in the
//!   pre-existing `EdgeKind` set, the integration edges it emits are confined to
//!   `{RoutesTo, Reads, Writes}`, and every edge validates.
//!
//! These assertions are intentionally about *variant membership and validity*
//! (the no-migration invariant), kept non-overlapping with the property tests
//! that already pin exact edge sets (9.5), the fan-out cap (9.6), normalized
//! matching (9.7), and the confidence band/ordering (9.9). It is an example
//! test, not a property test.

use std::collections::HashSet;

use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::{resolve_edges, to_edges, ArtifactKind};

/// The pre-existing `SymbolKind` variants an artifact extractor may emit — the
/// YAML/SQL/HTML/Markdown extractors and their textual fallback. No new variant
/// is introduced by this feature (population, not migration).
fn is_preexisting_symbol_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Var
            | SymbolKind::Const
            | SymbolKind::Class
            | SymbolKind::Function
            | SymbolKind::Route
            | SymbolKind::Module
    )
}

/// The full pre-existing `EdgeKind` set declared by the schema before this
/// feature. Every resolver output must be a member — no new variant.
fn is_preexisting_edge_kind(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::Imports
            | EdgeKind::Inherits
            | EdgeKind::Implements
            | EdgeKind::Reads
            | EdgeKind::Writes
            | EdgeKind::RoutesTo
            | EdgeKind::Tests
    )
}

/// The integration edges this feature populates (Req 6/7/8).
fn is_integration_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::RoutesTo | EdgeKind::Reads | EdgeKind::Writes
    )
}

/// A genuine code (non-artifact) `Symbol` whose `body_excerpt` is the text the
/// integration resolvers scan. Tagged `language == "go"` so it is never treated
/// as an artifact symbol.
fn code_symbol(id: &str, name: &str, body: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "go".to_string(),
        module: "code".to_string(),
        file_path: "code.go".to_string(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: format!("h_{id}"),
        body_excerpt: Some(body.to_string()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// Build one small mixed batch: HTML routes + handlers, YAML config keys +
/// readers, SQL tables + read/write query sites, plus a Markdown section. The
/// batch is crafted so each integration resolver emits at least one edge.
fn mixed_batch() -> (Vec<Symbol>, Vec<Symbol>) {
    // ---- Artifact symbols, from the real extractors ----
    let html = "<!DOCTYPE html>\n<html>\n<body>\n\
                <a href=\"/api/world\">world</a>\n\
                <script>function submitCode() { fetch('/api/submit'); }</script>\n\
                </body>\n</html>\n";
    let html_out = extract_artifact(ArtifactKind::Html, "web/index.html", html);

    let yaml = "port: 8080\ndb:\n  host: localhost\n";
    let yaml_out = extract_artifact(ArtifactKind::Yaml, "config/app.yaml", yaml);

    let sql = "CREATE TABLE users (id INT, name TEXT);\n\
               CREATE TABLE audit_log (id INT);\n";
    let sql_out = extract_artifact(ArtifactKind::Sql, "db/schema.sql", sql);

    let md = "# Overview\nsome prose\n\n## Security\nmore prose\n";
    let md_out = extract_artifact(ArtifactKind::Markdown, "docs/NOTES.md", md);

    let mut artifacts: Vec<Symbol> = Vec::new();
    artifacts.extend(html_out.symbols);
    artifacts.extend(yaml_out.symbols);
    artifacts.extend(sql_out.symbols);
    artifacts.extend(md_out.symbols);

    // ---- Code symbols that the integration resolvers join against ----
    let code = vec![
        // Handlers declaring the exact route literals → RoutesTo (Req 6).
        code_symbol(
            "go:code.go:worldHandler@1",
            "worldHandler",
            "router.HandleFunc(\"/api/world\", worldHandler)",
        ),
        code_symbol(
            "go:code.go:submitHandler@2",
            "submitHandler",
            "router.HandleFunc(\"/api/submit\", submitHandler)",
        ),
        // Reader referencing the exact config key literal → config Reads (Req 7).
        code_symbol(
            "go:code.go:loadPort@3",
            "loadPort",
            "p := os.Getenv(\"port\")",
        ),
        // Read query site → SQL Reads (Req 8).
        code_symbol(
            "go:code.go:listUsers@4",
            "listUsers",
            "db.Query(\"SELECT id, name FROM users\")",
        ),
        // Write query site → SQL Writes (Req 8). `audit_log` normalizes to match
        // an `AuditLog` code identifier as well; the literal table name suffices.
        code_symbol(
            "go:code.go:writeAudit@5",
            "writeAudit",
            "db.Exec(\"INSERT INTO audit_log VALUES (1)\")",
        ),
    ];

    (artifacts, code)
}

/// Every symbol an artifact extractor emits uses a pre-existing `SymbolKind`
/// and passes `Symbol::validate` (no schema migration — Req 6.3/7.5/8.4 rest on
/// this population-only guarantee for the symbol side).
#[test]
fn artifact_symbols_use_only_preexisting_kinds_and_validate() {
    let (artifacts, _code) = mixed_batch();
    assert!(
        !artifacts.is_empty(),
        "the crafted batch must produce artifact symbols (non-vacuity)"
    );
    for sym in &artifacts {
        assert!(
            is_preexisting_symbol_kind(sym.kind),
            "artifact symbol emitted a non-pre-existing SymbolKind {:?}: {}",
            sym.kind,
            sym.qualified_name
        );
        sym.validate()
            .unwrap_or_else(|e| panic!("artifact symbol must validate ({e}): {sym:?}"));
    }
}

/// Every edge the resolver stage emits uses a pre-existing `EdgeKind`, the
/// integration edges are confined to `{RoutesTo, Reads, Writes}`, and every
/// edge passes `Edge::validate` (Req 6.3, 7.5, 8.4 — population, not migration).
#[test]
fn integration_edges_use_only_preexisting_kinds_and_validate() {
    let (artifacts, code) = mixed_batch();
    let mut batch: Vec<Symbol> = Vec::new();
    batch.extend(artifacts);
    batch.extend(code);

    let edges: Vec<Edge> = to_edges(&resolve_edges(&batch));

    // Non-vacuity: the batch must actually produce integration edges, otherwise
    // the kind/validity assertions below would be vacuously true.
    assert!(
        edges.iter().any(|e| is_integration_edge(e.kind)),
        "the crafted batch must emit at least one integration edge"
    );

    let mut integration_kinds: HashSet<EdgeKind> = HashSet::new();
    for e in &edges {
        // No new variant: every edge kind is one the schema already declared.
        assert!(
            is_preexisting_edge_kind(e.kind),
            "resolver emitted a non-pre-existing EdgeKind {:?}",
            e.kind
        );
        // Every emitted edge is well-formed.
        e.validate()
            .unwrap_or_else(|err| panic!("edge must validate ({err}): {e:?}"));

        if is_integration_edge(e.kind) {
            integration_kinds.insert(e.kind);
        }
    }

    // The integration edges emitted are exactly within the pre-existing
    // {RoutesTo, Reads, Writes} set (Req 6.3/7.5/8.4).
    for k in &integration_kinds {
        assert!(
            matches!(k, EdgeKind::RoutesTo | EdgeKind::Reads | EdgeKind::Writes),
            "integration edge kind {k:?} is outside the pre-existing set"
        );
    }

    // All three integration edge kinds are populated by the crafted batch, so
    // each existing variant is exercised (RoutesTo from Req 6, Reads from Req
    // 7/8, Writes from Req 8).
    assert!(
        integration_kinds.contains(&EdgeKind::RoutesTo),
        "expected a RoutesTo edge (Req 6.3)"
    );
    assert!(
        integration_kinds.contains(&EdgeKind::Reads),
        "expected a Reads edge (Req 7.5 / 8.4)"
    );
    assert!(
        integration_kinds.contains(&EdgeKind::Writes),
        "expected a Writes edge (Req 8.4)"
    );
}
