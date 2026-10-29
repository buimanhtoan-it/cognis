//! Property-based test for Markdown section partition (Task 6.2).
//!
//! Feature: non-code-artifact-coverage, Property 11: Markdown section partition
//!
//! Validates: Requirements 5.1, 5.2, 5.6
//!
//! ## The property
//!
//! *For any* Markdown document, the emitted `SymbolKind::Module` heading-section
//! symbols (plus a leading preamble symbol when preamble text exists) partition
//! the file's lines into non-overlapping ranges, each symbol's text containing
//! its own heading and body lines and no other section's body.
//!
//! ## How it is driven
//!
//! Rather than round-tripping arbitrary Markdown (whose section set would be
//! unknowable), the generator produces a **known model** — an optional leading
//! preamble block plus a sequence of headings (levels 1–6), each with a body of
//! `N` lines — and renders *both* the document text *and* the exact expected
//! section set from that same model. The document is then fed through the
//! genuine public extractor (`extract_artifact`) and the emitted symbols are
//! checked against the model.
//!
//! Every heading text and every body/preamble line is a **globally unique,
//! delimited token** (`w{n}w`) drawn from a monotonic counter, so:
//!
//! - leaf tokens never collide and never form a substring of one another
//!   (`w1w` is not a substring of `w11w` / `w10w`, because the wrapping `w`
//!   delimiters never line up), making the per-section content oracle exact;
//! - no generated body/preamble line can itself parse as an ATX heading (never
//!   starts with `#`) or a fenced-code marker (never starts with ```` ``` ````
//!   / `~~~`), so the model's section boundaries are exactly the generated
//!   headings — the documented supported surface, with no fenced-code or
//!   accidental-heading edge cases.
//!
//! The generator constrains the preamble to be either **absent** (the document
//! begins directly with a heading) or **present with non-blank content**, so a
//! preamble symbol is emitted **iff** non-blank preamble text was generated
//! (Req 5.6) and the emitted spans always tile `[1, total]` with no gaps. The
//! generator always emits at least one heading, so this exercises the
//! structured path, never the heading-less whole-file fallback (tested
//! separately as a unit test for Req 5.5).

use std::collections::BTreeMap;

use cognis_core::SymbolKind;
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::ArtifactKind;
use proptest::prelude::*;

/// Fixed repo-relative path fed to the extractor.
const FILE: &str = "docs/GUIDE.md";

/// Name the extractor assigns to the leading-preamble section (Req 5.6).
const PREAMBLE_NAME: &str = "preamble";

// ===========================================================================
// Generators — a structural spec; tokens are assigned during rendering so
// every heading / body / preamble line is globally unique.
// ===========================================================================

/// One heading section spec: an ATX level (1–6) and a body line count (0–4).
/// A body of 0 exercises heading-immediately-followed-by-heading boundaries.
fn section_spec() -> impl Strategy<Value = (usize, usize)> {
    (1usize..=6, 0usize..=4)
}

/// A document spec: `(preamble_len, sections)`. `preamble_len == 0` means the
/// document begins with a heading; otherwise the preamble is 1–3 non-blank
/// content lines. At least one section is always generated.
fn doc_spec() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    (
        prop_oneof![
            1 => Just(0usize),   // no preamble → document starts with a heading
            2 => 1usize..=3,   // non-blank preamble content
        ],
        prop::collection::vec(section_spec(), 1..5),
    )
}

// ===========================================================================
// Rendering: model -> (source, preamble_tokens, [(heading_token, body_tokens)])
// ===========================================================================

/// Monotonic token allocator producing globally unique, delimited tokens.
struct Ctr(u32);
impl Ctr {
    fn tok(&mut self) -> String {
        let n = self.0;
        self.0 += 1;
        format!("w{n}w")
    }
}

/// Render the model to Markdown text and return the exact expected sections.
/// `(rendered source, preamble tokens, [(heading token, body tokens)])`.
type RenderedDoc = (String, Vec<String>, Vec<(String, Vec<String>)>);

fn render(preamble_len: usize, sections: &[(usize, usize)]) -> RenderedDoc {
    let mut ctr = Ctr(0);
    let mut lines: Vec<String> = Vec::new();

    // Leading preamble (non-blank content lines), if any.
    let mut preamble_tokens: Vec<String> = Vec::new();
    for _ in 0..preamble_len {
        let t = ctr.tok();
        lines.push(t.clone());
        preamble_tokens.push(t);
    }

    // Heading sections. Levels vary 1–6 to exercise "next heading of any level".
    let mut expected: Vec<(String, Vec<String>)> = Vec::new();
    for (level, body_len) in sections {
        let heading = ctr.tok();
        lines.push(format!("{} {}", "#".repeat(*level), heading));
        let mut body: Vec<String> = Vec::new();
        for _ in 0..*body_len {
            let b = ctr.tok();
            lines.push(b.clone());
            body.push(b);
        }
        expected.push((heading, body));
    }

    let mut source = lines.join("\n");
    source.push('\n');
    (source, preamble_tokens, expected)
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 11.
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 11: Markdown section partition
    #[test]
    fn markdown_sections_partition_file(
        (preamble_len, sections) in doc_spec(),
    ) {
        let (source, preamble_tokens, expected) = render(preamble_len, &sections);
        // Use the identical line-count basis the extractor uses.
        let total = source.lines().count() as u32;

        let out = extract_artifact(ArtifactKind::Markdown, FILE, &source);

        // With at least one heading, structured extraction runs — never the
        // heading-less whole-file fallback.
        prop_assert!(
            !out.fell_back,
            "a document with >=1 heading must not fall back:\n{source}"
        );

        // --- Every emitted symbol is a valid Module (Req 5.4 invariant). ---
        for s in &out.symbols {
            prop_assert_eq!(
                s.kind,
                SymbolKind::Module,
                "every Markdown symbol must be a Module, got {:?} for {}",
                s.kind,
                s.name
            );
            s.validate()
                .map_err(|e| TestCaseError::fail(format!("emitted symbol failed validate(): {e}")))?;
            prop_assert!(
                s.line_end >= s.line_start && s.line_start >= 1,
                "span invariant line_end >= line_start >= 1 violated: {:?}",
                (s.line_start, s.line_end)
            );
        }

        // --- A preamble symbol exists iff non-blank preamble text was generated
        // (Req 5.6). ---
        let has_preamble_sym = out.symbols.iter().any(|s| s.name == PREAMBLE_NAME);
        prop_assert_eq!(
            has_preamble_sym,
            !preamble_tokens.is_empty(),
            "preamble symbol presence must match generated preamble content"
        );

        // --- One symbol per generated heading section, plus the optional
        // preamble (Req 5.1). ---
        let expected_count = expected.len() + usize::from(!preamble_tokens.is_empty());
        prop_assert_eq!(
            out.symbols.len(),
            expected_count,
            "symbol count must equal heading sections (+ preamble). source:\n{}",
            source
        );

        // --- Partition (Req 5.1, 5.2): sorted spans tile [1, total] contiguously
        // with no gaps and no overlaps. ---
        let mut spans: Vec<(u32, u32)> =
            out.symbols.iter().map(|s| (s.line_start, s.line_end)).collect();
        spans.sort_unstable();
        prop_assert_eq!(spans[0].0, 1, "partition must start at line 1: {:?}", spans);
        prop_assert_eq!(
            spans.last().unwrap().1,
            total,
            "partition must end at the last line ({}): {:?}",
            total,
            spans
        );
        for w in spans.windows(2) {
            prop_assert_eq!(
                w[1].0,
                w[0].1 + 1,
                "sections must be contiguous and non-overlapping: {:?}",
                spans
            );
        }

        // --- Content (Req 5.2): each symbol's searchable text contains its own
        // heading + body tokens and NO other section's tokens. ---
        // Build the owner -> own-tokens oracle. The heading section's owner key is
        // its (unique) heading token, which equals the emitted symbol name; the
        // preamble owner key is the constant PREAMBLE_NAME.
        let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if !preamble_tokens.is_empty() {
            own.insert(PREAMBLE_NAME.to_string(), preamble_tokens.clone());
        }
        for (heading, body) in &expected {
            let mut toks = Vec::with_capacity(body.len() + 1);
            toks.push(heading.clone());
            toks.extend(body.iter().cloned());
            own.insert(heading.clone(), toks);
        }

        for s in &out.symbols {
            let text = s.body_excerpt.as_deref().unwrap_or("");
            let my = own
                .get(&s.name)
                .expect("emitted symbol name must be a known section owner");

            // Its own heading + body lines are present.
            for t in my {
                prop_assert!(
                    text.contains(t.as_str()),
                    "section {} text must contain its own token {t:?}\ntext: {text:?}",
                    s.name
                );
            }

            // No other section's tokens leak into this section's text.
            for (other, toks) in &own {
                if other == &s.name {
                    continue;
                }
                for t in toks {
                    prop_assert!(
                        !text.contains(t.as_str()),
                        "section {} text must NOT contain {other}'s token {t:?}\ntext: {text:?}",
                        s.name
                    );
                }
            }
        }
    }
}
