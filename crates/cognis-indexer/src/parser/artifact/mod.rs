//! Artifact extractor stage — typed, answer-granularity symbol extraction for
//! non-code artifact files (YAML/TOML, SQL, HTML/embedded-JS, Markdown).
//!
//! [`extract_artifact`] is the artifact analogue of
//! [`parse_source`](crate::parser::parse_source): it takes the
//! [`ArtifactKind`](crate::pipeline::ArtifactKind) resolved by the walker's
//! second admission predicate and returns the **same** [`ParseOutput`] shape, so
//! the enrich/embed/write stages stay ignorant of file type (design §2, Req
//! 10.1).
//!
//! Like `parse_source`, the stage is **fault-tolerant per file**: when a
//! structured extractor yields nothing for non-empty source, the file falls back
//! to a single whole-file textual `Module` symbol via [`textual_fallback`] so the
//! file remains searchable and the batch is never aborted (Req 2.7, 3.6, 4.4,
//! 5.5). This mirrors the textual-fallback discipline of `parser::textual_fallback`.
//!
//! This is a **population-only** feature: every emitted symbol uses an existing
//! [`SymbolKind`] value and passes `Symbol::validate` — no schema migration.
//!
//! ## Scaffolding note
//!
//! The four concrete extractors land in later tasks (`yaml` → 3.1, `sql` → 4.1,
//! `html` → 5.1, `markdown` → 6.1). Until then every kind routes to the shared
//! textual fallback below. The dispatcher is deliberately written as one match
//! arm per kind so each concrete extractor plugs into its own arm without
//! touching the others, and the shared helpers (`mk_symbol`, `make_symbol_id`,
//! `content_hash`, `body_excerpt`, `module_from_path`, and this
//! `textual_fallback`) are re-exported for those extractor modules to reuse.

use cognis_core::{ParseStatus, Symbol, SymbolKind};

use crate::parser::{content_hash, make_symbol_id, ParseOutput};
use crate::pipeline::ArtifactKind;

use super::support::{body_excerpt, module_from_path};

mod html;
mod markdown;
mod sql;
mod yaml;

// Shared parser-stage constructor reused by the concrete extractors (yaml/sql/
// html/markdown) as they land. Re-exported here so those modules reference the
// artifact-family helper surface via `super::` rather than reaching across the
// parser tree. Unused until the concrete extractors are implemented.
#[allow(unused_imports)]
pub(crate) use crate::parser::mk_symbol;

/// Extract typed artifact symbols from `source` for `file_path`, dispatching on
/// the resolved [`ArtifactKind`]. Fault-tolerant: never panics, never aborts; on
/// empty structured extraction it produces a whole-file textual fallback so the
/// file stays searchable and the batch continues.
///
/// `file_path` should be repo-relative with forward slashes, exactly as
/// [`parse_source`](crate::parser::parse_source) expects.
pub fn extract_artifact(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    match kind {
        // Concrete extractors land in later tasks; until then each kind routes
        // to the shared whole-file textual fallback (Req 2.7/3.6/4.4/5.5).
        ArtifactKind::Yaml | ArtifactKind::Toml => {
            // Structured YAML/TOML leaf extraction (Task 3.1); yaml::extract
            // internally falls back to the shared whole-file textual symbol when
            // it yields 0 or > 10,000 leaves (Req 2.7).
            yaml::extract(kind, file_path, source)
        }
        ArtifactKind::Sql => {
            // Structured SQL DDL extraction (Task 4.1); sql::extract internally
            // falls back to the shared whole-file textual symbol when no
            // parseable `CREATE TABLE (...)` DDL is found (Req 3.6).
            sql::extract(kind, file_path, source)
        }
        ArtifactKind::Html => {
            // Structured HTML/embedded-JS extraction (Task 5.1); html::extract
            // internally falls back to the shared whole-file textual symbol when
            // it finds no route literals and no JS functions (Req 4.4).
            html::extract(kind, file_path, source)
        }
        ArtifactKind::Markdown => {
            // Structured Markdown heading-section extraction (Task 6.1);
            // markdown::extract internally falls back to the shared whole-file
            // textual symbol when the document contains no headings (Req 5.5).
            markdown::extract(kind, file_path, source)
        }
    }
}

/// Lower-case language label / id-prefix for an [`ArtifactKind`], mirroring the
/// `Language::label` / `Language::lang` convention of the code parsers.
fn kind_label(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Yaml => "yaml",
        ArtifactKind::Toml => "toml",
        ArtifactKind::Sql => "sql",
        ArtifactKind::Html => "html",
        ArtifactKind::Markdown => "markdown",
    }
}

/// Build a single coarse whole-file `Module` symbol when structured extraction
/// yields nothing, so an admitted artifact file remains searchable and the batch
/// continues (Req 2.7, 3.6, 4.4, 5.5). Same shape as
/// [`parser::textual_fallback`](crate::parser) but specialized for artifacts: the
/// language label is the artifact kind's tag and empty/whitespace source yields
/// no symbol (the pipeline skips zero-symbol files without aborting).
pub(crate) fn textual_fallback(kind: ArtifactKind, file_path: &str, source: &str) -> ParseOutput {
    let label = kind_label(kind);
    if source.trim().is_empty() {
        // Empty/whitespace artifact: nothing to index, but not a failure.
        return ParseOutput {
            symbols: Vec::new(),
            status: ParseStatus::Ok,
            language: Some(label),
            fell_back: true,
        };
    }
    let module = module_from_path(file_path);
    let name = module.rsplit('/').next().unwrap_or(&module).to_string();
    let line_count = source.lines().count().max(1) as u32;
    let qualified_name = format!("{label}:{file_path}:{name}");
    let symbol = Symbol {
        id: make_symbol_id(label, file_path, &name, source),
        kind: SymbolKind::Module,
        name,
        qualified_name,
        language: label.to_string(),
        module,
        file_path: file_path.to_string(),
        line_start: 1,
        line_end: line_count,
        signature: None,
        docstring: None,
        content_hash: content_hash(source),
        body_excerpt: Some(body_excerpt(source)),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    };
    ParseOutput {
        symbols: vec![symbol],
        status: ParseStatus::Ok,
        language: Some(label),
        fell_back: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_core::SymbolKind;

    /// Kinds without a concrete extractor yet route to the whole-file textual
    /// fallback, which emits exactly one valid `Module` symbol spanning line
    /// 1..last line. (YAML/TOML now have a structured extractor — see below.)
    #[test]
    fn dispatch_routes_every_kind_to_valid_fallback() {
        let src = "alpha one\nbeta two\ngamma three\n";
        for kind in [
            ArtifactKind::Sql,
            ArtifactKind::Html,
            ArtifactKind::Markdown,
        ] {
            let out = extract_artifact(kind, "config/app.txt", src);
            assert_eq!(out.symbols.len(), 1, "one fallback symbol for {kind:?}");
            let sym = &out.symbols[0];
            assert_eq!(sym.kind, SymbolKind::Module);
            assert_eq!(sym.line_start, 1);
            assert_eq!(sym.line_end, src.lines().count() as u32);
            assert!(sym.line_end >= sym.line_start && sym.line_start >= 1);
            sym.validate().expect("fallback symbol must be valid");
            assert!(out.fell_back);
        }
    }

    /// YAML/TOML dispatch to the structured leaf-key extractor (Task 3.1),
    /// emitting one valid `Var` symbol per leaf rather than a whole-file blob.
    #[test]
    fn dispatch_routes_yaml_toml_to_structured_extractor() {
        let src = "alpha: 1\nbeta:\n  gamma: two\n";
        let out = extract_artifact(ArtifactKind::Yaml, "config/app.yaml", src);
        assert!(
            out.symbols.len() >= 2,
            "structured leaves: {:?}",
            out.symbols
        );
        assert!(!out.fell_back);
        for sym in &out.symbols {
            assert_eq!(sym.kind, SymbolKind::Var);
            sym.validate().expect("structured symbol must be valid");
        }
    }

    /// Empty / whitespace-only source produces no symbols so the pipeline skips
    /// the file without aborting the batch.
    #[test]
    fn empty_source_emits_no_symbols() {
        let out = extract_artifact(ArtifactKind::Markdown, "docs/EMPTY.md", "   \n\t\n");
        assert!(out.symbols.is_empty());
        assert!(out.fell_back);
    }

    /// The fallback symbol's searchable text (`body_excerpt`) carries the file
    /// content so the artifact is retrievable.
    #[test]
    fn fallback_carries_searchable_text() {
        let src = "SELECT * FROM widgets;\n";
        let out = extract_artifact(ArtifactKind::Sql, "db/query.sql", src);
        let sym = &out.symbols[0];
        assert_eq!(sym.language, "sql");
        assert!(sym
            .body_excerpt
            .as_deref()
            .unwrap_or("")
            .contains("widgets"));
    }
}
