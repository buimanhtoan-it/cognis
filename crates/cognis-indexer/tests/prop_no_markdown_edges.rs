//! Property-based test that no integration edge touches a Markdown heading
//! symbol (Task 9.10).
//!
//! Feature: non-code-artifact-coverage, Property 17: No integration edge touches a Markdown heading symbol
//!
//! Validates: Requirements 9.1, 9.2
//!
//! ## The property
//!
//! *For any* resolved edge set, the number of `RoutesTo`/`Reads`/`Writes`/`Tests`
//! edges incident (as source or target) to any Markdown-emitted
//! `SymbolKind::Module` heading-section symbol (including a Markdown textual
//! fallback) is exactly `0`.
//!
//! ## How it is driven — a triple-bait mixed batch
//!
//! A blanket "suppress every edge incident to markdown" is trivially satisfiable
//! by a resolver that emits *no* integration edges at all. To make the exclusion
//! meaningful, the generator builds a batch that **mixes** genuine Markdown
//! heading sections with exactly the ingredients that make integration edges:
//!
//! - **RoutesTo bait.** Real HTML `Route` symbols (via `extract_artifact`) whose
//!   route strings are also declared by real code handler symbols.
//! - **config `Reads` bait.** Real YAML config-key `Var` symbols whose key
//!   literals are also referenced by real code reader symbols.
//! - **SQL `Reads`/`Writes` bait.** Real SQL table `Class` symbols whose names are
//!   also referenced (with a `SELECT` / `INSERT` verb) by real code query sites.
//!
//! Crucially, the Markdown section bodies are crafted to **contain every one of
//! those route literals, config-key literals, and SQL table names + verbs**, so a
//! naive resolver that did not exclude Markdown `Module` symbols from candidacy
//! *would* treat each Markdown section as a handler / config reader / query site
//! and manufacture edges incident to it.
//!
//! The test then asserts two things at once, so the exclusion is verified to be
//! *targeted*, not a blanket edge suppression:
//!
//! 1. **Exclusion (the property):** no `RoutesTo`/`Reads`/`Writes`/`Tests` edge
//!    has any Markdown symbol id as `src_id` or `dst_id`.
//! 2. **Non-vacuity:** the intended *non-Markdown* integration edges still form —
//!    each `Route → code handler` `RoutesTo`, each `code reader → config-key`
//!    `Reads`, each `code query site → SQL table` `Reads`/`Writes`.

use std::collections::{HashMap, HashSet};

use cognis_core::{EdgeKind, Symbol, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::{resolve_edges, ArtifactKind};
use proptest::prelude::*;

/// Integration edge kinds excluded from touching a Markdown heading symbol
/// (Req 9.1).
fn is_integration_edge(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::RoutesTo | EdgeKind::Reads | EdgeKind::Writes | EdgeKind::Tests
    )
}

/// Build a genuine code (non-artifact) `Symbol` with full control over the
/// searchable text the resolvers scan (`body_excerpt`). Tagged `language ==
/// "go"` so it is never treated as an artifact symbol.
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

/// A short lowercase stem; combined with a fold of the generation index it makes
/// every route / key / table name globally unique by construction.
fn stem() -> impl Strategy<Value = String> {
    "[a-z]{1,4}"
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 17.
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 17: No integration edge touches a Markdown heading symbol
    #[test]
    fn no_integration_edge_touches_markdown(
        route_stems in prop::collection::vec(stem(), 1..4),
        cfg_stems in prop::collection::vec(stem(), 1..4),
        tbl_stems in prop::collection::vec(stem(), 1..4),
    ) {
        // --- Build the globally-unique names (index fold guarantees uniqueness
        // even when two stems collide, and the per-category infix keeps the
        // three name-spaces disjoint). ---
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

        // Markdown heading section whose body CONTAINS every route literal,
        // config-key literal, and SQL table name + verb — the bait a naive
        // resolver would link to.
        let mut md = String::from("# Bait Section\n");
        for r in &routes {
            md.push_str(&format!("HandleFunc(\"{r}\", h)\n"));
        }
        for k in &cfg_keys {
            md.push_str(&format!("os.Getenv(\"{k}\")\n"));
        }
        for t in &read_tables {
            md.push_str(&format!("SELECT c FROM {t}\n"));
        }
        for t in &write_tables {
            md.push_str(&format!("INSERT INTO {t} VALUES (1)\n"));
        }
        let md_out = extract_artifact(ArtifactKind::Markdown, "docs/NOTES.md", &md);

        // Markdown must actually have produced at least one symbol, and its text
        // must actually carry the bait — otherwise the exclusion would be
        // vacuously satisfied.
        prop_assert!(!md_out.symbols.is_empty(), "markdown must emit at least one symbol");
        let md_ids: HashSet<String> = md_out.symbols.iter().map(|s| s.id.clone()).collect();
        for s in &md_out.symbols {
            prop_assert_eq!(
                s.kind,
                SymbolKind::Module,
                "every markdown symbol must be a Module (the excluded kind)"
            );
        }
        let md_text: String = md_out
            .symbols
            .iter()
            .filter_map(|s| s.body_excerpt.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for r in &routes {
            prop_assert!(md_text.contains(r.as_str()), "markdown bait must contain route {r:?}");
        }
        for k in &cfg_keys {
            prop_assert!(md_text.contains(k.as_str()), "markdown bait must contain key {k:?}");
        }
        for t in read_tables.iter().chain(write_tables.iter()) {
            prop_assert!(md_text.contains(t.as_str()), "markdown bait must contain table {t:?}");
        }

        // Index artifact symbols by name → id for the non-vacuity checks.
        let route_id: HashMap<&str, &str> = html_out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Route)
            .map(|s| (s.name.as_str(), s.id.as_str()))
            .collect();
        let cfg_id: HashMap<&str, &str> = yaml_out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .map(|s| (s.name.as_str(), s.id.as_str()))
            .collect();
        let table_id: HashMap<&str, &str> = sql_out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .map(|s| (s.name.as_str(), s.id.as_str()))
            .collect();

        // ================= Code symbols (genuine handlers/readers/sites) =======

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

        // ================= Merge the batch and resolve =========================

        let mut all: Vec<Symbol> = Vec::new();
        all.extend(md_out.symbols.iter().cloned());
        all.extend(html_out.symbols.iter().cloned());
        all.extend(yaml_out.symbols.iter().cloned());
        all.extend(sql_out.symbols.iter().cloned());
        all.extend(code.iter().cloned());

        let edges = resolve_edges(&all);

        // --- THE PROPERTY (Req 9.1/9.2): zero integration edges incident to any
        // Markdown symbol, as source or target. ---
        let incident = edges
            .iter()
            .filter(|e| is_integration_edge(e.kind))
            .filter(|e| md_ids.contains(&e.src_id) || md_ids.contains(&e.dst_id))
            .count();
        prop_assert_eq!(
            incident,
            0,
            "no RoutesTo/Reads/Writes/Tests edge may touch a markdown symbol; found {} such edge(s)\nmd_ids: {:?}\nedges: {:?}",
            incident,
            md_ids,
            edges
                .iter()
                .filter(|e| is_integration_edge(e.kind))
                .map(|e| (e.src_id.clone(), e.dst_id.clone(), e.kind))
                .collect::<Vec<_>>()
        );

        // --- NON-VACUITY: the intended non-markdown integration edges DO form,
        // so the exclusion is targeted rather than a blanket suppression. ---
        let has_edge = |src: &str, dst: &str, kind: EdgeKind| {
            edges
                .iter()
                .any(|e| e.src_id == src && e.dst_id == dst && e.kind == kind)
        };

        // Every Route → its code handler (RoutesTo).
        for (i, r) in routes.iter().enumerate() {
            let rid = *route_id.get(r.as_str()).expect("a Route symbol per route string");
            let hid = format!("go:code.go:handler{i}@h{i}");
            prop_assert!(
                has_edge(rid, &hid, EdgeKind::RoutesTo),
                "expected RoutesTo {rid} -> {hid} for route {r:?}"
            );
        }
        // Every code reader → its config key (Reads).
        for (i, k) in cfg_keys.iter().enumerate() {
            let kid = *cfg_id.get(k.as_str()).expect("a Var symbol per config key");
            let rid = format!("go:code.go:reader{i}@h{i}");
            prop_assert!(
                has_edge(&rid, kid, EdgeKind::Reads),
                "expected Reads {rid} -> {kid} for key {k:?}"
            );
        }
        // Every read query site → its SQL table (Reads).
        for (i, t) in read_tables.iter().enumerate() {
            let tid = *table_id.get(t.as_str()).expect("a Class symbol per read table");
            let sid = format!("go:code.go:readsite{i}@h{i}");
            prop_assert!(
                has_edge(&sid, tid, EdgeKind::Reads),
                "expected Reads {sid} -> {tid} for table {t:?}"
            );
        }
        // Every write query site → its SQL table (Writes).
        for (i, t) in write_tables.iter().enumerate() {
            let tid = *table_id.get(t.as_str()).expect("a Class symbol per write table");
            let sid = format!("go:code.go:writesite{i}@h{i}");
            prop_assert!(
                has_edge(&sid, tid, EdgeKind::Writes),
                "expected Writes {sid} -> {tid} for table {t:?}"
            );
        }
    }
}
