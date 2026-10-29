//! Property-based test for integration-edge validity and confidence ordering
//! (Task 9.9).
//!
//! Feature: non-code-artifact-coverage, Property 16: Integration edges validate with a strict confidence ordering
//!
//! Validates: Requirements 6.4, 7.6, 8.3
//!
//! ## The property
//!
//! For any emitted integration edge, [`Edge::validate`] passes (non-empty
//! endpoints, `confidence ∈ [0,1]`), every `RoutesTo` edge has confidence
//! `1.0`, and every SQL `Reads`/`Writes` edge has an identical fixed confidence
//! in `[0.50, 0.80]` that is strictly less than `1.0`.
//!
//! ## How it is driven — a mixed batch with edges of known provenance
//!
//! `EdgeKind::Reads` is ambiguous by kind alone: it can come from **either** the
//! config-reads resolver (confidence `0.7`) **or** the SQL-edge resolver
//! (confidence `0.65`). To assert the SQL-edge confidence band precisely, the
//! test never inspects `Reads` edges from the full `resolve_edges` output for the
//! band claim. Instead it establishes provenance by construction:
//!
//! 1. **Validity (Req 6.4 / 7.6 / 8.3 well-formedness).** Build one large mixed
//!    batch — HTML routes + code handlers, YAML config keys + code readers, SQL
//!    tables + code query sites, plus genuine code and Markdown sections — and
//!    assert every `Edge` in `to_edges(resolve_edges(batch))` passes
//!    `Edge::validate()` (non-empty endpoints, confidence in `[0,1]`). This
//!    covers *all* edge kinds, so the mixed-provenance `Reads` set is validated
//!    as a whole for well-formedness.
//!
//! 2. **RoutesTo confidence ceiling (Req 6.4).** Drive [`RoutesToResolver`] in
//!    isolation over the HTML+handler slice: every emitted edge is `RoutesTo`
//!    with confidence *exactly* `1.0`.
//!
//! 3. **SQL band + strict ordering (Req 8.3).** Drive [`SqlEdgeResolver`] in
//!    isolation over the SQL+query-site slice: every emitted `Reads`/`Writes`
//!    edge carries one identical fixed confidence that lies in `[0.50, 0.80]`
//!    and is strictly below the `RoutesTo` ceiling `1.0`. Because the resolver
//!    is driven alone over a slice whose only artifact symbols are SQL, every
//!    `Reads` here is provably a SQL edge.
//!
//! The batches are also checked for **non-vacuity** (each isolated resolver
//! actually emits at least one edge) so the confidence assertions are never
//! satisfied by an empty edge set.

use std::collections::HashSet;

use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::resolver::{RoutesToResolver, SqlEdgeResolver};
use cognis_indexer::{resolve_edges, to_edges, ArtifactKind};
use proptest::prelude::*;

/// The `RoutesTo` confidence ceiling: exact route-string identity, no
/// normalization (Req 6.4).
const CONF_ROUTES_TO: f64 = 1.0;
/// SQL-edge confidence band required by the design (Req 8.3): identical across
/// every SQL edge and strictly below the exact-match ceiling `1.0`.
const SQL_CONF_LO: f64 = 0.50;
const SQL_CONF_HI: f64 = 0.80;

/// Integration edge kinds (Req 6/7/8): the edges this property constrains.
fn is_integration_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::RoutesTo | EdgeKind::Reads | EdgeKind::Writes | EdgeKind::Tests
    )
}

/// A genuine code (non-artifact) `Symbol` whose `body_excerpt` is the text the
/// resolvers scan. Tagged `language == "go"` so it is never treated as an
/// artifact symbol.
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

/// A short lowercase stem; combined with the generation index it makes every
/// route / key / table name globally unique by construction.
fn stem() -> impl Strategy<Value = String> {
    "[a-z]{1,4}"
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 16.
    #![proptest_config(ProptestConfig::with_cases(128))]

    // Feature: non-code-artifact-coverage, Property 16: Integration edges validate with a strict confidence ordering
    #[test]
    fn integration_edges_validate_with_strict_confidence_ordering(
        route_stems in prop::collection::vec(stem(), 1..4),
        cfg_stems in prop::collection::vec(stem(), 1..4),
        tbl_stems in prop::collection::vec(stem(), 1..4),
    ) {
        // --- Globally-unique names (index fold + per-category infix keep the
        // four name-spaces disjoint even when two stems collide). ---
        let routes: Vec<String> = route_stems
            .iter()
            .enumerate()
            .map(|(i, s)| format!("/api/{s}r{i}"))
            .collect();
        let cfg_keys: Vec<String> = cfg_stems
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{s}cfg{i}"))
            .collect();
        let read_tables: Vec<String> = tbl_stems
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{s}rt{i}"))
            .collect();
        let write_tables: Vec<String> = tbl_stems
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{s}wt{i}"))
            .collect();

        // ================= Artifact symbols (real extractors) =================

        // HTML `Route` symbols.
        let mut html = String::from("<!DOCTYPE html>\n<html>\n<body>\n");
        for (i, r) in routes.iter().enumerate() {
            html.push_str(&format!("<a href=\"{r}\">l{i}</a>\n"));
        }
        html.push_str("</body>\n</html>\n");
        let html_out = extract_artifact(ArtifactKind::Html, "web/index.html", &html);

        // YAML config-key `Var` symbols (one leaf key per line).
        let mut yaml = String::new();
        for (i, k) in cfg_keys.iter().enumerate() {
            yaml.push_str(&format!("{k}: v{i}\n"));
        }
        let yaml_out = extract_artifact(ArtifactKind::Yaml, "config/app.yaml", &yaml);

        // SQL table `Class` symbols (a read table and a write table per stem).
        let mut sql = String::new();
        for t in read_tables.iter().chain(write_tables.iter()) {
            sql.push_str(&format!("CREATE TABLE {t} (col INT);\n"));
        }
        let sql_out = extract_artifact(ArtifactKind::Sql, "db/schema.sql", &sql);

        // Markdown heading sections — documentation content that must never be
        // an integration-edge endpoint but is part of the mixed batch.
        let md = String::from("# Overview\nsome prose about the service\n\n## Details\nmore prose\n");
        let md_out = extract_artifact(ArtifactKind::Markdown, "docs/NOTES.md", &md);

        // ================= Code symbols (handlers/readers/sites) ===============

        let mut code: Vec<Symbol> = Vec::new();
        // One handler per route, declaring the exact route literal.
        for (i, r) in routes.iter().enumerate() {
            code.push(code_symbol(
                &format!("go:code.go:handler{i}@h{i}"),
                &format!("handler{i}"),
                &format!("router.HandleFunc(\"{r}\", handler{i})"),
            ));
        }
        // One reader per config key, referencing the exact key literal.
        for (i, k) in cfg_keys.iter().enumerate() {
            code.push(code_symbol(
                &format!("go:code.go:reader{i}@h{i}"),
                &format!("reader{i}"),
                &format!("cfg := os.Getenv(\"{k}\")"),
            ));
        }
        // One read query site per read table.
        for (i, t) in read_tables.iter().enumerate() {
            code.push(code_symbol(
                &format!("go:code.go:readsite{i}@h{i}"),
                &format!("readsite{i}"),
                &format!("db.Query(\"SELECT c FROM {t}\")"),
            ));
        }
        // One write query site per write table.
        for (i, t) in write_tables.iter().enumerate() {
            code.push(code_symbol(
                &format!("go:code.go:writesite{i}@h{i}"),
                &format!("writesite{i}"),
                &format!("db.Exec(\"INSERT INTO {t} VALUES (1)\")"),
            ));
        }

        // ===================================================================
        // Sub-assertion 1: every edge from to_edges(resolve_edges(batch))
        // passes Edge::validate() — non-empty endpoints, confidence in [0,1]
        // (Req 6.4 / 7.6 / 8.3 well-formedness), over the full mixed batch.
        // ===================================================================
        let mut all: Vec<Symbol> = Vec::new();
        all.extend(md_out.symbols.iter().cloned());
        all.extend(html_out.symbols.iter().cloned());
        all.extend(yaml_out.symbols.iter().cloned());
        all.extend(sql_out.symbols.iter().cloned());
        all.extend(code.iter().cloned());

        let edges: Vec<Edge> = to_edges(&resolve_edges(&all));
        for e in &edges {
            prop_assert!(
                e.validate().is_ok(),
                "every emitted edge must validate: {e:?}"
            );
            // Restate the invariants explicitly so a validate() weakening cannot
            // silently pass this test.
            prop_assert!(!e.src_id.is_empty(), "edge src_id must be non-empty: {e:?}");
            prop_assert!(!e.dst_id.is_empty(), "edge dst_id must be non-empty: {e:?}");
            prop_assert!(
                (0.0..=1.0).contains(&e.confidence),
                "edge confidence {} must be in [0,1]",
                e.confidence
            );
        }
        // Non-vacuity: at least one integration edge exists in the mixed batch.
        prop_assert!(
            edges.iter().any(|e| is_integration_edge(e.kind)),
            "mixed batch must produce at least one integration edge"
        );

        // ===================================================================
        // Sub-assertion 2: RoutesToResolver alone → every edge is RoutesTo at
        // confidence exactly 1.0 (Req 6.4).
        // ===================================================================
        let mut routes_slice: Vec<Symbol> = Vec::new();
        routes_slice.extend(html_out.symbols.iter().cloned());
        for (i, r) in routes.iter().enumerate() {
            routes_slice.push(code_symbol(
                &format!("go:code.go:handler{i}@h{i}"),
                &format!("handler{i}"),
                &format!("router.HandleFunc(\"{r}\", handler{i})"),
            ));
        }
        let routes_to = RoutesToResolver.resolve(&routes_slice);
        prop_assert!(
            !routes_to.is_empty(),
            "RoutesTo slice must produce at least one edge (non-vacuity)"
        );
        for e in &routes_to {
            prop_assert_eq!(e.kind, EdgeKind::RoutesTo, "RoutesToResolver emits only RoutesTo");
            prop_assert_eq!(
                e.confidence,
                CONF_ROUTES_TO,
                "every RoutesTo edge must have confidence exactly 1.0"
            );
        }

        // ===================================================================
        // Sub-assertion 3: SqlEdgeResolver alone → every Reads/Writes edge has
        // one identical fixed confidence in [0.50, 0.80], strictly < 1.0
        // (Req 8.3). Provenance is guaranteed: the only artifact symbols in this
        // slice are SQL tables, so every emitted Reads edge is a SQL edge.
        // ===================================================================
        let mut sql_slice: Vec<Symbol> = Vec::new();
        sql_slice.extend(sql_out.symbols.iter().cloned());
        for (i, t) in read_tables.iter().enumerate() {
            sql_slice.push(code_symbol(
                &format!("go:code.go:readsite{i}@h{i}"),
                &format!("readsite{i}"),
                &format!("db.Query(\"SELECT c FROM {t}\")"),
            ));
        }
        for (i, t) in write_tables.iter().enumerate() {
            sql_slice.push(code_symbol(
                &format!("go:code.go:writesite{i}@h{i}"),
                &format!("writesite{i}"),
                &format!("db.Exec(\"INSERT INTO {t} VALUES (1)\")"),
            ));
        }
        let sql_edges = SqlEdgeResolver.resolve(&sql_slice);
        prop_assert!(
            !sql_edges.is_empty(),
            "SQL slice must produce at least one edge (non-vacuity)"
        );
        // A single, shared confidence value across every SQL edge (Req 8.3).
        let c0 = sql_edges[0].confidence;
        let kinds_seen: HashSet<EdgeKind> = sql_edges.iter().map(|e| e.kind).collect();
        prop_assert!(
            kinds_seen.iter().all(|k| matches!(k, EdgeKind::Reads | EdgeKind::Writes)),
            "SqlEdgeResolver emits only Reads/Writes, saw {kinds_seen:?}"
        );
        for e in &sql_edges {
            prop_assert_eq!(
                e.confidence,
                c0,
                "SQL edge confidence must be identical across every edge"
            );
            prop_assert!(
                (SQL_CONF_LO..=SQL_CONF_HI).contains(&e.confidence),
                "SQL edge confidence {} must be in [{}, {}]",
                e.confidence,
                SQL_CONF_LO,
                SQL_CONF_HI
            );
            prop_assert!(
                e.confidence < CONF_ROUTES_TO,
                "SQL edge confidence {} must be strictly below the RoutesTo ceiling {}",
                e.confidence,
                CONF_ROUTES_TO
            );
        }
    }
}
