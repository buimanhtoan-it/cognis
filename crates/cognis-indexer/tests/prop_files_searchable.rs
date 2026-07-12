//! Property-based test that non-empty artifact files remain searchable (Task 7.3).
//!
//! Feature: non-code-artifact-coverage, Property 6: Non-empty artifact files remain searchable
//!
//! Validates: Requirements 2.7, 3.6, 4.4, 5.5
//!
//! ## The property
//!
//! *For any* non-empty admitted artifact file (of any supported type), the
//! extractor emits at least one symbol, so the file's content is never silently
//! dropped from the index.
//!
//! ## Precise statement driven here
//!
//! The shared `textual_fallback` returns **zero** symbols only for
//! empty/whitespace-only source (`source.trim().is_empty()`). So the property
//! applies to any source that is **not** whitespace-only — i.e. contains at
//! least one non-whitespace character. For every such source and for **each**
//! [`ArtifactKind`] variant, `extract_artifact(...).symbols` is non-empty
//! (`len >= 1`), whether that comes from structured leaves (keys, columns,
//! routes/functions, heading sections) or from the whole-file textual fallback.
//!
//! ## How it is driven
//!
//! The generator produces a mix of structured content and arbitrary
//! non-whitespace strings (unicode, control chars, random fragments), then
//! **guarantees** the source has at least one non-whitespace character by
//! appending a sentinel non-whitespace glyph. A defensive `prop_filter` also
//! rejects any residual whitespace-only case. The assertion is never weakened:
//! a non-whitespace input that yields zero symbols for some kind is a real bug
//! (content silently dropped) and is reported as such.

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
/// that stress each extractor's structured path as well as its textual fallback.
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
    // Nasty scalars / control / unicode (all contain non-whitespace glyphs)
    "\u{0}",
    "\u{7}",
    "\u{1b}[0m",
    "日本語: 値",
    "emoji 🚀 key: 🔥",
    "\"unterminated",
    "'",
    "\\",
];

/// A single line: either a structural fragment or an arbitrary unicode blob
/// (including control characters via `(?s)`).
fn any_line() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => prop::sample::select(FRAGMENTS).prop_map(|s| s.to_string()),
        3 => "(?s).{0,40}",
        1 => "[\\x00-\\x1f\\u{80}-\\u{ffff}]{0,16}",
    ]
}

/// An arbitrary source document that is **guaranteed non-whitespace**: assemble
/// a mix of structured and random lines, then append a sentinel non-whitespace
/// glyph so `source.trim()` is never empty. This keeps the generator inside the
/// property's precondition (source with at least one non-whitespace char)
/// without discarding most cases.
fn nonwhitespace_source_strategy() -> impl Strategy<Value = String> {
    let sentinel = prop::sample::select(&["x", "1", "#", "/", "日", "🚀", "\u{7}"][..]);
    let body = prop_oneof![
        // The common case: 0..24 lines assembled into a document.
        20 => prop::collection::vec(any_line(), 0..24).prop_map(|lines| lines.join("\n")),
        // Fully arbitrary unicode incl. newlines/control chars, no structure.
        6 => "(?s).{0,300}".prop_map(|s| s),
        // Whitespace-heavy body: the sentinel is what keeps it in-precondition.
        2 => "[ \\t\\r\\n]{0,40}".prop_map(|s| s),
        // Deeply nested / repeated structures (nesting, huge width).
        1 => (1usize..400).prop_map(|n| {
            (0..n).map(|i| format!("{}k{i}:", "  ".repeat(i % 30))).collect::<Vec<_>>().join("\n")
        }),
        // Occasionally huge to exercise fallback boundaries and large spans.
        1 => (1usize..12000).prop_map(|n| "x\n".repeat(n)),
    ];
    // Interleave the sentinel among the body (front or back at random) so the
    // non-whitespace char is not always in the same position.
    (body, sentinel, any::<bool>()).prop_map(|(body, sentinel, prepend)| {
        if prepend {
            format!("{sentinel}\n{body}")
        } else {
            format!("{body}\n{sentinel}")
        }
    })
}

proptest! {
    // Minimum 100 iterations; one test, looping over all kinds internally.
    #![proptest_config(ProptestConfig::with_cases(300))]

    // Feature: non-code-artifact-coverage, Property 6: Non-empty artifact files remain searchable
    #[test]
    fn nonempty_artifact_files_remain_searchable(source in nonwhitespace_source_strategy()) {
        // Precondition guard: the property only applies to source that is not
        // whitespace-only. The generator guarantees this, but assert it so a
        // regression in the generator surfaces as a discard, not a false pass.
        prop_assume!(!source.trim().is_empty());

        for kind in ALL_KINDS {
            let out = extract_artifact(kind, "artifact/input.dat", &source);
            // The core invariant: a non-whitespace file always yields at least
            // one symbol (structured leaves or the whole-file textual fallback),
            // so its content is never silently dropped from the index.
            prop_assert!(
                !out.symbols.is_empty(),
                "extract_artifact({kind:?}) emitted ZERO symbols for non-whitespace source \
                 (content silently dropped)\nsource(len={}): {:?}",
                source.len(),
                source.chars().take(200).collect::<String>(),
            );
        }
    }
}
