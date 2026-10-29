//! SQL DDL artifact extractor (Task 4.1, Req 3).
//!
//! Scans a SQL file for `CREATE TABLE [IF NOT EXISTS] name (...)` statements and
//! emits **answer-granularity** symbols — never a whole-file blob (Req 3.4):
//!
//! - one [`SymbolKind::Class`] per declared table, `name` = the table name, with
//!   searchable text that includes the table name (Req 3.1);
//! - one [`SymbolKind::Var`] per declared column, with searchable text that
//!   includes the owning **table name** and the **column name** (Req 3.2, 3.3).
//!
//! Each emitted symbol's `line_start`/`line_end` bound its table or column
//! declaration span (Req 3.5), honoring the `line_end >= line_start >= 1`
//! invariant of `Symbol::validate`.
//!
//! ## Tolerant hand-rolled scan
//!
//! No SQL parser is available to the offline workspace, so this module hand-rolls
//! a small, dialect-tolerant DDL scanner over the mainstream `CREATE TABLE`
//! surface. It:
//! - strips `--` line comments and `/* … */` block comments (preserving line
//!   numbers) while respecting `'…'`, `"…"`, `` `…` ``, and `[…]` string/identifier
//!   quoting;
//! - accepts quoted / backticked / bracketed / schema-qualified table names;
//! - splits the parenthesised body at top-level commas into column definitions;
//! - skips table-level constraints (`PRIMARY KEY`, `FOREIGN KEY`, `CONSTRAINT`,
//!   `UNIQUE`, `CHECK`, `KEY`, `INDEX`, …).
//!
//! Anything it cannot resolve into at least one table routes to the shared
//! whole-file [`textual_fallback`](super::textual_fallback) so the file stays
//! searchable and the batch continues (Req 3.6) — the same fault-tolerant
//! discipline as `parse_source`.

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use super::textual_fallback;
use crate::parser::support::module_from_path;
use crate::parser::{content_hash, make_symbol_id, ParseOutput};
use crate::pipeline::ArtifactKind;

/// Language / id-prefix tag for SQL artifact symbols.
const LABEL: &str = "sql";

/// Max length (in characters) of an emitted symbol's searchable text.
const TEXT_LIMIT: usize = 4096;

/// One parsed column: cleaned name, its full (comment-stripped) definition text,
/// and 1-based source line span.
struct Column {
    name: String,
    def: String,
    line_start: u32,
    line_end: u32,
}

/// One parsed `CREATE TABLE`: table name, its columns, and the 1-based source
/// line span from the `CREATE` keyword to the closing paren.
struct Table {
    name: String,
    columns: Vec<Column>,
    line_start: u32,
    line_end: u32,
}

/// Extract typed SQL table/column symbols from `source`.
///
/// Emits one [`SymbolKind::Class`] per table and one [`SymbolKind::Var`] per
/// column. Falls back to a single whole-file textual symbol when no parseable
/// `CREATE TABLE (...)` DDL is found (Req 3.6).
pub(crate) fn extract(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    let tables = parse_ddl(source);

    // Req 3.6: no parseable DDL routes to the shared whole-file textual fallback
    // so the file remains searchable and the batch is never aborted.
    if tables.is_empty() {
        return textual_fallback(kind, file_path, source);
    }

    let module = module_from_path(file_path);
    let mut symbols: Vec<Symbol> = Vec::new();
    for table in tables {
        symbols.push(build_table_symbol(file_path, &module, &table));
        for column in &table.columns {
            symbols.push(build_column_symbol(file_path, &module, &table.name, column));
        }
    }

    ParseOutput {
        symbols,
        status: ParseStatus::Ok,
        language: Some(LABEL),
        fell_back: false,
    }
}

/// Truncate searchable text to [`TEXT_LIMIT`] characters (not bytes).
fn truncate_text(text: String) -> String {
    if text.chars().count() > TEXT_LIMIT {
        text.chars().take(TEXT_LIMIT).collect()
    } else {
        text
    }
}

/// Build the `Class` symbol for a table. Searchable text includes the table name
/// (Req 3.1) plus its column names to aid retrieval.
fn build_table_symbol(file_path: &str, module: &str, table: &Table) -> Symbol {
    let cols: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    let text = if cols.is_empty() {
        format!("table {}", table.name)
    } else {
        format!("table {} ({})", table.name, cols.join(", "))
    };
    let text = truncate_text(text);
    let line_start = table.line_start.max(1);
    let line_end = table.line_end.max(line_start);
    let qualified_name = format!("{LABEL}:{file_path}:{}", table.name);
    Symbol {
        id: make_symbol_id(LABEL, file_path, &table.name, &text),
        kind: SymbolKind::Class,
        name: table.name.clone(),
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

/// Build a `Var` symbol for a column. Searchable text includes the owning table
/// name and the column name (Req 3.3) plus the full column definition.
fn build_column_symbol(file_path: &str, module: &str, table: &str, column: &Column) -> Symbol {
    let text = truncate_text(format!("{} {} {}", table, column.name, column.def));
    let line_start = column.line_start.max(1);
    let line_end = column.line_end.max(line_start);
    let qualified = format!("{table}.{}", column.name);
    let qualified_name = format!("{LABEL}:{file_path}:{qualified}");
    Symbol {
        id: make_symbol_id(LABEL, file_path, &qualified, &text),
        kind: SymbolKind::Var,
        name: column.name.clone(),
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
// DDL scanner
// ===========================================================================

/// Parse every `CREATE TABLE (...)` statement in `source` into [`Table`]s.
fn parse_ddl(source: &str) -> Vec<Table> {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    let cleaned = strip_comments(&chars);
    let line_no = line_numbers(&chars);

    let mut tables = Vec::new();
    let mut pos = 0usize;
    while pos < n {
        skip_ws(&cleaned, &mut pos);
        if pos >= n {
            break;
        }
        let word_start = pos;
        if let Some((word, end)) = read_word_at(&cleaned, pos) {
            pos = end;
            if word.eq_ignore_ascii_case("create") {
                try_parse_create_table(&cleaned, &mut pos, &line_no, word_start, &mut tables);
            }
        } else {
            // Punctuation / other; advance one char to guarantee progress.
            pos += 1;
        }
    }
    tables
}

/// Attempt to parse a `CREATE TABLE` statement. `*pos` points just past the
/// `CREATE` keyword; `create_start` is the char index of `CREATE` (for the line
/// span). On success pushes one [`Table`]; otherwise leaves `*pos` advanced past
/// whatever it consumed and pushes nothing.
fn try_parse_create_table(
    cl: &[char],
    pos: &mut usize,
    line_no: &[u32],
    create_start: usize,
    tables: &mut Vec<Table>,
) {
    let n = cl.len();

    // Skip optional modifiers (TEMP/TEMPORARY/UNLOGGED/GLOBAL/LOCAL/VIRTUAL)
    // until the TABLE keyword. Anything else (INDEX/VIEW/DATABASE/…) is not a
    // table declaration.
    loop {
        skip_ws(cl, pos);
        let Some((word, end)) = read_word_at(cl, *pos) else {
            return;
        };
        if word.eq_ignore_ascii_case("table") {
            *pos = end;
            break;
        }
        if matches!(
            word.to_ascii_lowercase().as_str(),
            "temp" | "temporary" | "unlogged" | "global" | "local" | "virtual"
        ) {
            *pos = end;
            continue;
        }
        return;
    }

    // Optional `IF NOT EXISTS`.
    skip_ifnotexists(cl, pos);

    // Table name (possibly quoted / bracketed / backticked / schema-qualified).
    let Some(table_name) = read_qualified_name(cl, pos) else {
        return;
    };

    // Require a parenthesised column list. Forms without one (e.g.
    // `CREATE TABLE x AS SELECT …`) are not answer-granularity DDL and are
    // skipped.
    skip_ws(cl, pos);
    if *pos >= n || cl[*pos] != '(' {
        return;
    }
    let body_start = *pos + 1;
    let Some(close_idx) = match_paren(cl, body_start) else {
        return;
    };

    let columns = parse_columns(cl, body_start, close_idx, line_no);

    let line_start = line_no.get(create_start).copied().unwrap_or(1);
    let line_end = line_no.get(close_idx).copied().unwrap_or(line_start);
    tables.push(Table {
        name: table_name,
        columns,
        line_start,
        line_end,
    });
    *pos = close_idx + 1;
}

/// Split the parenthesised body `[body_start, body_end)` at top-level commas and
/// parse each segment into a [`Column`], skipping table-level constraints.
fn parse_columns(cl: &[char], body_start: usize, body_end: usize, line_no: &[u32]) -> Vec<Column> {
    let mut columns = Vec::new();
    for (seg_start, seg_end) in split_top_level(cl, body_start, body_end) {
        if let Some(col) = parse_column_segment(cl, seg_start, seg_end, line_no) {
            columns.push(col);
        }
    }
    columns
}

/// Parse one comma-delimited segment into a [`Column`], or `None` when the
/// segment is a table-level constraint or is empty.
fn parse_column_segment(cl: &[char], start: usize, end: usize, line_no: &[u32]) -> Option<Column> {
    // Trim to the first/last non-whitespace char.
    let mut fnw = start;
    while fnw < end && cl[fnw].is_whitespace() {
        fnw += 1;
    }
    if fnw >= end {
        return None;
    }
    let mut lnw = end - 1;
    while lnw > fnw && cl[lnw].is_whitespace() {
        lnw -= 1;
    }

    // First token: a quoted identifier is always a column; a bare word may be a
    // table-level constraint keyword to skip.
    if !is_quote(cl[fnw]) {
        if let Some((word, _)) = read_word_at(cl, fnw) {
            if is_constraint_keyword(&word) {
                return None;
            }
        }
    }

    let mut name_pos = fnw;
    let name = read_name_part(cl, &mut name_pos)?;
    if name.is_empty() {
        return None;
    }

    let def: String = cl[fnw..=lnw].iter().collect();
    let def = def.split_whitespace().collect::<Vec<_>>().join(" ");
    let line_start = line_no.get(fnw).copied().unwrap_or(1);
    let line_end = line_no.get(lnw).copied().unwrap_or(line_start);
    Some(Column {
        name,
        def,
        line_start,
        line_end,
    })
}

/// True when `word` (any case) is a table-level constraint keyword, so its
/// segment is skipped rather than treated as a column (Req 3: skip
/// PRIMARY/FOREIGN/CONSTRAINT/UNIQUE/CHECK/KEY/INDEX/…).
fn is_constraint_keyword(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "primary"
            | "foreign"
            | "constraint"
            | "unique"
            | "check"
            | "key"
            | "index"
            | "fulltext"
            | "spatial"
            | "exclude"
    )
}

/// Return the char indices of the segment boundaries `(start, end)` obtained by
/// splitting `[body_start, body_end)` at commas that sit at paren-depth 0 and
/// outside any quote.
fn split_top_level(cl: &[char], body_start: usize, body_end: usize) -> Vec<(usize, usize)> {
    let mut segs = Vec::new();
    let mut seg_start = body_start;
    let mut depth = 0i32;
    let mut q = QuoteState::default();
    let mut j = body_start;
    while j < body_end {
        let c = cl[j];
        if q.step(c) {
            j += 1;
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                segs.push((seg_start, j));
                seg_start = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    segs.push((seg_start, body_end));
    segs
}

/// Find the index of the `)` that closes the `(` whose body begins at
/// `body_start` (depth already 1). Respects quotes. `None` when unbalanced.
fn match_paren(cl: &[char], body_start: usize) -> Option<usize> {
    let n = cl.len();
    let mut depth = 1i32;
    let mut q = QuoteState::default();
    let mut j = body_start;
    while j < n {
        let c = cl[j];
        if q.step(c) {
            j += 1;
            continue;
        }
        match c {
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

// ===========================================================================
// Lexical primitives
// ===========================================================================

/// Tracks whether the scanner is inside a `'…'`, `"…"`, `` `…` ``, or `[…]`
/// quoted region.
#[derive(Default)]
struct QuoteState {
    single: bool,
    double: bool,
    back: bool,
    bracket: bool,
}

impl QuoteState {
    /// Advance the quote state by one char. Returns `true` when `c` was consumed
    /// as (or toggled) quote state and the caller should treat it as opaque.
    fn step(&mut self, c: char) -> bool {
        if self.single {
            if c == '\'' {
                self.single = false;
            }
            return true;
        }
        if self.double {
            if c == '"' {
                self.double = false;
            }
            return true;
        }
        if self.back {
            if c == '`' {
                self.back = false;
            }
            return true;
        }
        if self.bracket {
            if c == ']' {
                self.bracket = false;
            }
            return true;
        }
        match c {
            '\'' => {
                self.single = true;
                true
            }
            '"' => {
                self.double = true;
                true
            }
            '`' => {
                self.back = true;
                true
            }
            '[' => {
                self.bracket = true;
                true
            }
            _ => false,
        }
    }
}

fn is_quote(c: char) -> bool {
    matches!(c, '\'' | '"' | '`' | '[')
}

/// A char that may appear in a bare SQL identifier.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

fn skip_ws(cl: &[char], pos: &mut usize) {
    while *pos < cl.len() && cl[*pos].is_whitespace() {
        *pos += 1;
    }
}

/// Read a bare identifier word starting at `pos` (assumed non-whitespace).
/// Returns the original-case word and the index just past it, or `None` when the
/// char at `pos` is not an identifier char.
fn read_word_at(cl: &[char], pos: usize) -> Option<(String, usize)> {
    if pos >= cl.len() || !is_ident_char(cl[pos]) {
        return None;
    }
    let mut end = pos;
    while end < cl.len() && is_ident_char(cl[end]) {
        end += 1;
    }
    Some((cl[pos..end].iter().collect(), end))
}

/// Consume an optional `IF NOT EXISTS` clause at `*pos`, restoring `*pos` when
/// the three keywords are not all present.
fn skip_ifnotexists(cl: &[char], pos: &mut usize) {
    let save = *pos;
    for kw in ["if", "not", "exists"] {
        skip_ws(cl, pos);
        match read_word_at(cl, *pos) {
            Some((w, end)) if w.eq_ignore_ascii_case(kw) => *pos = end,
            _ => {
                *pos = save;
                return;
            }
        }
    }
}

/// Read one name part at `*pos`: a quoted (`"…"`), backticked (`` `…` ``),
/// bracketed (`[…]`) identifier, or a bare identifier. Advances `*pos` past it
/// and returns the unquoted name.
fn read_name_part(cl: &[char], pos: &mut usize) -> Option<String> {
    skip_ws(cl, pos);
    if *pos >= cl.len() {
        return None;
    }
    let c = cl[*pos];
    let (open, close) = match c {
        '"' => ('"', '"'),
        '`' => ('`', '`'),
        '[' => ('[', ']'),
        _ => {
            // Bare identifier.
            let (word, end) = read_word_at(cl, *pos)?;
            *pos = end;
            return Some(word);
        }
    };
    debug_assert_eq!(cl[*pos], open);
    *pos += 1; // consume opening delimiter
    let start = *pos;
    while *pos < cl.len() && cl[*pos] != close {
        *pos += 1;
    }
    let name: String = cl[start..*pos].iter().collect();
    if *pos < cl.len() {
        *pos += 1; // consume closing delimiter
    }
    Some(name)
}

/// Read a possibly schema-qualified name (`schema.name`, `a.b.c`, mixing quoting)
/// at `*pos`, returning the **last** component (the table name).
fn read_qualified_name(cl: &[char], pos: &mut usize) -> Option<String> {
    let mut last = read_name_part(cl, pos)?;
    loop {
        let save = *pos;
        skip_ws(cl, pos);
        if *pos < cl.len() && cl[*pos] == '.' {
            *pos += 1;
            match read_name_part(cl, pos) {
                Some(part) => last = part,
                None => {
                    *pos = save;
                    break;
                }
            }
        } else {
            *pos = save;
            break;
        }
    }
    if last.is_empty() {
        None
    } else {
        Some(last)
    }
}

/// Replace `--` line comments and `/* … */` block comments with spaces,
/// preserving newline positions (so line numbers stay accurate) and quoted
/// regions. The returned vector is index-aligned 1:1 with `chars`.
fn strip_comments(chars: &[char]) -> Vec<char> {
    let n = chars.len();
    let mut out: Vec<char> = Vec::with_capacity(n);
    let mut i = 0usize;
    let mut q = QuoteState::default();
    while i < n {
        let c = chars[i];
        // Inside a quoted region: copy verbatim.
        if q.single || q.double || q.back || q.bracket {
            q.step(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            while i < n && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            out.push(' ');
            out.push(' ');
            i += 2;
            while i < n && !(chars[i] == '*' && i + 1 < n && chars[i + 1] == '/') {
                out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            if i < n {
                out.push(' ');
                out.push(' ');
                i += 2;
            }
            continue;
        }
        // Not a comment: track quote openings and copy.
        q.step(c);
        out.push(c);
        i += 1;
    }
    out
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
        extract(ArtifactKind::Sql, "db/schema.sql", src)
    }

    #[test]
    fn create_table_emits_class_and_columns() {
        let src = "CREATE TABLE users (\n  id INTEGER PRIMARY KEY,\n  email TEXT NOT NULL,\n  age INT\n);\n";
        let out = extract_src(src);
        assert!(!out.fell_back);
        let table = out
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class)
            .expect("a table Class symbol");
        assert_eq!(table.name, "users");
        assert!(table.body_excerpt.as_deref().unwrap().contains("users"));

        let cols: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .map(|s| s.name.as_str())
            .collect();
        assert!(cols.contains(&"id"), "{cols:?}");
        assert!(cols.contains(&"email"), "{cols:?}");
        assert!(cols.contains(&"age"), "{cols:?}");

        for s in &out.symbols {
            s.validate().expect("valid symbol");
            assert!(s.line_end >= s.line_start && s.line_start >= 1);
        }
    }

    #[test]
    fn column_text_includes_table_and_column() {
        let src = "CREATE TABLE orders (order_id BIGINT, total NUMERIC);\n";
        let out = extract_src(src);
        let col = out
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Var && s.name == "total")
            .unwrap();
        let text = col.body_excerpt.as_deref().unwrap();
        assert!(text.contains("orders"), "{text}");
        assert!(text.contains("total"), "{text}");
    }

    #[test]
    fn table_level_constraints_are_skipped() {
        let src = "CREATE TABLE t (\n  id INT,\n  name TEXT,\n  PRIMARY KEY (id),\n  FOREIGN KEY (name) REFERENCES other(x),\n  CONSTRAINT uq UNIQUE (name),\n  UNIQUE (id),\n  CHECK (id > 0)\n);\n";
        let out = extract_src(src);
        let cols: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(cols, vec!["id", "name"], "only real columns: {cols:?}");
    }

    #[test]
    fn if_not_exists_and_schema_qualified_and_quoted_names() {
        let src = "CREATE TABLE IF NOT EXISTS myschema.\"Users\" (\n  `id` INT,\n  [full name] TEXT\n);\n";
        let out = extract_src(src);
        let table = out
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class)
            .unwrap();
        assert_eq!(table.name, "Users");
        let cols: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .map(|s| s.name.as_str())
            .collect();
        assert!(cols.contains(&"id"), "{cols:?}");
        assert!(cols.contains(&"full name"), "{cols:?}");
    }

    #[test]
    fn line_spans_are_real() {
        let src = "CREATE TABLE t (\n  a INT,\n  b INT\n);\n";
        let out = extract_src(src);
        let a = out.symbols.iter().find(|s| s.name == "a").unwrap();
        assert_eq!(a.line_start, 2);
        let b = out.symbols.iter().find(|s| s.name == "b").unwrap();
        assert_eq!(b.line_start, 3);
    }

    #[test]
    fn comments_do_not_break_parsing() {
        let src = "-- a comment with CREATE TABLE fake (x)\nCREATE TABLE real ( /* inline */ id INT, -- trailing\n name TEXT );\n";
        let out = extract_src(src);
        let tables: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            tables,
            vec!["real"],
            "comment CREATE TABLE ignored: {tables:?}"
        );
    }

    #[test]
    fn no_ddl_falls_back_to_single_whole_file_symbol() {
        let src = "SELECT * FROM users WHERE id = 1;\nUPDATE users SET x = 2;\n";
        let out = extract_src(src);
        assert!(out.fell_back);
        assert_eq!(out.symbols.len(), 1);
        assert_eq!(out.symbols[0].kind, SymbolKind::Module);
        assert_eq!(out.symbols[0].line_start, 1);
    }

    #[test]
    fn paren_in_string_default_does_not_break_columns() {
        let src = "CREATE TABLE t (\n  note TEXT DEFAULT '(none)',\n  qty INT\n);\n";
        let out = extract_src(src);
        let cols: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Var)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(cols, vec!["note", "qty"], "{cols:?}");
    }

    #[test]
    fn multiple_tables_all_emitted() {
        let src = "CREATE TABLE a (x INT);\nCREATE TABLE b (y INT);\n";
        let out = extract_src(src);
        let tables: Vec<&str> = out
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .map(|s| s.name.as_str())
            .collect();
        assert!(tables.contains(&"a"));
        assert!(tables.contains(&"b"));
    }

    #[test]
    fn create_index_is_not_a_table() {
        let src = "CREATE INDEX idx ON users (email);\n";
        let out = extract_src(src);
        // No CREATE TABLE → whole-file fallback.
        assert!(out.fell_back);
    }
}
