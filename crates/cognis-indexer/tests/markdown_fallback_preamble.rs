//! Unit/integration tests for the Markdown extractor's whole-file textual
//! fallback and its leading-preamble handling (Task 6.3, Requirements 5.5 and
//! 5.6).
//!
//! Feature: non-code-artifact-coverage
//!
//! ## What this pins
//!
//! - Requirement 5.5: "IF a Markdown Artifact_File contains no headings, THEN
//!   THE Markdown_Extractor SHALL fall back to a single textual symbol spanning
//!   the whole file so the file remains searchable and the batch continues."
//! - Requirement 5.6: "WHERE a Markdown Artifact_File contains body text before
//!   its first heading, THE Markdown_Extractor SHALL emit one `SymbolKind::Module`
//!   symbol for that leading preamble section so leading content remains
//!   searchable."
//!
//! These drive the public artifact API
//! (`cognis_indexer::parser::artifact::extract_artifact`) with
//! `cognis_indexer::ArtifactKind::Markdown`. The extractor
//! (`crates/cognis-indexer/src/parser/artifact/markdown.rs`) routes a
//! heading-less document to the shared whole-file `textual_fallback` (exactly one
//! `SymbolKind::Module` symbol spanning line 1..last with `fell_back == true`),
//! and emits a dedicated `preamble` `Module` symbol for non-blank body text that
//! appears before the first ATX heading.

use cognis_core::SymbolKind;
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;

/// The name the extractor assigns to the leading-preamble section (Req 5.6).
const PREAMBLE_NAME: &str = "preamble";

/// Convenience: extract Markdown symbols for a fixed doc path.
fn extract_md(source: &str) -> cognis_indexer::parser::ParseOutput {
    extract_artifact(ArtifactKind::Markdown, "docs/GUIDE.md", source)
}

/// Collect the emitted symbol names.
fn names_of(out: &cognis_indexer::parser::ParseOutput) -> Vec<String> {
    out.symbols.iter().map(|s| s.name.clone()).collect()
}

// ===========================================================================
// Req 5.5 — heading-less document → one whole-file textual fallback symbol
// ===========================================================================

/// A heading-less Markdown document (plain prose) yields **exactly one** Module
/// fallback symbol, `fell_back == true`, spanning the whole file (line 1..last),
/// and the symbol passes `Symbol::validate`.
#[test]
fn heading_less_document_yields_one_whole_file_fallback() {
    let source = "Just some prose.\nNo headings at all here.\nA third line of body.\n";
    let expected_last_line = source.lines().count() as u32;
    assert!(expected_last_line >= 2, "fixture must be multi-line");

    let out = extract_md(source);

    // Exactly one symbol …
    assert_eq!(
        out.symbols.len(),
        1,
        "heading-less Markdown must yield exactly one fallback symbol, got {:?}",
        out.symbols
    );
    // … and it is the textual fallback.
    assert!(
        out.fell_back,
        "heading-less Markdown must be reported as a textual fallback (fell_back == true)"
    );

    let sym = &out.symbols[0];

    // The whole-file fallback symbol is a `Module` (Req 5.5 fallback shape).
    assert_eq!(
        sym.kind,
        SymbolKind::Module,
        "the fallback symbol must be a Module symbol"
    );

    // Spans the whole file: line 1 to the last line.
    assert_eq!(sym.line_start, 1, "fallback must start at line 1");
    assert_eq!(
        sym.line_end, expected_last_line,
        "fallback must span to the last line ({expected_last_line})"
    );

    // Carries the Markdown artifact language tag and remains searchable.
    assert_eq!(sym.language, "markdown");
    assert!(
        sym.body_excerpt.as_deref().unwrap_or("").contains("prose"),
        "fallback body must carry the file text so it stays searchable"
    );

    // Honors every `Symbol::validate` invariant, in particular
    // `line_end >= line_start >= 1` (Req 5.5).
    sym.validate()
        .expect("fallback symbol must satisfy Symbol::validate");
    assert!(sym.line_end >= sym.line_start && sym.line_start >= 1);
}

/// A single-line heading-less document is still a valid whole-file fallback
/// spanning line 1..1.
#[test]
fn single_line_heading_less_document_falls_back() {
    let out = extract_md("only one prose line, no heading\n");

    assert_eq!(out.symbols.len(), 1, "{:?}", out.symbols);
    assert!(out.fell_back);
    let sym = &out.symbols[0];
    assert_eq!(sym.kind, SymbolKind::Module);
    assert_eq!(sym.line_start, 1);
    assert_eq!(sym.line_end, 1);
    sym.validate().expect("fallback symbol must be valid");
}

// ===========================================================================
// Req 5.6 — preamble before the first heading gets its own Module symbol
// ===========================================================================

/// Non-blank body text before the first `#` heading yields its own `preamble`
/// Module symbol spanning the preamble lines; its `body_excerpt` contains the
/// preamble text but NOT the first heading's body. The heading section(s) are
/// also emitted.
#[test]
fn preamble_before_first_heading_is_emitted_as_own_symbol() {
    let source = "This is intro prose.\nMore intro context.\n# First\nheading body line\n";
    let out = extract_md(source);

    // Structured extraction (headings present) → not a fallback.
    assert!(
        !out.fell_back,
        "a document with headings must not fall back"
    );

    // The preamble symbol exists, is a Module, and spans lines 1..2 (the lines
    // before the first heading on line 3).
    let preamble = out
        .symbols
        .iter()
        .find(|s| s.name == PREAMBLE_NAME)
        .expect("a leading-preamble Module symbol must be emitted");
    assert_eq!(
        preamble.kind,
        SymbolKind::Module,
        "the preamble section is a Module symbol"
    );
    assert_eq!(preamble.line_start, 1, "preamble starts at line 1");
    assert_eq!(
        preamble.line_end, 2,
        "preamble spans up to the line before the first heading"
    );

    // Its searchable text carries the preamble prose …
    let body = preamble.body_excerpt.as_deref().unwrap_or("");
    assert!(
        body.contains("intro prose"),
        "preamble body must contain the preamble text: {body:?}"
    );
    assert!(
        body.contains("More intro context"),
        "preamble body must contain every preamble line: {body:?}"
    );
    // … but NOT the first heading's text or body (non-overlapping sections).
    assert!(
        !body.contains("First"),
        "preamble body must not include the first heading text: {body:?}"
    );
    assert!(
        !body.contains("heading body line"),
        "preamble body must not include the first heading's body: {body:?}"
    );
    preamble.validate().expect("preamble symbol must be valid");

    // The heading section is also emitted alongside the preamble (Req 5.6 keeps
    // both the leading content and the heading sections searchable).
    let first = out
        .symbols
        .iter()
        .find(|s| s.name == "First")
        .expect("the first heading section must also be emitted");
    assert_eq!(first.kind, SymbolKind::Module);
    assert_eq!(first.line_start, 3, "heading is on line 3");
    assert!(
        first
            .body_excerpt
            .as_deref()
            .unwrap_or("")
            .contains("heading body line"),
        "the heading section carries its own body"
    );
    first.validate().expect("heading symbol must be valid");

    // Exactly one preamble symbol is ever emitted.
    assert_eq!(
        names_of(&out)
            .iter()
            .filter(|n| n.as_str() == PREAMBLE_NAME)
            .count(),
        1,
        "at most one preamble symbol: {:?}",
        names_of(&out)
    );
}

// ===========================================================================
// Req 5.6 — no preamble when the document begins with a heading
// ===========================================================================

/// A document starting directly with a heading yields NO preamble symbol.
#[test]
fn document_starting_with_heading_has_no_preamble() {
    let out = extract_md("# Top\nbody\n## Next\nmore\n");

    assert!(!out.fell_back);
    assert!(
        !names_of(&out).contains(&PREAMBLE_NAME.to_string()),
        "no preamble symbol when the document opens with a heading: {:?}",
        names_of(&out)
    );
    // The heading sections are still emitted.
    assert!(names_of(&out).contains(&"Top".to_string()));
    assert!(names_of(&out).contains(&"Next".to_string()));
}

/// A document with only blank lines before the first heading yields NO preamble
/// symbol — blank lines are not "body text" (Req 5.6).
#[test]
fn blank_lines_before_first_heading_produce_no_preamble() {
    let out = extract_md("\n\n   \n# Top\nbody\n");

    assert!(!out.fell_back);
    assert!(
        !names_of(&out).contains(&PREAMBLE_NAME.to_string()),
        "blank-only lines before the first heading must not make a preamble: {:?}",
        names_of(&out)
    );
    assert!(names_of(&out).contains(&"Top".to_string()));
}
