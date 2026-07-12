//! Markdown artifact extractor (Task 6.1, Req 5).
//!
//! Scans a Markdown file for ATX headings and emits **answer-granularity**
//! symbols — one [`SymbolKind::Module`] per heading section, never a whole-file
//! blob (Req 5.3):
//!
//! - one `Module` per ATX heading (levels 1–6). A heading section spans from its
//!   heading line up to the line immediately **before** the next heading of any
//!   level, or the end of file (Req 5.1). Sections therefore **partition** the
//!   file's lines and never overlap (Req 5.2).
//! - a leading-preamble `Module` when the file has body text before its first
//!   heading (Req 5.6). The preamble spans line 1 to the line before the first
//!   heading.
//!
//! Per emitted symbol:
//! - `name` is the heading text (or `preamble` for the leading section).
//! - searchable text (`body_excerpt`) = the heading text plus **only** that
//!   section's own body lines — no other section's body (Req 5.2).
//! - `line_start` = the heading line and `line_end` = the last body line of the
//!   section (Req 5.4), honoring the `line_end >= line_start >= 1` invariant of
//!   `Symbol::validate`.
//!
//! When the document has **no** ATX headings, extraction routes to the shared
//! whole-file [`textual_fallback`](super::textual_fallback) so the file stays
//! searchable and the batch continues (Req 5.5) — the same fault-tolerant
//! discipline as `parse_source`.
//!
//! ## Tolerant hand-rolled scan
//!
//! Rather than pull in a full CommonMark parser, this module hand-rolls a small
//! line scanner over the mainstream ATX-heading surface. Crucially it tracks
//! **fenced code blocks** (```` ``` ```` and `~~~`): a `#` inside a fenced code
//! block is code, not a heading, so such lines are never treated as section
//! boundaries. Headings may carry up to three leading spaces (CommonMark) and an
//! optional trailing `#` closing sequence.

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use super::textual_fallback;
use crate::parser::support::module_from_path;
use crate::parser::{content_hash, make_symbol_id, ParseOutput};
use crate::pipeline::ArtifactKind;

/// Language / id-prefix tag for Markdown artifact symbols.
const LABEL: &str = "markdown";

/// Max length (in characters) of an emitted symbol's searchable text.
const TEXT_LIMIT: usize = 4096;

/// Name assigned to the leading-preamble section (Req 5.6).
const PREAMBLE_NAME: &str = "preamble";

/// One heading occurrence: 0-based line index and its cleaned heading text.
struct Heading {
    line_idx: usize,
    text: String,
}

/// Extract typed Markdown heading-section symbols from `source`.
///
/// Emits one [`SymbolKind::Module`] per ATX heading section (plus a leading
/// preamble section when preamble text exists). Falls back to a single whole-file
/// textual symbol when the document contains no headings (Req 5.5).
pub(crate) fn extract(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    let lines: Vec<&str> = source.lines().collect();
    let headings = scan_headings(&lines);

    // Req 5.5: no headings at all → single whole-file textual fallback so the
    // file remains searchable and the batch is never aborted.
    if headings.is_empty() {
        return textual_fallback(kind, file_path, source);
    }

    let module = module_from_path(file_path);
    let total = lines.len();
    let mut symbols: Vec<Symbol> = Vec::new();

    // Req 5.6: leading preamble — body text before the first heading gets its own
    // `Module` symbol spanning line 1 to the line before the first heading.
    let first = headings[0].line_idx;
    if first > 0 && has_content(&lines[0..first]) {
        let body: String = lines[0..first].join("\n");
        symbols.push(build_symbol(
            file_path,
            &module,
            PREAMBLE_NAME,
            &format!("{PREAMBLE_NAME}@1"),
            body,
            1,
            first as u32, // 1-based line number of the last preamble line
        ));
    }

    // One symbol per heading section. Section i covers 0-based lines
    // [h_i, h_{i+1}) → 1-based [h_i+1, h_{i+1}], or to EOF for the last heading
    // (Req 5.1). Sections partition the file and never overlap (Req 5.2).
    for (i, heading) in headings.iter().enumerate() {
        let start_idx = heading.line_idx;
        let end_idx = headings.get(i + 1).map(|h| h.line_idx).unwrap_or(total); // exclusive end
        let line_start = (start_idx + 1) as u32;
        let line_end = (end_idx.max(start_idx + 1)) as u32;

        // Searchable text = heading text + only this section's body lines
        // (the lines after the heading, up to but not including the next
        // heading). No other section's body is included (Req 5.2).
        let body_lines = &lines[start_idx + 1..end_idx.max(start_idx + 1)];
        let text = if body_lines.is_empty() {
            heading.text.clone()
        } else {
            format!("{}\n{}", heading.text, body_lines.join("\n"))
        };

        let name = if heading.text.trim().is_empty() {
            format!("heading-{line_start}")
        } else {
            heading.text.clone()
        };
        let id_key = format!("{name}@{line_start}");
        symbols.push(build_symbol(
            file_path, &module, &name, &id_key, text, line_start, line_end,
        ));
    }

    ParseOutput {
        symbols,
        status: ParseStatus::Ok,
        language: Some(LABEL),
        fell_back: false,
    }
}

/// True when any line in the slice has non-whitespace content.
fn has_content(lines: &[&str]) -> bool {
    lines.iter().any(|l| !l.trim().is_empty())
}

/// Truncate searchable text to [`TEXT_LIMIT`] characters (not bytes).
fn truncate_text(text: String) -> String {
    if text.chars().count() > TEXT_LIMIT {
        text.chars().take(TEXT_LIMIT).collect()
    } else {
        text
    }
}

/// Build one `Module` symbol for a section, defensively clamping the span to the
/// `line_end >= line_start >= 1` validate invariant.
fn build_symbol(
    file_path: &str,
    module: &str,
    name: &str,
    id_key: &str,
    text: String,
    line_start: u32,
    line_end: u32,
) -> Symbol {
    let text = truncate_text(text);
    let line_start = line_start.max(1);
    let line_end = line_end.max(line_start);
    let qualified_name = format!("{LABEL}:{file_path}:{name}");
    Symbol {
        id: make_symbol_id(LABEL, file_path, id_key, &text),
        kind: SymbolKind::Module,
        name: name.to_string(),
        qualified_name,
        language: LABEL.to_string(),
        module: module.to_string(),
        file_path: file_path.to_string(),
        line_start,
        line_end,
        signature: None,
        docstring: None,
        content_hash: content_hash(&text),
        body_excerpt: Some(text),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

// ===========================================================================
// Scanner
// ===========================================================================

/// Scan `lines` for ATX headings, skipping any `#` line inside a fenced code
/// block (```` ``` ```` / `~~~`). Returns headings in document order.
fn scan_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (idx, line) in lines.iter().enumerate() {
        match fence {
            Some((ch, len)) => {
                // Inside a fenced code block: only a matching closing fence ends
                // it; nothing else (including a `#` line) is a heading.
                if let Some((fch, frun, only_fence)) = fence_marker(line) {
                    if fch == ch && frun >= len && only_fence {
                        fence = None;
                    }
                }
            }
            None => {
                if let Some((fch, frun, _)) = fence_marker(line) {
                    // Opening fence: enter code-block mode.
                    fence = Some((fch, frun));
                    continue;
                }
                if let Some(text) = parse_atx_heading(line) {
                    headings.push(Heading {
                        line_idx: idx,
                        text,
                    });
                }
            }
        }
    }
    headings
}

/// Detect a fenced code-block marker line. Returns `(fence_char, run_len,
/// only_fence)` where `only_fence` is true when the line has no non-whitespace
/// content after the fence run (a valid closing fence). Up to three leading
/// spaces are permitted (CommonMark).
fn fence_marker(line: &str) -> Option<(char, usize, bool)> {
    let leading = line.chars().take_while(|&c| c == ' ').count();
    if leading > 3 {
        return None;
    }
    let rest = &line[leading..];
    let first = rest.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = rest.chars().take_while(|&c| c == first).count();
    if run < 3 {
        return None;
    }
    let after: String = rest.chars().skip(run).collect();
    let only_fence = after.trim().is_empty();
    Some((first, run, only_fence))
}

/// Parse an ATX heading line, returning its cleaned heading text when the line is
/// a heading (1–6 `#` followed by a space or end of line, with up to three
/// leading spaces). `None` otherwise.
fn parse_atx_heading(line: &str) -> Option<String> {
    let leading = line.chars().take_while(|&c| c == ' ').count();
    if leading > 3 {
        return None;
    }
    let rest = &line[leading..];
    let hashes = rest.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after: String = rest.chars().skip(hashes).collect();
    // The hash run must be followed by a space/tab or be the whole line (an
    // empty heading such as `##`).
    if !after.is_empty() {
        let next = after.chars().next().unwrap();
        if next != ' ' && next != '\t' {
            return None;
        }
    }
    Some(clean_heading_text(after.trim()))
}

/// Strip an optional ATX closing sequence (a trailing run of `#` preceded by
/// whitespace) from an already-trimmed heading body.
fn clean_heading_text(text: &str) -> String {
    let stripped = text.trim_end_matches('#');
    if stripped.len() != text.len()
        && (stripped.is_empty() || stripped.ends_with(' ') || stripped.ends_with('\t'))
    {
        stripped.trim_end().to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_src(src: &str) -> ParseOutput {
        extract(ArtifactKind::Markdown, "docs/GUIDE.md", src)
    }

    fn names_of(out: &ParseOutput) -> Vec<String> {
        out.symbols.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn one_module_per_heading() {
        let src = "# Title\nintro line\n## Security\nsecure stuff\n## Deploy\nship it\n";
        let out = extract_src(src);
        assert!(!out.fell_back);
        let names = names_of(&out);
        assert!(names.contains(&"Title".to_string()), "{names:?}");
        assert!(names.contains(&"Security".to_string()), "{names:?}");
        assert!(names.contains(&"Deploy".to_string()), "{names:?}");
        for s in &out.symbols {
            assert_eq!(s.kind, SymbolKind::Module);
            s.validate().expect("valid symbol");
            assert!(s.line_end >= s.line_start && s.line_start >= 1);
        }
    }

    #[test]
    fn sections_do_not_overlap_and_span_to_next_heading() {
        let src = "# A\na1\na2\n## B\nb1\n";
        let out = extract_src(src);
        let a = out.symbols.iter().find(|s| s.name == "A").unwrap();
        let b = out.symbols.iter().find(|s| s.name == "B").unwrap();
        // A: heading line 1, body lines 2..3 → line_end 3 (line before ## B).
        assert_eq!(a.line_start, 1);
        assert_eq!(a.line_end, 3);
        // B: heading line 4, body line 5 → line_end 5 (EOF).
        assert_eq!(b.line_start, 4);
        assert_eq!(b.line_end, 5);
        // A's text must not contain B's body.
        assert!(!a.body_excerpt.as_deref().unwrap().contains("b1"));
        assert!(a.body_excerpt.as_deref().unwrap().contains("a1"));
    }

    #[test]
    fn searchable_text_includes_heading_and_body() {
        let src = "# ARCHITECTURE\nThe security model is layered.\n";
        let out = extract_src(src);
        let sym = out
            .symbols
            .iter()
            .find(|s| s.name == "ARCHITECTURE")
            .unwrap();
        let text = sym.body_excerpt.as_deref().unwrap();
        assert!(text.contains("ARCHITECTURE"), "{text}");
        assert!(text.contains("security model"), "{text}");
    }

    #[test]
    fn preamble_before_first_heading_is_emitted() {
        let src = "This is intro prose.\nMore intro.\n# First\nbody\n";
        let out = extract_src(src);
        let pre = out
            .symbols
            .iter()
            .find(|s| s.name == PREAMBLE_NAME)
            .expect("a preamble symbol");
        assert_eq!(pre.line_start, 1);
        assert_eq!(pre.line_end, 2);
        assert!(pre.body_excerpt.as_deref().unwrap().contains("intro prose"));
        // The preamble must not include the first heading's body.
        assert!(!pre.body_excerpt.as_deref().unwrap().contains("body"));
    }

    #[test]
    fn no_preamble_when_document_starts_with_heading() {
        let src = "# Top\nbody\n";
        let out = extract_src(src);
        assert!(!names_of(&out).contains(&PREAMBLE_NAME.to_string()));
    }

    #[test]
    fn blank_lines_before_first_heading_do_not_make_preamble() {
        let src = "\n\n# Top\nbody\n";
        let out = extract_src(src);
        assert!(!names_of(&out).contains(&PREAMBLE_NAME.to_string()));
    }

    #[test]
    fn no_headings_falls_back_to_whole_file_symbol() {
        let src = "Just prose.\nNo headings at all.\n";
        let out = extract_src(src);
        assert!(out.fell_back);
        assert_eq!(out.symbols.len(), 1);
        assert_eq!(out.symbols[0].kind, SymbolKind::Module);
        assert_eq!(out.symbols[0].line_start, 1);
    }

    #[test]
    fn hash_inside_fenced_code_block_is_not_a_heading() {
        let src = "# Real\n```\n# not a heading\ncode line\n```\ntrailing\n";
        let out = extract_src(src);
        let names = names_of(&out);
        assert!(names.contains(&"Real".to_string()), "{names:?}");
        assert!(
            !names.contains(&"not a heading".to_string()),
            "fenced # must not be a heading: {names:?}"
        );
        // Only the one real heading section.
        assert_eq!(out.symbols.len(), 1, "{names:?}");
        // The fenced content is part of the Real section body.
        assert!(out.symbols[0]
            .body_excerpt
            .as_deref()
            .unwrap()
            .contains("# not a heading"));
    }

    #[test]
    fn tilde_fence_also_guards_headings() {
        let src = "# Real\n~~~\n# nope\n~~~\n";
        let out = extract_src(src);
        assert_eq!(out.symbols.len(), 1);
        assert_eq!(out.symbols[0].name, "Real");
    }

    #[test]
    fn seven_hashes_is_not_a_heading() {
        let src = "####### too many\nplain text\n";
        let out = extract_src(src);
        // No valid heading → fallback.
        assert!(out.fell_back);
    }

    #[test]
    fn hash_without_space_is_not_a_heading() {
        let src = "#tag not a heading\njust text\n";
        let out = extract_src(src);
        assert!(out.fell_back);
    }

    #[test]
    fn closing_hash_sequence_is_stripped() {
        let src = "## Section ##\nbody\n";
        let out = extract_src(src);
        assert!(
            names_of(&out).contains(&"Section".to_string()),
            "{:?}",
            names_of(&out)
        );
    }

    #[test]
    fn all_six_levels_are_headings() {
        let src = "# h1\n## h2\n### h3\n#### h4\n##### h5\n###### h6\n";
        let out = extract_src(src);
        assert_eq!(out.symbols.len(), 6, "{:?}", names_of(&out));
    }

    #[test]
    fn empty_source_emits_no_symbols() {
        let out = extract_src("   \n\t\n");
        assert!(out.symbols.is_empty());
        assert!(out.fell_back);
    }

    #[test]
    fn sections_partition_lines_without_overlap() {
        let src = "pre\n# A\na\n## B\nb\n### C\nc\n";
        let out = extract_src(src);
        // Collect (line_start, line_end) sorted; they must tile [1, total]
        // contiguously with no gaps or overlaps.
        let mut spans: Vec<(u32, u32)> = out
            .symbols
            .iter()
            .map(|s| (s.line_start, s.line_end))
            .collect();
        spans.sort();
        let total = src.lines().count() as u32;
        assert_eq!(spans[0].0, 1, "first span starts at 1");
        assert_eq!(spans.last().unwrap().1, total, "last span ends at EOF");
        for w in spans.windows(2) {
            assert_eq!(w[1].0, w[0].1 + 1, "contiguous non-overlapping: {spans:?}");
        }
    }
}
