//! Unit/integration test for Requirement 4.4 (Task 5.3): an HTML artifact file
//! that yields no route literals and no named JS functions falls back to
//! **exactly one** whole-file textual `Module` symbol, and the batch continues.
//!
//! Feature: non-code-artifact-coverage
//!
//! ## What this pins
//!
//! Requirement 4.4: "IF an HTML Artifact_File yields no route literals and no
//! JavaScript function definitions, THEN THE HTML_Extractor SHALL emit exactly
//! one textual fallback symbol scoped to the whole file so the file remains
//! searchable, and SHALL continue processing the remaining files in the batch
//! without aborting."
//!
//! The relevant seam is `cognis_indexer::parser::artifact::extract_artifact`
//! (dispatching `ArtifactKind::Html` to `html::extract`), which routes
//! route-free / function-free HTML to the shared whole-file `textual_fallback`.
//! The fallback produces one `SymbolKind::Module` symbol spanning line 1..last,
//! with `fell_back == true`, that passes `Symbol::validate`.
//!
//! The pinned edge cases:
//! - plain prose HTML with no `/`-prefixed literals and no JS functions;
//! - an HTML document whose only link is an **absolute** URL
//!   (`href="https://..."`), which is NOT a route (it does not begin with `/`).
//!
//! This test drives the public extractor API directly for the core Req-4.4
//! assertion, and additionally drives the public `IndexerPipeline::index_repo`
//! API against a temp repo containing a route/function-free `.html` file
//! alongside a valid code file to prove the batch continues (both files indexed).

use std::path::PathBuf;

use cognis_core::{Config, SymbolKind};
use cognis_indexer::parser::artifact::extract_artifact;
use cognis_indexer::{ArtifactKind, IndexerPipeline};
use cognis_store::Database;

/// Assert that `source` (HTML with no routes and no functions) produces exactly
/// one whole-file textual `Module` symbol spanning line 1..last, with
/// `fell_back == true`, and that the symbol passes `Symbol::validate`.
fn assert_single_whole_file_fallback(source: &str) {
    let out = extract_artifact(ArtifactKind::Html, "web/index.html", source);

    // Exactly one symbol …
    assert_eq!(
        out.symbols.len(),
        1,
        "route/function-free HTML must yield exactly one fallback symbol, got {}: {:?}",
        out.symbols.len(),
        out.symbols
    );
    // … and it is the textual fallback.
    assert!(
        out.fell_back,
        "route/function-free HTML must be reported as a textual fallback (fell_back == true)"
    );

    let sym = &out.symbols[0];

    // Whole-file textual symbol is a `Module` (Req 4.4 fallback shape).
    assert_eq!(
        sym.kind,
        SymbolKind::Module,
        "the fallback symbol must be a Module symbol"
    );

    // Spans the whole file: line 1 to the last line.
    let last_line = source.lines().count().max(1) as u32;
    assert_eq!(sym.line_start, 1, "fallback must start at line 1");
    assert_eq!(
        sym.line_end, last_line,
        "fallback must span to the last line ({last_line})"
    );

    // Language label is the HTML artifact tag.
    assert_eq!(sym.language, "html");

    // The emitted symbol honors every `Symbol::validate` invariant, in
    // particular `line_end >= line_start >= 1`.
    sym.validate().expect("fallback symbol must be valid");
    assert!(sym.line_end >= sym.line_start && sym.line_start >= 1);

    // No Route or Function symbols leaked out of the fallback path.
    assert!(
        out.symbols
            .iter()
            .all(|s| s.kind != SymbolKind::Route && s.kind != SymbolKind::Function),
        "fallback path must emit no Route/Function symbols: {:?}",
        out.symbols
    );
}

/// Plain prose HTML — no `/`-prefixed literals, no JS — yields a single
/// whole-file textual fallback symbol.
#[test]
fn plain_prose_html_falls_back_to_single_whole_file_symbol() {
    assert_single_whole_file_fallback("<html><body><p>text</p></body></html>\n");
}

/// Multi-line prose HTML with headings and paragraphs but nothing routable
/// still yields exactly one whole-file fallback spanning every line.
#[test]
fn multiline_prose_html_yields_one_whole_file_symbol() {
    let src = "<html>\n\
               <head><title>About</title></head>\n\
               <body>\n\
                 <h1>Welcome</h1>\n\
                 <p>Just some prose, nothing routable here.</p>\n\
               </body>\n\
               </html>\n";
    assert_single_whole_file_fallback(src);
}

/// An HTML document whose only link is an **absolute** URL (`https://...`) has
/// no route (the value does not begin with `/`), so it falls back to a single
/// whole-file textual symbol.
#[test]
fn absolute_url_href_is_not_a_route_and_falls_back() {
    assert_single_whole_file_fallback(
        "<html><body><a href=\"https://example.com/page\">ext</a></body></html>\n",
    );
}

/// A document mixing an absolute URL and an empty (non-JS) `<script>` block —
/// still no route and no function — falls back to one whole-file symbol.
#[test]
fn absolute_url_with_empty_script_falls_back() {
    let src = "<html>\n\
               <body>\n\
                 <a href='https://cdn.example.com/lib.js'>lib</a>\n\
                 <script></script>\n\
               </body>\n\
               </html>\n";
    assert_single_whole_file_fallback(src);
}

/// A fresh, process-and-time unique temp directory so concurrent test binaries
/// never collide on the same repo root.
fn unique_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-html-no-route-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// End-to-end: a route/function-free `.html` artifact alongside a valid code
/// file. The batch must continue — both files are indexed, and the HTML file
/// contributes exactly one whole-file `Module` fallback symbol (Req 4.4).
#[test]
fn route_free_html_indexes_and_batch_continues() {
    let repo = unique_repo("e2e");

    // (a) A route/function-free HTML file — must fall back, not abort.
    std::fs::write(
        repo.join("about.html"),
        "<html><body><a href=\"https://example.com\">ext</a><p>prose</p></body></html>\n",
    )
    .unwrap();

    // (b) A valid code file that must still be indexed after the HTML file.
    std::fs::write(
        repo.join("app.py"),
        "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
    )
    .unwrap();

    let db = Database::open(":memory:").expect("open in-memory uckg");
    let mut pipeline = IndexerPipeline::new(db.clone(), Config::default());
    pipeline
        .index_repo(&repo, true)
        .expect("index_repo must not abort on a route/function-free HTML artifact");

    let symbols = db.list_symbols().expect("read symbols back");

    // The route/function-free HTML file contributes exactly one whole-file
    // Module fallback.
    let html_symbols: Vec<_> = symbols
        .iter()
        .filter(|s| s.file_path == "about.html")
        .collect();
    assert_eq!(
        html_symbols.len(),
        1,
        "route/function-free HTML must contribute exactly one fallback symbol, got {:?}",
        html_symbols
    );
    assert_eq!(html_symbols[0].kind, SymbolKind::Module);

    // The batch continued: the valid code file was indexed too.
    let app_symbols = symbols.iter().filter(|s| s.file_path == "app.py").count();
    assert!(
        app_symbols > 0,
        "valid code file app.py must be indexed alongside the route-free HTML artifact"
    );

    let _ = std::fs::remove_dir_all(&repo);
}
