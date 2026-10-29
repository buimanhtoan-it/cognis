//! HTML / embedded-JS artifact extractor (Task 5.1, Req 4).
//!
//! Scans an HTML file for **answer-granularity** symbols — never a whole-file
//! blob (Req 4.3):
//!
//! - one [`SymbolKind::Route`] per **distinct** `/`-prefixed string literal found
//!   in an HTML attribute value (`href="/api/x"`, `hx-get='/state'`) or in
//!   embedded `<script>` JavaScript (`fetch('/api/world/state')`). `name` is the
//!   exact route string and the searchable text includes it (Req 4.1, 4.5).
//!   Routes are deduped by exact string, keeping the first occurrence's span.
//! - one [`SymbolKind::Function`] per named JS function definition inside a
//!   `<script>` block: function declarations (`function foo(){}`), named function
//!   expressions (`const foo = function(){}`, `var bar = function baz(){}`,
//!   arrow bindings `const foo = () => {}`), and named methods (`foo() {}`,
//!   `async foo() {}`). `name` is the declared identifier and the searchable text
//!   includes it (Req 4.2, 4.5).
//!
//! Each emitted symbol's `line_start`/`line_end` bound its source span (Req 4.1,
//! 4.2), honoring the `line_end >= line_start >= 1` invariant of
//! `Symbol::validate`.
//!
//! ## Tolerant hand-rolled scan
//!
//! No HTML/JS parser is available to the offline workspace, so this module
//! hand-rolls a small, tolerant scanner. It favors robustness over completeness:
//! anything it cannot resolve into at least one route or function routes to the
//! shared whole-file [`textual_fallback`](super::textual_fallback) so the file
//! stays searchable and the batch continues (Req 4.4) — the same fault-tolerant
//! discipline as `parse_source`.

use std::collections::HashSet;

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use super::textual_fallback;
use crate::parser::support::module_from_path;
use crate::parser::{content_hash, make_symbol_id, ParseOutput};
use crate::pipeline::ArtifactKind;

/// Language / id-prefix tag for HTML artifact symbols.
const LABEL: &str = "html";

/// Max length (in characters) of an emitted symbol's searchable text.
const TEXT_LIMIT: usize = 4096;

/// One route-literal occurrence: its exact string and 1-based source line span.
struct RouteOcc {
    /// Char offset of the literal's first content char (for first-occurrence
    /// ordering / dedupe).
    start: usize,
    value: String,
    line_start: u32,
    line_end: u32,
}

/// One named JS function: its declared identifier and 1-based source line span.
struct Func {
    name: String,
    line_start: u32,
    line_end: u32,
}

/// Extract typed HTML route / JS function symbols from `source`.
///
/// Emits one [`SymbolKind::Route`] per distinct route literal and one
/// [`SymbolKind::Function`] per named JS function. Falls back to a single
/// whole-file textual symbol when neither is found (Req 4.4).
pub(crate) fn extract(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    let chars: Vec<char> = source.chars().collect();
    let line_no = line_numbers(&chars);

    // Collect route occurrences from HTML attribute values (whole document) and
    // from embedded-JS string literals, then find named functions in each
    // <script> block.
    let mut route_occ: Vec<RouteOcc> = Vec::new();
    let mut funcs: Vec<Func> = Vec::new();

    scan_attr_routes(&chars, &line_no, &mut route_occ);
    for (s, e) in script_regions(&chars) {
        scan_js(&chars, s, e, &line_no, &mut route_occ, &mut funcs);
    }

    let routes = dedupe_routes(route_occ);

    // Req 4.4: no routes and no functions → single whole-file textual fallback so
    // the file remains searchable and the batch is never aborted.
    if routes.is_empty() && funcs.is_empty() {
        return textual_fallback(kind, file_path, source);
    }

    let module = module_from_path(file_path);
    let mut symbols: Vec<Symbol> = Vec::with_capacity(routes.len() + funcs.len());
    for route in routes {
        symbols.push(build_route_symbol(file_path, &module, &route));
    }
    for func in funcs {
        symbols.push(build_func_symbol(file_path, &module, &func));
    }

    ParseOutput {
        symbols,
        status: ParseStatus::Ok,
        language: Some(LABEL),
        fell_back: false,
    }
}

/// Dedupe route occurrences by exact string, keeping the first occurrence's line
/// span (Req 4.1: distinct routes only).
fn dedupe_routes(mut occ: Vec<RouteOcc>) -> Vec<RouteOcc> {
    occ.sort_by_key(|r| r.start);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for r in occ {
        if seen.insert(r.value.clone()) {
            out.push(r);
        }
    }
    out
}

/// Truncate searchable text to [`TEXT_LIMIT`] characters (not bytes).
fn truncate_text(text: String) -> String {
    if text.chars().count() > TEXT_LIMIT {
        text.chars().take(TEXT_LIMIT).collect()
    } else {
        text
    }
}

/// Build the `Route` symbol for a route literal. `name` is the exact route
/// string; searchable text includes it (Req 4.1, 4.5).
fn build_route_symbol(file_path: &str, module: &str, route: &RouteOcc) -> Symbol {
    let text = truncate_text(format!("route {}", route.value));
    let line_start = route.line_start.max(1);
    let line_end = route.line_end.max(line_start);
    let qualified_name = format!("{LABEL}:{file_path}:{}", route.value);
    Symbol {
        id: make_symbol_id(LABEL, file_path, &route.value, &text),
        kind: SymbolKind::Route,
        name: route.value.clone(),
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

/// Build the `Function` symbol for a named JS function. `name` is the declared
/// identifier; searchable text includes it (Req 4.2, 4.5). The id-derivation key
/// folds in the line so two same-named functions do not collide.
fn build_func_symbol(file_path: &str, module: &str, func: &Func) -> Symbol {
    let text = truncate_text(format!("function {}", func.name));
    let line_start = func.line_start.max(1);
    let line_end = func.line_end.max(line_start);
    let id_key = format!("{}@{line_start}", func.name);
    let qualified_name = format!("{LABEL}:{file_path}:{}", func.name);
    Symbol {
        id: make_symbol_id(LABEL, file_path, &id_key, &text),
        kind: SymbolKind::Function,
        name: func.name.clone(),
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
// Scanners
// ===========================================================================

/// Locate every `<script ...>` … `</script>` block and return the char span
/// `(content_start, content_end)` of its inner JS content. Case-insensitive; a
/// `>` inside a quoted attribute value does not end the opening tag.
fn script_regions(chars: &[char]) -> Vec<(usize, usize)> {
    let n = chars.len();
    let mut regions = Vec::new();
    let mut i = 0usize;
    while i < n {
        if chars[i] == '<' && matches_ci(chars, i + 1, "script") {
            // Advance to the '>' that closes the opening tag, skipping quoted
            // attribute values.
            let mut j = i + 1 + "script".len();
            while j < n && chars[j] != '>' {
                if chars[j] == '"' || chars[j] == '\'' {
                    let q = chars[j];
                    j += 1;
                    while j < n && chars[j] != q {
                        j += 1;
                    }
                }
                j += 1;
            }
            let content_start = (j + 1).min(n);
            // Find the closing </script>.
            let mut k = content_start;
            while k < n && !(chars[k] == '<' && matches_ci(chars, k + 1, "/script")) {
                k += 1;
            }
            let content_end = k.min(n);
            if content_start <= content_end {
                regions.push((content_start, content_end));
            }
            i = if k < n { k + 1 } else { n };
        } else {
            i += 1;
        }
    }
    regions
}

/// Scan the whole document for HTML attribute-value routes: a quoted value
/// (`="…"` / `='…'`) whose content begins with `/` (Req 4.1).
fn scan_attr_routes(chars: &[char], line_no: &[u32], out: &mut Vec<RouteOcc>) {
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        if chars[i] == '=' {
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && (chars[j] == '"' || chars[j] == '\'') {
                let q = chars[j];
                let vstart = j + 1;
                let mut k = vstart;
                while k < n && chars[k] != q {
                    k += 1;
                }
                if let Some(occ) = route_occ_if_slash(chars, line_no, vstart, k) {
                    out.push(occ);
                }
                i = (k + 1).max(i + 1);
                continue;
            }
        }
        i += 1;
    }
}

/// Scan a `<script>` region `[s, e)` as JavaScript, collecting `/`-prefixed
/// string-literal routes and named function definitions. Skips comments and
/// respects string/template quoting.
fn scan_js(
    chars: &[char],
    s: usize,
    e: usize,
    line_no: &[u32],
    routes: &mut Vec<RouteOcc>,
    funcs: &mut Vec<Func>,
) {
    // Last significant (non-whitespace, non-comment) char before the current
    // token. Used to tell a function *declaration* (emit the name after
    // `function`) from a function *expression* (name comes from the binding).
    let mut prev_sig = ';';
    let mut j = s;
    while j < e {
        let c = chars[j];

        // Line comment.
        if c == '/' && j + 1 < e && chars[j + 1] == '/' {
            while j < e && chars[j] != '\n' {
                j += 1;
            }
            continue;
        }
        // Block comment.
        if c == '/' && j + 1 < e && chars[j + 1] == '*' {
            j += 2;
            while j + 1 < e && !(chars[j] == '*' && chars[j + 1] == '/') {
                j += 1;
            }
            j = (j + 2).min(e);
            continue;
        }
        // String / template literal.
        if c == '\'' || c == '"' || c == '`' {
            let q = c;
            let vstart = j + 1;
            let mut k = vstart;
            while k < e {
                if chars[k] == '\\' {
                    k += 2;
                    continue;
                }
                if chars[k] == q {
                    break;
                }
                k += 1;
            }
            if let Some(occ) = route_occ_if_slash(chars, line_no, vstart, k.min(e)) {
                routes.push(occ);
            }
            prev_sig = '"';
            j = (k + 1).min(e).max(j + 1);
            continue;
        }
        // Identifier / keyword.
        if is_ident_start(c) {
            let ws = j;
            while j < e && is_ident_char(chars[j]) {
                j += 1;
            }
            let word: String = chars[ws..j].iter().collect();

            if word == "function" {
                // Declaration or named function expression. Skip an optional
                // generator star, then read the name.
                let mut p = j;
                skip_ws(chars, &mut p, e);
                if p < e && chars[p] == '*' {
                    p += 1;
                    skip_ws(chars, &mut p, e);
                }
                if p < e && is_ident_start(chars[p]) {
                    let ns = p;
                    while p < e && is_ident_char(chars[p]) {
                        p += 1;
                    }
                    // Emit the name only for a declaration; for an expression
                    // (`= function baz`, `(function baz`, …) the binding branch
                    // supplies the useful identifier.
                    if !matches!(prev_sig, '=' | '(' | ':' | ',') {
                        let name: String = chars[ns..p].iter().collect();
                        funcs.push(func_at(&name, line_no, ns));
                    }
                    j = p;
                }
                prev_sig = 'a';
                continue;
            }

            // Some other identifier: look ahead for a method or a
            // function-valued binding.
            let mut p = j;
            skip_ws(chars, &mut p, e);
            if p < e && chars[p] == '(' && !is_keyword(&word) {
                // `NAME ( … ) {` → a named method definition.
                if let Some(close) = match_paren_js(chars, p + 1, e) {
                    let mut q = close + 1;
                    skip_ws(chars, &mut q, e);
                    if q < e && chars[q] == '{' {
                        funcs.push(func_at(&word, line_no, ws));
                    }
                }
            } else if p < e && chars[p] == '=' && !(p + 1 < e && matches!(chars[p + 1], '=' | '>'))
            {
                // `NAME = <function value>` → a function expression bound to a
                // named identifier.
                let mut r = p + 1;
                skip_ws(chars, &mut r, e);
                if is_function_rhs(chars, r, e) {
                    funcs.push(func_at(&word, line_no, ws));
                }
            }
            prev_sig = 'a';
            continue;
        }

        // Any other char.
        if !c.is_whitespace() {
            prev_sig = c;
        }
        j += 1;
    }
}

/// Build a [`RouteOcc`] for the literal `[vstart, vend)` when its value begins
/// with `/`; otherwise `None`.
fn route_occ_if_slash(
    chars: &[char],
    line_no: &[u32],
    vstart: usize,
    vend: usize,
) -> Option<RouteOcc> {
    if vstart >= vend || vstart >= chars.len() || chars[vstart] != '/' {
        return None;
    }
    let value: String = chars[vstart..vend].iter().collect();
    let line_start = line_at(line_no, vstart);
    let line_end = line_at(line_no, vend.saturating_sub(1)).max(line_start);
    Some(RouteOcc {
        start: vstart,
        value,
        line_start,
        line_end,
    })
}

/// Build a [`Func`] spanning a single line (the definition's starting line),
/// which satisfies the `line_end >= line_start >= 1` validate invariant.
fn func_at(name: &str, line_no: &[u32], pos: usize) -> Func {
    let line = line_at(line_no, pos);
    Func {
        name: name.to_string(),
        line_start: line,
        line_end: line,
    }
}

/// True when the text at `r` is the right-hand side of a function-valued
/// binding: `function …`, `async function …`, `( … ) =>`, `async ( … ) =>`, or
/// a single-identifier arrow `x =>`.
fn is_function_rhs(chars: &[char], r: usize, e: usize) -> bool {
    let mut p = r;
    if keyword_at(chars, p, "async") {
        p += "async".len();
        skip_ws(chars, &mut p, e);
    }
    if keyword_at(chars, p, "function") {
        return true;
    }
    // Parenthesized arrow: `( … ) =>`.
    if p < e && chars[p] == '(' {
        if let Some(close) = match_paren_js(chars, p + 1, e) {
            let mut q = close + 1;
            skip_ws(chars, &mut q, e);
            return q + 1 < e && chars[q] == '=' && chars[q + 1] == '>';
        }
        return false;
    }
    // Single-identifier arrow: `x =>`.
    if p < e && is_ident_start(chars[p]) {
        let mut q = p;
        while q < e && is_ident_char(chars[q]) {
            q += 1;
        }
        skip_ws(chars, &mut q, e);
        return q + 1 < e && chars[q] == '=' && chars[q + 1] == '>';
    }
    false
}

// ===========================================================================
// Lexical primitives
// ===========================================================================

/// Reserved words that must not be mistaken for a method name in `NAME ( … ) {`.
fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "return"
            | "function"
            | "do"
            | "else"
            | "with"
            | "typeof"
            | "void"
            | "delete"
            | "new"
            | "in"
            | "of"
            | "instanceof"
            | "await"
            | "yield"
            | "case"
            | "super"
            | "var"
            | "let"
            | "const"
            | "class"
            | "throw"
            | "try"
            | "finally"
            | "import"
            | "export"
            | "default"
            | "extends"
            | "async"
    )
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn skip_ws(chars: &[char], pos: &mut usize, e: usize) {
    while *pos < e && chars[*pos].is_whitespace() {
        *pos += 1;
    }
}

/// True when `chars[pos..]` begins with the lowercase ASCII `needle`, compared
/// case-insensitively.
fn matches_ci(chars: &[char], pos: usize, needle: &str) -> bool {
    let nb: Vec<char> = needle.chars().collect();
    if pos + nb.len() > chars.len() {
        return false;
    }
    for (i, &nc) in nb.iter().enumerate() {
        if chars[pos + i].to_ascii_lowercase() != nc {
            return false;
        }
    }
    true
}

/// True when `chars[p..]` is exactly the (case-sensitive) keyword `kw` at a word
/// boundary (not immediately followed by another identifier char).
fn keyword_at(chars: &[char], p: usize, kw: &str) -> bool {
    let kb: Vec<char> = kw.chars().collect();
    if p + kb.len() > chars.len() {
        return false;
    }
    for (i, &kc) in kb.iter().enumerate() {
        if chars[p + i] != kc {
            return false;
        }
    }
    let after = p + kb.len();
    !(after < chars.len() && is_ident_char(chars[after]))
}

/// Find the index of the `)` matching the `(` whose body begins at `from`
/// (depth already 1), respecting string/template quoting. `None` when unbalanced.
fn match_paren_js(chars: &[char], from: usize, e: usize) -> Option<usize> {
    let mut depth = 1i32;
    let mut j = from;
    while j < e {
        let c = chars[j];
        match c {
            '\'' | '"' | '`' => {
                let q = c;
                j += 1;
                while j < e {
                    if chars[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if chars[j] == q {
                        break;
                    }
                    j += 1;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// 1-based line number of char index `pos`, clamped into the lookup table.
fn line_at(line_no: &[u32], pos: usize) -> u32 {
    if line_no.is_empty() {
        return 1;
    }
    let idx = pos.min(line_no.len() - 1);
    line_no[idx]
}

/// Build a 1-based line-number lookup: `line_numbers(chars)[i]` is the line of
/// `chars[i]`.
fn line_numbers(chars: &[char]) -> Vec<u32> {
    let mut out = Vec::with_capacity(chars.len());
    let mut cur = 1u32;
    for &c in chars {
        out.push(cur);
        if c == '\n' {
            cur += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_src(src: &str) -> ParseOutput {
        extract(ArtifactKind::Html, "web/index.html", src)
    }

    fn names_of(out: &ParseOutput, kind: SymbolKind) -> Vec<String> {
        out.symbols
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.name.clone())
            .collect()
    }

    #[test]
    fn attribute_routes_are_extracted() {
        let src = "<a href=\"/api/x\">x</a>\n<button hx-get='/state'>go</button>\n";
        let out = extract_src(src);
        let routes = names_of(&out, SymbolKind::Route);
        assert!(routes.contains(&"/api/x".to_string()), "{routes:?}");
        assert!(routes.contains(&"/state".to_string()), "{routes:?}");
        for s in &out.symbols {
            s.validate().expect("valid symbol");
            assert!(s.line_end >= s.line_start && s.line_start >= 1);
        }
    }

    #[test]
    fn embedded_js_fetch_route_is_extracted() {
        let src = "<script>\n  fetch('/api/world/state').then(r => r.json());\n</script>\n";
        let out = extract_src(src);
        let routes = names_of(&out, SymbolKind::Route);
        assert!(
            routes.contains(&"/api/world/state".to_string()),
            "{routes:?}"
        );
        let route = out
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Route)
            .unwrap();
        assert!(route
            .body_excerpt
            .as_deref()
            .unwrap()
            .contains("/api/world/state"));
    }

    #[test]
    fn distinct_routes_only() {
        let src =
            "<a href=\"/dup\">1</a><a href=\"/dup\">2</a>\n<script>fetch(\"/dup\");</script>\n";
        let out = extract_src(src);
        let dup: Vec<_> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Route && s.name == "/dup")
            .collect();
        assert_eq!(dup.len(), 1, "distinct routes only");
    }

    #[test]
    fn function_declaration_is_extracted() {
        let src = "<script>\nfunction submitCode() { return 1; }\n</script>\n";
        let out = extract_src(src);
        let funcs = names_of(&out, SymbolKind::Function);
        assert!(funcs.contains(&"submitCode".to_string()), "{funcs:?}");
        let f = out
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function)
            .unwrap();
        assert!(f.body_excerpt.as_deref().unwrap().contains("submitCode"));
        assert_eq!(f.line_start, 2);
    }

    #[test]
    fn named_function_expressions_and_arrows() {
        let src = "<script>\nconst foo = function() {};\nvar bar = function baz() {};\nconst quux = () => {};\nlet single = x => x + 1;\n</script>\n";
        let out = extract_src(src);
        let funcs = names_of(&out, SymbolKind::Function);
        assert!(funcs.contains(&"foo".to_string()), "{funcs:?}");
        assert!(funcs.contains(&"bar".to_string()), "{funcs:?}");
        assert!(funcs.contains(&"quux".to_string()), "{funcs:?}");
        assert!(funcs.contains(&"single".to_string()), "{funcs:?}");
        // The inner expression name `baz` is not double-emitted for the binding.
        assert!(!funcs.contains(&"baz".to_string()), "{funcs:?}");
    }

    #[test]
    fn named_methods_are_extracted() {
        let src = "<script>\nconst obj = {\n  greet() { return 'hi'; },\n  async load() { await go(); }\n};\n</script>\n";
        let out = extract_src(src);
        let funcs = names_of(&out, SymbolKind::Function);
        assert!(funcs.contains(&"greet".to_string()), "{funcs:?}");
        assert!(funcs.contains(&"load".to_string()), "{funcs:?}");
    }

    #[test]
    fn control_keywords_are_not_functions() {
        let src = "<script>\nif (x) { y(); }\nfor (;;) { z(); }\nwhile (a) { b(); }\n</script>\n";
        let out = extract_src(src);
        let funcs = names_of(&out, SymbolKind::Function);
        assert!(
            !funcs
                .iter()
                .any(|n| matches!(n.as_str(), "if" | "for" | "while")),
            "{funcs:?}"
        );
    }

    #[test]
    fn no_routes_no_functions_falls_back() {
        let src = "<html>\n<body><p>Just some prose, nothing routable.</p></body>\n</html>\n";
        let out = extract_src(src);
        assert!(out.fell_back);
        assert_eq!(out.symbols.len(), 1);
        assert_eq!(out.symbols[0].kind, SymbolKind::Module);
        assert_eq!(out.symbols[0].line_start, 1);
    }

    #[test]
    fn empty_source_emits_no_symbols() {
        let out = extract_src("   \n\t\n");
        assert!(out.symbols.is_empty());
        assert!(out.fell_back);
    }

    #[test]
    fn non_route_attribute_is_ignored() {
        let src = "<a href=\"https://example.com\">ext</a>\n";
        let out = extract_src(src);
        // Absolute URL does not begin with '/', so no route; falls back.
        assert!(names_of(&out, SymbolKind::Route).is_empty());
    }
}
