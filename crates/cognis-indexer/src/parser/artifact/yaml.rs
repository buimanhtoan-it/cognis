//! YAML/TOML artifact extractor (Task 3.1, Req 2).
//!
//! Parses a YAML or TOML document into a value tree, walks to **leaf keys**, and
//! emits one [`SymbolKind::Var`] per leaf — each element of a sequence/array
//! counting as its own leaf (`servers[0]`, `servers[1]`, …). This is the
//! answer-granularity contamination lever of Req 2.5: one symbol per answerable
//! setting, never a whole-file blob.
//!
//! Per emitted symbol:
//! - `name` / `qualified_name` carry the fully-qualified **dotted key path**.
//! - `body_excerpt` carries the dotted key path plus the **scalar value**
//!   truncated to at most [`SCALAR_LIMIT`] (4096) characters (Req 2.2). A null or
//!   empty value yields an empty scalar (Req 2.4). Placing the scalar in
//!   `body_excerpt` puts it on the enricher's secret-redaction path (Req 2.8).
//! - `line_start` / `line_end` bound the leaf's source line span, honoring the
//!   `line_end >= line_start >= 1` invariant of `Symbol::validate` (Req 2.6).
//!
//! ## Line spans without a marks-preserving parser
//!
//! `serde_yaml` (the only YAML parser available to the offline workspace) does
//! not surface source marks on its value tree, and no TOML parser is available,
//! so this module hand-rolls a line-tracking walker over the mainstream
//! block-style YAML and standard TOML surface. Anything the walker cannot resolve
//! into leaves (e.g. exotic flow constructs, malformed input) yields zero leaves
//! and routes to the shared whole-file [`textual_fallback`](super::textual_fallback)
//! so the file stays searchable and the batch continues — the same discipline
//! Req 2.7 mandates for the 0-leaf and >10 000-leaf boundaries.

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use crate::parser::{content_hash, make_symbol_id, ParseOutput};
use crate::pipeline::ArtifactKind;

use super::textual_fallback;
use crate::parser::support::module_from_path;
/// Max length (in characters) of the scalar-value component of a leaf's
/// searchable text (Req 2.2).
const SCALAR_LIMIT: usize = 4096;

/// Upper bound on structured leaves; above this the file falls back to a single
/// whole-file textual symbol (Req 2.7).
const MAX_LEAVES: usize = 10_000;

/// One extracted leaf key: its fully-qualified dotted path, scalar value, and
/// 1-based source line span.
struct Leaf {
    path: String,
    value: String,
    line_start: u32,
    line_end: u32,
}

/// Extract typed YAML/TOML leaf-key symbols from `source`.
///
/// Emits one [`SymbolKind::Var`] per leaf key. Falls back to a single whole-file
/// textual symbol when structured extraction yields **0** leaves or **more than
/// [`MAX_LEAVES`]** leaves (Req 2.7).
pub(crate) fn extract(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    let label = match kind {
        ArtifactKind::Toml => "toml",
        _ => "yaml",
    };

    let leaves = match kind {
        ArtifactKind::Toml => parse_toml(source),
        _ => parse_yaml(source),
    };

    // Req 2.7: 0 leaves (empty/malformed/no structure) or an implausible leaf
    // explosion routes to the shared whole-file textual fallback so the file
    // remains searchable and the batch is never aborted.
    if leaves.is_empty() || leaves.len() > MAX_LEAVES {
        return textual_fallback(kind, file_path, source);
    }

    let module = module_from_path(file_path);
    let symbols: Vec<Symbol> = leaves
        .into_iter()
        .map(|leaf| build_symbol(label, file_path, &module, leaf))
        .collect();

    ParseOutput {
        symbols,
        status: ParseStatus::Ok,
        language: Some(label),
        fell_back: false,
    }
}

/// Build one `Var` symbol for a leaf, placing the scalar value (truncated to
/// [`SCALAR_LIMIT`] chars, Req 2.2) alongside the dotted path in `body_excerpt`
/// so the enricher's secret redaction covers it (Req 2.8).
fn build_symbol(label: &str, file_path: &str, module: &str, leaf: Leaf) -> Symbol {
    let scalar: String = leaf.value.chars().take(SCALAR_LIMIT).collect();
    // Searchable text: fully-qualified dotted key path + scalar value (Req 2.2).
    // Null/empty value → key path with an empty scalar (Req 2.4).
    let text = if scalar.is_empty() {
        leaf.path.clone()
    } else {
        format!("{} {}", leaf.path, scalar)
    };
    let qualified_name = format!("{label}:{file_path}:{}", leaf.path);
    // Defensive: honor `line_end >= line_start >= 1` even if a walker slip
    // produced a degenerate span.
    let line_start = leaf.line_start.max(1);
    let line_end = leaf.line_end.max(line_start);
    Symbol {
        id: make_symbol_id(label, file_path, &leaf.path, &text),
        kind: SymbolKind::Var,
        name: leaf.path,
        qualified_name,
        language: label.to_string(),
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

fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

// ===========================================================================
// YAML (block style) walker
// ===========================================================================

/// Count of leading space characters (YAML forbids tab indentation; a stray tab
/// is counted as a single indent unit so the walker degrades rather than panics).
fn leading_indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Strip a trailing `#` comment that is outside quotes and preceded by
/// whitespace (or begins the line), matching YAML comment rules. Returns the
/// content with the comment removed (leading indentation preserved).
fn strip_yaml_comment(line: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_ws = true; // start-of-line counts as preceding whitespace
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(c);
                prev_ws = false;
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(c);
                prev_ws = false;
            }
            '#' if !in_single && !in_double && prev_ws => break,
            _ => {
                prev_ws = c == ' ' || c == '\t';
                out.push(c);
            }
        }
    }
    out
}

/// Content of `line` after comment stripping, with a trailing carriage return /
/// whitespace trimmed. `None` if the line is blank, comment-only, or a document
/// marker (`---` / `...`).
fn yaml_significant(line: &str) -> Option<String> {
    let stripped = strip_yaml_comment(line);
    let trimmed = stripped.trim_end_matches(['\r', ' ', '\t']);
    let body = trimmed.trim_start();
    if body.is_empty() || body == "---" || body == "..." {
        return None;
    }
    Some(trimmed.to_string())
}

/// Index of the next significant YAML line at or after `from`.
fn next_yaml_significant(lines: &[&str], from: usize) -> Option<usize> {
    (from..lines.len()).find(|&j| yaml_significant(lines[j]).is_some())
}

fn is_seq_item(entry: &str) -> bool {
    entry == "-" || entry.starts_with("- ") || entry.starts_with("-\t")
}

/// Strip one layer of matching surrounding quotes.
fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Map YAML null tokens (`~`, `null`, `Null`, `NULL`, empty) to an empty scalar
/// (Req 2.4); otherwise unquote the plain/quoted scalar.
fn scalar_value(value_part: &str) -> String {
    let t = value_part.trim();
    if t.is_empty() || t == "~" || t == "null" || t == "Null" || t == "NULL" {
        return String::new();
    }
    unquote(t)
}

/// Split a mapping entry into `(key, value_part)`; the value is empty for a bare
/// `key:` parent/null. Returns `None` when the entry is not a `key: value`
/// mapping line.
fn split_kv(entry: &str) -> Option<(String, String)> {
    // Quoted key: "a: b": value  /  'a': value
    if entry.starts_with('"') || entry.starts_with('\'') {
        let q = entry.chars().next().unwrap();
        if let Some(close) = entry[1..].find(q) {
            let key = entry[1..1 + close].to_string();
            let rest = entry[1 + close + 1..].trim_start();
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            return Some((key, rest.trim_start().to_string()));
        }
    }
    if let Some(pos) = find_kv_colon(entry) {
        let key = entry[..pos].trim().to_string();
        let value = entry[pos + 1..].trim_start().to_string();
        Some((key, value))
    } else {
        entry
            .strip_suffix(':')
            .map(|key| (key.trim().to_string(), String::new()))
    }
}

/// Byte index of the key/value separator `:` — the first colon followed by a
/// space/tab (or end of string), outside quotes.
fn find_kv_colon(entry: &str) -> Option<usize> {
    let bytes = entry.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b':' if !in_single && !in_double => {
                let next = bytes.get(i + 1);
                if next.is_none() || matches!(next, Some(b' ') | Some(b'\t')) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// True when `value_part` is a YAML block-scalar indicator (`|`, `>`, with
/// optional chomping/indent indicators) rather than an inline scalar.
fn is_block_scalar(value_part: &str) -> bool {
    let v = value_part.trim();
    (v.starts_with('|') || v.starts_with('>'))
        && v[1..].chars().all(|c| matches!(c, '+' | '-' | '0'..='9'))
}

fn parse_yaml(source: &str) -> Vec<Leaf> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut leaves = Vec::new();
    // Determine the top-level block's indentation from its first content line.
    if let Some(first) = next_yaml_significant(&lines, 0) {
        let base = leading_indent(lines[first]);
        let mut i = first;
        parse_yaml_block(&lines, &mut i, base, "", &mut leaves);
    }
    leaves
}

/// Parse a block of sibling entries at exactly `indent`. Dispatches to a
/// sequence or mapping based on the first significant line.
fn parse_yaml_block(
    lines: &[&str],
    i: &mut usize,
    indent: usize,
    prefix: &str,
    leaves: &mut Vec<Leaf>,
) {
    let Some(j) = next_yaml_significant(lines, *i) else {
        *i = lines.len();
        return;
    };
    if leading_indent(lines[j]) != indent {
        return;
    }
    let entry = yaml_significant(lines[j]).unwrap();
    let entry = entry.trim_start();
    if is_seq_item(entry) {
        parse_yaml_sequence(lines, i, indent, prefix, leaves);
    } else {
        parse_yaml_mapping(lines, i, indent, prefix, leaves);
    }
}

fn parse_yaml_mapping(
    lines: &[&str],
    i: &mut usize,
    indent: usize,
    prefix: &str,
    leaves: &mut Vec<Leaf>,
) {
    loop {
        let Some(j) = next_yaml_significant(lines, *i) else {
            *i = lines.len();
            return;
        };
        if leading_indent(lines[j]) != indent {
            return;
        }
        let raw = yaml_significant(lines[j]).unwrap();
        let entry = raw.trim_start().to_string();
        if is_seq_item(&entry) {
            return;
        }
        let line_no = (j + 1) as u32;
        *i = j + 1;
        parse_yaml_entry(lines, i, indent, prefix, &entry, line_no, leaves);
    }
}

/// Parse a single mapping entry (`key: value`, `key:`, or `key: |`), consuming
/// any nested child block. `*i` must already point past the entry's own line.
fn parse_yaml_entry(
    lines: &[&str],
    i: &mut usize,
    indent: usize,
    prefix: &str,
    entry: &str,
    line_no: u32,
    leaves: &mut Vec<Leaf>,
) {
    let Some((key, value_part)) = split_kv(entry) else {
        return;
    };
    let path = join_key(prefix, &unquote(&key));

    if is_block_scalar(&value_part) {
        let (value, end_line) = consume_block_scalar(lines, i, indent, line_no);
        leaves.push(Leaf {
            path,
            value,
            line_start: line_no,
            line_end: end_line,
        });
        return;
    }

    if value_part.trim().is_empty() {
        // Empty value: either a nested child block or a null leaf (Req 2.4).
        if let Some(n) = next_yaml_significant(lines, *i) {
            let child_indent = leading_indent(lines[n]);
            if child_indent > indent {
                let child = yaml_significant(lines[n]).unwrap();
                if is_seq_item(child.trim_start()) {
                    parse_yaml_sequence(lines, i, child_indent, &path, leaves);
                } else {
                    parse_yaml_mapping(lines, i, child_indent, &path, leaves);
                }
                return;
            }
        }
        // Null / empty leaf.
        leaves.push(Leaf {
            path,
            value: String::new(),
            line_start: line_no,
            line_end: line_no,
        });
        return;
    }

    // Inline scalar value.
    leaves.push(Leaf {
        path,
        value: scalar_value(&value_part),
        line_start: line_no,
        line_end: line_no,
    });
}

fn parse_yaml_sequence(
    lines: &[&str],
    i: &mut usize,
    indent: usize,
    prefix: &str,
    leaves: &mut Vec<Leaf>,
) {
    let mut index = 0usize;
    loop {
        let Some(j) = next_yaml_significant(lines, *i) else {
            *i = lines.len();
            return;
        };
        if leading_indent(lines[j]) != indent {
            return;
        }
        let raw = yaml_significant(lines[j]).unwrap();
        let entry = raw.trim_start().to_string();
        if !is_seq_item(&entry) {
            return;
        }
        let line_no = (j + 1) as u32;
        let elem_prefix = format!("{prefix}[{index}]");

        // Text after the dash (may be empty, a scalar, or an inline map entry).
        let after_dash = if entry == "-" {
            ""
        } else {
            entry[1..].trim_start()
        };
        let extra = entry.len().saturating_sub(1) - after_dash.len();
        let key_col = indent + 1 + extra; // column where inline content begins

        if after_dash.is_empty() {
            // Nested block introduced under the dash on following lines.
            *i = j + 1;
            if let Some(n) = next_yaml_significant(lines, *i) {
                let child_indent = leading_indent(lines[n]);
                if child_indent > indent {
                    let child = yaml_significant(lines[n]).unwrap();
                    if is_seq_item(child.trim_start()) {
                        parse_yaml_sequence(lines, i, child_indent, &elem_prefix, leaves);
                    } else {
                        parse_yaml_mapping(lines, i, child_indent, &elem_prefix, leaves);
                    }
                    index += 1;
                    continue;
                }
            }
            // Empty element → null leaf.
            leaves.push(Leaf {
                path: elem_prefix,
                value: String::new(),
                line_start: line_no,
                line_end: line_no,
            });
            index += 1;
            continue;
        }

        if is_inline_mapping(after_dash) {
            // Sequence of mappings: the dash begins a mapping whose first key is
            // inline and whose remaining keys align at `key_col`.
            *i = j + 1;
            parse_yaml_entry(lines, i, key_col, &elem_prefix, after_dash, line_no, leaves);
            parse_yaml_mapping(lines, i, key_col, &elem_prefix, leaves);
            index += 1;
            continue;
        }

        // Scalar element.
        leaves.push(Leaf {
            path: elem_prefix,
            value: scalar_value(after_dash),
            line_start: line_no,
            line_end: line_no,
        });
        *i = j + 1;
        index += 1;
    }
}

/// True when the text (after a dash) is itself a `key: value` mapping entry.
fn is_inline_mapping(after_dash: &str) -> bool {
    split_kv(after_dash).is_some()
}

/// Consume a YAML block scalar (`|` / `>`): all following lines more indented
/// than the owning key, plus interspersed blank lines. Returns the joined value
/// and the last content line consumed.
fn consume_block_scalar(
    lines: &[&str],
    i: &mut usize,
    key_indent: usize,
    key_line: u32,
) -> (String, u32) {
    let mut collected: Vec<String> = Vec::new();
    let mut end_line = key_line;
    while *i < lines.len() {
        let raw = lines[*i];
        let no_cr = raw.trim_end_matches('\r');
        if no_cr.trim().is_empty() {
            collected.push(String::new());
            *i += 1;
            continue;
        }
        if leading_indent(no_cr) <= key_indent {
            break;
        }
        collected.push(no_cr.trim_start().to_string());
        end_line = (*i + 1) as u32;
        *i += 1;
    }
    // Drop trailing blank lines that were speculatively collected.
    while collected.last().map(|s| s.is_empty()).unwrap_or(false) {
        collected.pop();
    }
    (collected.join("\n"), end_line)
}

// ===========================================================================
// TOML walker
// ===========================================================================

/// Strip a `#` comment outside quotes.
fn strip_toml_comment(line: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(c);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(c);
            }
            '#' if !in_single && !in_double => break,
            _ => out.push(c),
        }
    }
    out
}

fn unquote_toml_key(key: &str) -> String {
    unquote(key)
}

fn parse_toml(source: &str) -> Vec<Leaf> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut leaves = Vec::new();
    let mut table_prefix = String::new();
    // Per-array-of-tables running index, keyed by the header name.
    let mut aot: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line_no = (i + 1) as u32;
        let stripped = strip_toml_comment(lines[i]);
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some(inner) = trimmed
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
        {
            let name = inner.trim().to_string();
            let idx = aot.entry(name.clone()).or_insert(0);
            table_prefix = format!("{name}[{idx}]");
            *idx += 1;
            i += 1;
            continue;
        }
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            table_prefix = inner.trim().to_string();
            i += 1;
            continue;
        }
        // key = value
        if let Some(eq) = find_toml_eq(trimmed) {
            let key = unquote_toml_key(trimmed[..eq].trim());
            let first_part = trimmed[eq + 1..].trim().to_string();
            let path = join_key(&table_prefix, &key);
            let (value_str, end_idx) = collect_toml_value(&lines, i, &first_part);
            let end_line = (end_idx + 1) as u32;
            push_toml_value(&path, &value_str, line_no, end_line, &mut leaves);
            i = end_idx + 1;
        } else {
            i += 1;
        }
    }
    leaves
}

/// Byte index of the top-level `=` assignment separator (outside quotes/brackets).
fn find_toml_eq(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'=' if !in_single && !in_double => return Some(i),
            _ => {}
        }
    }
    None
}

/// Accumulate a possibly multi-line TOML value (arrays and triple-quoted strings
/// may span lines) starting at `start_idx` with the already-split `first_part`.
/// Returns the joined value string and the index of the last line consumed.
fn collect_toml_value(lines: &[&str], start_idx: usize, first_part: &str) -> (String, usize) {
    let mut acc = first_part.to_string();
    let mut idx = start_idx;
    while !toml_value_complete(&acc) && idx + 1 < lines.len() {
        idx += 1;
        let part = strip_toml_comment(lines[idx]);
        acc.push('\n');
        acc.push_str(part.trim_end());
    }
    (acc.trim().to_string(), idx)
}

/// Heuristic completeness check: brackets/braces balanced and triple-quoted
/// strings closed. Good enough for well-formed TOML; malformed input still
/// terminates at EOF via the `collect_toml_value` bound.
fn toml_value_complete(s: &str) -> bool {
    // Triple-quoted strings may legally span lines; if an odd number of triple
    // delimiters is present the value is not yet closed.
    if !count_occurrences(s, "\"\"\"").is_multiple_of(2)
        || !count_occurrences(s, "'''").is_multiple_of(2)
    {
        return false;
    }
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '[' if !in_single && !in_double => depth_bracket += 1,
            ']' if !in_single && !in_double => depth_bracket -= 1,
            '{' if !in_single && !in_double => depth_brace += 1,
            '}' if !in_single && !in_double => depth_brace -= 1,
            _ => {}
        }
    }
    depth_bracket <= 0 && depth_brace <= 0 && !in_single && !in_double
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        count += 1;
        rest = &rest[pos + needle.len()..];
    }
    count
}

/// Classify a collected TOML value string and push the resulting leaf/leaves.
fn push_toml_value(
    path: &str,
    value_str: &str,
    line_start: u32,
    line_end: u32,
    leaves: &mut Vec<Leaf>,
) {
    let v = value_str.trim();
    // Array → one leaf per element (Req 2.3).
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let elems = split_top_level(inner);
        if elems.is_empty() {
            leaves.push(Leaf {
                path: path.to_string(),
                value: String::new(),
                line_start,
                line_end,
            });
        } else {
            for (idx, e) in elems.iter().enumerate() {
                leaves.push(Leaf {
                    path: format!("{path}[{idx}]"),
                    value: toml_scalar(e),
                    line_start,
                    line_end,
                });
            }
        }
        return;
    }
    // Inline table → one leaf per subkey.
    if let Some(inner) = v.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let entries = split_top_level(inner);
        if entries.is_empty() {
            leaves.push(Leaf {
                path: path.to_string(),
                value: String::new(),
                line_start,
                line_end,
            });
        } else {
            for e in entries {
                if let Some(eq) = find_toml_eq(&e) {
                    let subkey = unquote_toml_key(e[..eq].trim());
                    let subval = toml_scalar(e[eq + 1..].trim());
                    leaves.push(Leaf {
                        path: join_key(path, &subkey),
                        value: subval,
                        line_start,
                        line_end,
                    });
                }
            }
        }
        return;
    }
    // Scalar.
    leaves.push(Leaf {
        path: path.to_string(),
        value: toml_scalar(v),
        line_start,
        line_end,
    });
}

/// Unquote a TOML scalar (basic/literal, incl. triple-quoted) or return it as-is.
fn toml_scalar(s: &str) -> String {
    let t = s.trim();
    for q in ["\"\"\"", "'''"] {
        if t.len() >= 2 * q.len() && t.starts_with(q) && t.ends_with(q) {
            return t[q.len()..t.len() - q.len()].trim().to_string();
        }
    }
    unquote(t)
}

/// Split a comma-separated list at the top nesting level, ignoring commas inside
/// quotes, brackets, or braces. Trailing empty elements (from a trailing comma)
/// are dropped.
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                cur.push(c);
            }
            '"' if !in_single => {
                in_double = !in_double;
                cur.push(c);
            }
            '[' | '{' if !in_single && !in_double => {
                depth += 1;
                cur.push(c);
            }
            ']' | '}' if !in_single && !in_double => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 && !in_single && !in_double => {
                out.push(cur.trim().to_string());
                cur = String::new();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|e| !e.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(kind: ArtifactKind, src: &str) -> Vec<(String, String)> {
        let out = extract(kind, "config/app.yaml", src);
        out.symbols
            .iter()
            .map(|s| (s.name.clone(), s.body_excerpt.clone().unwrap_or_default()))
            .collect()
    }

    #[test]
    fn yaml_nested_leaves() {
        let src = "server:\n  host: localhost\n  port: 8080\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"server.host".to_string()), "{names:?}");
        assert!(names.contains(&"server.port".to_string()), "{names:?}");
        for s in &out.symbols {
            assert_eq!(s.kind, SymbolKind::Var);
            s.validate().expect("valid symbol");
            assert!(s.line_end >= s.line_start && s.line_start >= 1);
        }
    }

    #[test]
    fn yaml_line_spans_are_real() {
        let src = "alpha: 1\nbeta: 2\ngamma: 3\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let beta = out.symbols.iter().find(|s| s.name == "beta").unwrap();
        assert_eq!(beta.line_start, 2);
        assert_eq!(beta.line_end, 2);
    }

    #[test]
    fn yaml_sequence_elements_are_leaves() {
        let src = "servers:\n  - alpha\n  - beta\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"servers[0]".to_string()), "{names:?}");
        assert!(names.contains(&"servers[1]".to_string()), "{names:?}");
    }

    #[test]
    fn yaml_null_leaf_has_empty_scalar() {
        let src = "password:\nother: x\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let pw = out.symbols.iter().find(|s| s.name == "password").unwrap();
        assert_eq!(pw.body_excerpt.as_deref(), Some("password"));
    }

    #[test]
    fn yaml_searchable_text_has_path_and_value() {
        let src = "jwt_secret: s3cr3t\n";
        let got = paths(ArtifactKind::Yaml, src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "jwt_secret");
        assert!(got[0].1.contains("jwt_secret"));
        assert!(got[0].1.contains("s3cr3t"));
    }

    #[test]
    fn yaml_seq_of_maps() {
        let src = "servers:\n  - name: a\n    port: 1\n  - name: b\n    port: 2\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"servers[0].name".to_string()), "{names:?}");
        assert!(names.contains(&"servers[0].port".to_string()), "{names:?}");
        assert!(names.contains(&"servers[1].name".to_string()), "{names:?}");
        assert!(names.contains(&"servers[1].port".to_string()), "{names:?}");
    }

    #[test]
    fn yaml_empty_falls_back() {
        let out = extract(ArtifactKind::Yaml, "c.yaml", "   \n\n");
        assert!(out.symbols.is_empty());
        assert!(out.fell_back);
    }

    #[test]
    fn toml_table_and_scalar() {
        let src = "[server]\nhost = \"localhost\"\nport = 8080\n";
        let out = extract(ArtifactKind::Toml, "c.toml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"server.host".to_string()), "{names:?}");
        assert!(names.contains(&"server.port".to_string()), "{names:?}");
        let host = out
            .symbols
            .iter()
            .find(|s| s.name == "server.host")
            .unwrap();
        assert!(host.body_excerpt.as_deref().unwrap().contains("localhost"));
    }

    #[test]
    fn toml_array_elements_are_leaves() {
        let src = "ports = [80, 443, 8080]\n";
        let out = extract(ArtifactKind::Toml, "c.toml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"ports[0]".to_string()), "{names:?}");
        assert!(names.contains(&"ports[2]".to_string()), "{names:?}");
    }

    #[test]
    fn toml_inline_table() {
        let src = "point = { x = 1, y = 2 }\n";
        let out = extract(ArtifactKind::Toml, "c.toml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"point.x".to_string()), "{names:?}");
        assert!(names.contains(&"point.y".to_string()), "{names:?}");
    }

    #[test]
    fn toml_array_of_tables() {
        let src = "[[product]]\nname = \"a\"\n\n[[product]]\nname = \"b\"\n";
        let out = extract(ArtifactKind::Toml, "c.toml", src);
        let names: Vec<_> = out.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"product[0].name".to_string()), "{names:?}");
        assert!(names.contains(&"product[1].name".to_string()), "{names:?}");
    }

    #[test]
    fn every_symbol_is_valid_and_not_whole_file() {
        let src = "a: 1\nb:\n  c: 2\n  d: 3\ne:\n  - x\n  - y\n";
        let out = extract(ArtifactKind::Yaml, "c.yaml", src);
        let total = src.split('\n').count() as u32;
        for s in &out.symbols {
            s.validate().expect("valid");
            // Answer-granularity: no leaf spans the whole file (Req 2.5).
            assert!(!(s.line_start == 1 && s.line_end == total));
        }
    }
}
