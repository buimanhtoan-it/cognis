//! Property-based test for SQL table/column mapping (Task 4.2).
//!
//! Feature: non-code-artifact-coverage, Property 9: SQL table/column mapping
//!
//! Validates: Requirements 3.1, 3.2, 3.3
//!
//! ## The property
//!
//! *For any* SQL DDL, the extractor emits one `SymbolKind::Class` per declared
//! table (name = table name) and one `SymbolKind::Var` per declared column,
//! where each column symbol's searchable text contains both the owning table
//! name and the column name.
//!
//! ## How it is driven
//!
//! Rather than generate raw SQL text and hope to parse it back, the test
//! generates a **known model** — a set of tables, each with a set of columns
//! (simple identifier names + simple types) — and renders it into
//! `CREATE TABLE` statements. Because the model is known up front, the exact
//! expected table set and per-table column set are computable, so the assertions
//! are exact (not a re-implementation of the parser).
//!
//! To keep every emitted symbol unambiguously attributable, identifiers are made
//! globally unique by construction: table `i` is named `{stem}_t{i}` and column
//! `j` of table `i` is named `{stem}_c{i}_{j}`. The extractor derives a
//! deterministic `qualified_name` (`sql:{file}:{table}` for tables and
//! `sql:{file}:{table}.{column}` for columns), so each generated column maps to
//! exactly one expected `qualified_name` — the clean key used to associate a
//! `Var` symbol with its owning table even when two tables share a bare column
//! stem.
//!
//! Each rendered table also carries a random subset of **table-level
//! constraints** (`PRIMARY KEY`, `UNIQUE`, `FOREIGN KEY`, `CHECK`). These must
//! never be counted as columns, so asserting `Var` count == total declared
//! columns confirms the extractor skips them (Req 3.2 is about *declared
//! columns*, not constraint clauses).

use std::collections::BTreeSet;

use cognis_core::{Symbol, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Fixed repo-relative path fed to the extractor; the derived `qualified_name`
/// prefix depends on it, so the expected-key builder below must use the same
/// value.
const FILE: &str = "db/schema.sql";

/// Simple, well-supported column types (a couple carry parentheses to exercise
/// the depth-aware body splitter without introducing top-level commas).
const TYPES: &[&str] = &[
    "INT",
    "INTEGER",
    "TEXT",
    "BIGINT",
    "NUMERIC",
    "BOOLEAN",
    "REAL",
    "VARCHAR(255)",
];

/// One generated column blueprint: a lowercase stem plus a type pick.
#[derive(Debug, Clone)]
struct ColBp {
    stem: String,
    ty_pick: usize,
}

/// One generated table blueprint: a lowercase stem, its columns, an optional
/// `IF NOT EXISTS`, and a random subset of table-level constraints (which must
/// not be emitted as columns).
#[derive(Debug, Clone)]
struct TableBp {
    stem: String,
    cols: Vec<ColBp>,
    if_not_exists: bool,
    add_pk: bool,
    add_unique: bool,
    add_fk: bool,
    add_check: bool,
}

/// A simple identifier stem: lowercase letters only, so it can never collide
/// with a SQL keyword once the `_t{i}` / `_c{i}_{j}` suffix is appended.
fn ident_stem() -> impl Strategy<Value = String> {
    "[a-z]{1,5}"
}

fn col_bp() -> impl Strategy<Value = ColBp> {
    (ident_stem(), 0usize..TYPES.len()).prop_map(|(stem, ty_pick)| ColBp { stem, ty_pick })
}

fn table_bp() -> impl Strategy<Value = TableBp> {
    (
        ident_stem(),
        prop::collection::vec(col_bp(), 1..6),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(stem, cols, if_not_exists, add_pk, add_unique, add_fk, add_check)| TableBp {
                stem,
                cols,
                if_not_exists,
                add_pk,
                add_unique,
                add_fk,
                add_check,
            },
        )
}

/// Render the blueprint set to SQL DDL and return the paired known model as
/// `(table_name, [column_name, ...])`, with globally unique identifiers.
fn render(tables: &[TableBp]) -> (String, Vec<(String, Vec<String>)>) {
    let mut sql = String::new();
    let mut expected: Vec<(String, Vec<String>)> = Vec::new();

    for (i, t) in tables.iter().enumerate() {
        let tname = format!("{}_t{}", t.stem, i);

        let mut col_names: Vec<String> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        for (j, c) in t.cols.iter().enumerate() {
            let cname = format!("{}_c{}_{}", c.stem, i, j);
            lines.push(format!("  {} {}", cname, TYPES[c.ty_pick]));
            col_names.push(cname);
        }

        // Table-level constraints reference the first column. Each begins with a
        // constraint keyword the extractor is documented to skip, so none of
        // these should surface as a column `Var`.
        let first = &col_names[0];
        if t.add_pk {
            lines.push(format!("  PRIMARY KEY ({first})"));
        }
        if t.add_unique {
            lines.push(format!("  UNIQUE ({first})"));
        }
        if t.add_fk {
            lines.push(format!("  FOREIGN KEY ({first}) REFERENCES some_other(x)"));
        }
        if t.add_check {
            lines.push(format!("  CHECK ({first} IS NOT NULL)"));
        }

        let ine = if t.if_not_exists {
            "IF NOT EXISTS "
        } else {
            ""
        };
        sql.push_str(&format!(
            "CREATE TABLE {ine}{tname} (\n{}\n);\n\n",
            lines.join(",\n")
        ));

        expected.push((tname, col_names));
    }

    (sql, expected)
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 9.
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 9: SQL table/column mapping
    #[test]
    fn sql_maps_tables_to_classes_and_columns_to_vars(
        tables in prop::collection::vec(table_bp(), 1..5),
    ) {
        let (sql, expected) = render(&tables);

        let out = extract_artifact(ArtifactKind::Sql, FILE, &sql);

        // Structured DDL was recognized, not routed to the whole-file fallback.
        prop_assert!(!out.fell_back, "well-formed DDL must not fall back:\n{sql}");

        let classes: Vec<&Symbol> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        let vars: Vec<&Symbol> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .collect();

        // --- One Class per declared table, name = table name (Req 3.1). ---
        prop_assert_eq!(
            classes.len(),
            expected.len(),
            "expected exactly one Class per table"
        );
        let class_names: BTreeSet<&str> = classes.iter().map(|s| s.name.as_str()).collect();
        let expected_tables: BTreeSet<&str> =
            expected.iter().map(|(t, _)| t.as_str()).collect();
        prop_assert_eq!(
            class_names,
            expected_tables,
            "Class symbol names must be exactly the declared table names"
        );

        // Each table Class's searchable text includes the table name (Req 3.1).
        for (tname, _) in &expected {
            let class = classes
                .iter()
                .find(|s| &s.name == tname)
                .expect("a Class symbol for each table");
            let text = class.body_excerpt.as_deref().unwrap_or("");
            prop_assert!(
                text.contains(tname.as_str()),
                "table Class text {text:?} must contain table name {tname:?}"
            );
        }

        // --- One Var per declared column, and nothing else (Req 3.2). ---
        // Total column count matching the model confirms table-level constraint
        // clauses are NOT emitted as columns.
        let total_cols: usize = expected.iter().map(|(_, cs)| cs.len()).sum();
        prop_assert_eq!(
            vars.len(),
            total_cols,
            "expected exactly one Var per declared column (constraints must be skipped)"
        );

        // Each declared column maps to exactly one Var, correctly attributed to
        // its owning table, whose text contains BOTH the table and column name
        // (Req 3.2, 3.3).
        for (tname, cols) in &expected {
            for cname in cols {
                let expected_qn = format!("sql:{FILE}:{tname}.{cname}");
                let matches: Vec<&&Symbol> = vars
                    .iter()
                    .filter(|s| s.qualified_name == expected_qn)
                    .collect();
                prop_assert_eq!(
                    matches.len(),
                    1,
                    "column {}.{} must map to exactly one Var (qn {})",
                    tname,
                    cname,
                    expected_qn
                );
                let v = matches[0];
                prop_assert_eq!(
                    &v.name,
                    cname,
                    "column Var name must equal the column name"
                );
                let text = v.body_excerpt.as_deref().unwrap_or("");
                prop_assert!(
                    text.contains(tname.as_str()),
                    "column Var text {text:?} must contain owning table {tname:?}"
                );
                prop_assert!(
                    text.contains(cname.as_str()),
                    "column Var text {text:?} must contain column name {cname:?}"
                );
            }
        }

        // Every emitted symbol is valid (line_end >= line_start >= 1, etc.).
        for s in &out.symbols {
            s.validate().expect("emitted artifact symbol must be valid");
        }
    }
}
