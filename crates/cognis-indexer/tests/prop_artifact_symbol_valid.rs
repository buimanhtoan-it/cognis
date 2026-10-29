//! Property-based test that every emitted artifact symbol is valid (Task 7.1).
//!
//! Feature: non-code-artifact-coverage, Property 4: Every emitted artifact symbol is valid
//!
//! Validates: Requirements 2.6, 3.5, 4.1, 4.2, 5.4
//!
//! ## The property
//!
//! *For any* admitted artifact file of any supported type, every emitted
//! `Symbol` passes `Symbol::validate` — in particular
//! `line_end >= line_start >= 1` (also non-empty id and `risk_score ∈ [0,1]`).
//!
//! ## How it is driven
//!
//! This is a robustness / fuzz-style invariant, so the generator deliberately
//! produces **arbitrary, messy** source strings — empty, whitespace-only, random
//! unicode and control characters, partial/half-formed structures, deeply nested
//! shapes, and occasionally huge inputs — and feeds each one through the genuine
//! public extractor entry point (`extract_artifact`) for **every**
//! [`ArtifactKind`] variant. No matter the kind or the input, every symbol the
//! extractor emits must satisfy `Symbol::validate`. The assertion is never
//! weakened: any input that yields an invalid symbol is a real bug in the
//! extractor family.

use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Every supported artifact kind. The property must hold across all of them for
/// the same arbitrary input, so each generated case loops over this set.
const ALL_KINDS: [ArtifactKind; 5] = [
    ArtifactKind::Yaml,
    ArtifactKind::Toml,
    ArtifactKind::Sql,
    ArtifactKind::Html,
    ArtifactKind::Markdown,
];

/// Fragments drawn from the structural vocabulary of every supported artifact
/// type, so the generator can assemble half-formed / partially-valid documents
/// that stress each extractor's structured path rather than only its textual
/// fallback.
const FRAGMENTS: &[&str] = &[
    // YAML / TOML-ish
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
    // SQL-ish
    "CREATE TABLE",
    "CREATE TABLE t (",
    "id INT,",
    "name TEXT",
    ");",
    "SELECT * FROM t;",
    "CREATE TABLE (",
    ",",
    "()",
    // HTML / JS-ish
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
    // Markdown-ish
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
    // Nasty scalars / control / unicode
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

/// A single messy line: either a structural fragment or an arbitrary unicode
/// blob (including control characters via `(?s)`).
fn messy_line() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => prop::sample::select(FRAGMENTS).prop_map(|s| s.to_string()),
        3 => "(?s).{0,40}",
        1 => "[\\x00-\\x1f\\u{80}-\\u{ffff}]{0,16}",
    ]
}

/// An arbitrary source document: a handful of messy lines joined with newlines,
/// with rare degenerate and oversized cases mixed in.
fn source_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // The common case: 0..24 messy lines assembled into a document.
        20 => prop::collection::vec(messy_line(), 0..24)
            .prop_map(|lines| lines.join("\n")),
        // Fully arbitrary unicode incl. newlines/control chars, no structure.
        6 => "(?s).{0,300}",
        // Degenerate inputs.
        1 => Just(String::new()),
        1 => Just("   \n\t\n  ".to_string()),
        // Deeply nested / repeated structures (nesting, huge width).
        1 => (1usize..400).prop_map(|n| {
            (0..n).map(|i| format!("{}k{i}:", "  ".repeat(i % 30))).collect::<Vec<_>>().join("\n")
        }),
        // Occasionally huge to exercise fallback boundaries and large spans.
        1 => (1usize..12000).prop_map(|n| "x\n".repeat(n)),
    ]
}

proptest! {
    // Minimum 100 iterations; one test, looping over all kinds internally.
    #![proptest_config(ProptestConfig::with_cases(300))]

    // Feature: non-code-artifact-coverage, Property 4: Every emitted artifact symbol is valid
    #[test]
    fn every_emitted_artifact_symbol_is_valid(source in source_strategy()) {
        for kind in ALL_KINDS {
            let out = extract_artifact(kind, "artifact/input.dat", &source);
            for sym in &out.symbols {
                // The core invariant: every emitted symbol must pass validate()
                // — non-empty id, line_end >= line_start >= 1, risk_score ∈ [0,1].
                sym.validate().map_err(|e| {
                    TestCaseError::fail(format!(
                        "extract_artifact({kind:?}) emitted an invalid symbol: {e}\n\
                         name={:?} line_start={} line_end={} id_empty={}\n\
                         source(len={}): {:?}",
                        sym.name,
                        sym.line_start,
                        sym.line_end,
                        sym.id.is_empty(),
                        source.len(),
                        source.chars().take(200).collect::<String>(),
                    ))
                })?;

                // Restate the flagship span invariant explicitly for a sharper
                // failure message if validate()'s contract ever changes.
                prop_assert!(
                    sym.line_end >= sym.line_start && sym.line_start >= 1,
                    "span invariant violated for {kind:?}: line_start={} line_end={}",
                    sym.line_start,
                    sym.line_end
                );
            }
        }
    }
}
