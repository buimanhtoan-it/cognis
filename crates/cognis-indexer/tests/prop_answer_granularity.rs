//! Property-based test for answer-granularity: no whole-file blob symbols (Task 7.2).
//!
//! Feature: non-code-artifact-coverage, Property 5: Answer-granularity — no whole-file blob symbols
//!
//! Validates: Requirements 2.5, 3.4, 4.3, 5.3, 15.1, 15.2
//!
//! ## The property
//!
//! *For any* admitted artifact file, no emitted symbol has a line span equal to
//! the entire file, **except** the single explicit textual fallback symbol
//! permitted when structured extraction yields nothing.
//!
//! This is the contamination lever: a whole-file blob duplicates content that is
//! already covered by the granular units, inflating Contamination@k. The only
//! permitted whole-file span is the coarse textual fallback (`fell_back == true`)
//! emitted when structured extraction finds no answerable unit.
//!
//! ## How the line-span check is made precise
//!
//! Comparing `line_start == 1 && line_end == last_line` naively is *not* a valid
//! blob test, because several legitimate answer-granularity symbols can span the
//! whole file without being contamination:
//!
//! * **Tiny files.** On a 1-line file every symbol trivially spans line `[1, 1]`
//!   (e.g. `CREATE TABLE t (id INT);` emits a table *and* a column, both on line
//!   1). A whole-file span is only distinguishable from a unit span once the file
//!   has at least two lines, so the blob check only applies when `last_line >= 2`.
//! * **Single-unit files.** A file with exactly one answerable unit (one YAML
//!   leaf, one Markdown section, one route) legitimately has that single
//!   structured symbol span the file. That is answer-granularity by definition,
//!   not a blob, so the check only bites when the extractor emits **more than
//!   one** symbol.
//! * **Structural containers.** The SQL extractor emits a table `Class` whose
//!   span, per Req 3.5, is its `CREATE TABLE (...)` declaration span. For a file
//!   holding a single table that declaration *is* the whole file, while its
//!   columns (`Var`) are the granular sub-units. The container legitimately spans
//!   the file; the granular leaves must not. So `SymbolKind::Class` is excluded
//!   from the granular-leaf check. Crucially, the forbidden whole-file textual
//!   **fallback** blob is a `Module` (never a `Class`), so this exclusion never
//!   hides the real contamination bug.
//!
//! The resulting invariants (asserted for every [`ArtifactKind`] on the same
//! arbitrary source) are:
//!
//! * **B (structured answer-granularity):** when structured extraction succeeded
//!   (`!fell_back`) and produced more than one symbol on a `>= 2`-line file, no
//!   granular (non-`Class`) symbol spans the whole file `[1, last_line]`.
//! * **C (permitted fallback):** when the extractor fell back (`fell_back`), it
//!   emits at most one symbol — the single explicit whole-file textual fallback.
//!
//! The assertions are never weakened to pass: any structured extractor that
//! emits a whole-file-spanning granular symbol on a genuine multi-unit input is a
//! real answer-granularity (contamination) bug and must be reported as such.

use cognis_core::SymbolKind;
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Every supported artifact kind. The property must hold across all of them for
/// the same arbitrary input, so each generated case loops over this set. Feeding
/// a (say) YAML-shaped document to the SQL extractor simply exercises that
/// extractor's fallback path, which the property must also satisfy.
const ALL_KINDS: [ArtifactKind; 5] = [
    ArtifactKind::Yaml,
    ArtifactKind::Toml,
    ArtifactKind::Sql,
    ArtifactKind::Html,
    ArtifactKind::Markdown,
];

// ---------------------------------------------------------------------------
// Structured, multi-unit generators.
//
// These deliberately assemble MULTI-LINE documents with MULTIPLE answerable
// units so the structured path (`!fell_back`) is taken and invariant B (len > 1,
// last_line >= 2) is exercised with force, per the task's guidance to generate
// multi-key YAML, multi-column/multi-table SQL, multi-heading Markdown, and
// multi-route HTML.
// ---------------------------------------------------------------------------

/// Multi-key YAML/TOML-ish document: several leaf keys, each on its own line,
/// with occasional nesting so leaves land on distinct lines.
fn multi_key_yaml() -> impl Strategy<Value = String> {
    (2usize..12, any::<bool>()).prop_map(|(n, nested)| {
        let mut lines = Vec::new();
        for i in 0..n {
            if nested && i.is_multiple_of(3) {
                lines.push(format!("group{i}:"));
                lines.push(format!("  key{i}: value{i}"));
            } else {
                lines.push(format!("key{i}: value{i}"));
            }
        }
        lines.join("\n")
    })
}

/// Multi-table / multi-column SQL DDL: several `CREATE TABLE` statements each
/// spanning several lines, so no single table spans the whole file and every
/// column lands on its own line.
fn multi_table_sql() -> impl Strategy<Value = String> {
    (1usize..4, 2usize..6).prop_map(|(tables, cols)| {
        let mut out = String::new();
        for t in 0..tables {
            out.push_str(&format!("CREATE TABLE table_{t} (\n"));
            for c in 0..cols {
                let comma = if c + 1 < cols { "," } else { "" };
                out.push_str(&format!("  column_{c} INTEGER{comma}\n"));
            }
            out.push_str(");\n");
        }
        out
    })
}

/// Multi-heading Markdown: several ATX headings each with a body line, so the
/// sections partition the file into disjoint sub-ranges.
fn multi_heading_md() -> impl Strategy<Value = String> {
    (2usize..8, any::<bool>()).prop_map(|(n, preamble)| {
        let mut lines = Vec::new();
        if preamble {
            lines.push("preamble prose line".to_string());
        }
        for i in 0..n {
            let hashes = "#".repeat((i % 6) + 1);
            lines.push(format!("{hashes} Heading {i}"));
            lines.push(format!("body text for section {i}"));
        }
        lines.join("\n")
    })
}

/// Multi-route / multi-function HTML: several `/`-prefixed literals and named JS
/// functions, each on its own line.
fn multi_route_html() -> impl Strategy<Value = String> {
    (2usize..6, 1usize..5).prop_map(|(routes, funcs)| {
        let mut lines = vec!["<html>".to_string(), "<body>".to_string()];
        for r in 0..routes {
            lines.push(format!("<a href=\"/route/{r}\">link {r}</a>"));
        }
        lines.push("<script>".to_string());
        for f in 0..funcs {
            lines.push(format!("function handler_{f}() {{ return {f}; }}"));
        }
        lines.push("</script>".to_string());
        lines.push("</body>".to_string());
        lines.push("</html>".to_string());
        lines.join("\n")
    })
}

// ---------------------------------------------------------------------------
// Arbitrary / messy generator (robustness fuzz), mirroring Property 4's driver
// so the invariant is also stressed on empty, degenerate, unicode, control, and
// half-formed inputs that exercise every extractor's fallback boundary.
// ---------------------------------------------------------------------------

const FRAGMENTS: &[&str] = &[
    "key:",
    "  nested: value",
    "- item",
    "a.b.c: 1",
    "[section]",
    "k = \"v\"",
    "arr = [1, 2, 3]",
    ":",
    "   ",
    "\t- x",
    "root:\n  child:\n    leaf: 0",
    "CREATE TABLE",
    "CREATE TABLE t (",
    "id INT,",
    "name TEXT",
    ");",
    "SELECT * FROM t;",
    "CREATE TABLE (",
    ",",
    "()",
    "<a href=\"/api/x\">",
    "<div>",
    "</div>",
    "function foo() {}",
    "const bar = function() {}",
    "fetch('/world/state')",
    "<script>",
    "</script>",
    "onclick=\"/go\"",
    "/",
    "//",
    "/a/b/c",
    "# Heading",
    "## Sub",
    "###### Deep",
    "body text",
    "> quote",
    "- bullet",
    "```",
    "#",
    "#no-space",
    "\n\n",
    "\u{0}",
    "\u{7}",
    "\u{1b}[0m",
    "日本語: 値",
    "emoji 🚀 key: 🔥",
    "\r\n",
    "   \t  ",
    "\"unterminated",
    "'",
    "\\",
];

fn messy_line() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => prop::sample::select(FRAGMENTS).prop_map(|s| s.to_string()),
        3 => "(?s).{0,40}",
        1 => "[\\x00-\\x1f\\u{80}-\\u{ffff}]{0,16}",
    ]
}

fn messy_source() -> impl Strategy<Value = String> {
    prop_oneof![
        20 => prop::collection::vec(messy_line(), 0..24).prop_map(|l| l.join("\n")),
        6 => "(?s).{0,300}",
        1 => Just(String::new()),
        1 => Just("   \n\t\n  ".to_string()),
        1 => (1usize..12000).prop_map(|n| "x\n".repeat(n)),
    ]
}

/// The overall source generator: heavily biased toward structured multi-unit
/// documents (which force `!fell_back` with `len > 1`, exercising invariant B),
/// with arbitrary/messy inputs mixed in for robustness.
fn source_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => multi_key_yaml(),
        3 => multi_table_sql(),
        3 => multi_heading_md(),
        3 => multi_route_html(),
        4 => messy_source(),
    ]
}

proptest! {
    // Minimum 100 iterations; one test, looping over all kinds internally.
    #![proptest_config(ProptestConfig::with_cases(300))]

    // Feature: non-code-artifact-coverage, Property 5: Answer-granularity — no whole-file blob symbols
    #[test]
    fn no_whole_file_blob_among_structured_symbols(source in source_strategy()) {
        // Same line-count basis the extractors use for the fallback span.
        let last_line = source.lines().count().max(1) as u32;

        for kind in ALL_KINDS {
            let out = extract_artifact(kind, "artifact/input.dat", &source);
            let n = out.symbols.len();

            if out.fell_back {
                // Invariant C: the only permitted whole-file span is the single
                // explicit textual fallback — at most one symbol total.
                prop_assert!(
                    n <= 1,
                    "fallback for {kind:?} emitted {n} symbols (expected <= 1 whole-file fallback)\n\
                     source(len={}): {:?}",
                    source.len(),
                    source.chars().take(200).collect::<String>(),
                );
                continue;
            }

            // Structured extraction succeeded (`!fell_back`).
            //
            // The whole-file span is only distinguishable from a unit span once
            // the file has >= 2 lines, and a single answerable unit may legitimately
            // span the file, so the blob check only bites for multi-line,
            // multi-symbol structured output.
            if last_line < 2 || n <= 1 {
                continue;
            }

            // Invariant B: no granular (non-container) symbol spans the whole
            // file. `SymbolKind::Class` (a SQL table container) is excluded
            // because its declaration span may legitimately equal a single-table
            // file; the forbidden textual-fallback blob is always a `Module`, so
            // this exclusion never hides the contamination bug.
            for sym in &out.symbols {
                if sym.kind == SymbolKind::Class {
                    continue;
                }
                let spans_whole_file = sym.line_start == 1 && sym.line_end == last_line;
                prop_assert!(
                    !spans_whole_file,
                    "answer-granularity violated: structured {kind:?} extraction emitted a \
                     whole-file blob among {n} symbols\n\
                     kind={:?} name={:?} span=[{}, {}] last_line={} fell_back={}\n\
                     source(len={}): {:?}",
                    sym.kind,
                    sym.name,
                    sym.line_start,
                    sym.line_end,
                    last_line,
                    out.fell_back,
                    source.len(),
                    source.chars().take(200).collect::<String>(),
                );
            }
        }
    }
}
