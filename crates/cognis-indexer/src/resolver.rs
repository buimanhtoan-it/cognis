//! Resolver stage of the indexer pipeline (Task 8.2).
//!
//! Resolves call / inheritance edges between the symbols of a parse batch and
//! emits [`cognis_core::Edge`]s for the Writer to persist. Rust mirror of the
//! Python `cognis_indexer.resolver` package (`heuristic.py`, `oop.py`,
//! `pipeline.py`) so the two engines agree on symbol/edge counts (Requirement
//! 9.2 parity).
//!
//! Two resolvers run over every batch and their results are merged keeping the
//! highest confidence per `(src_id, dst_id, kind)`:
//!
//! * [`HeuristicResolver`] — three-phase `calls` resolution by scanning each
//!   symbol's `body_excerpt` for identifiers that match another symbol's name:
//!   same-file (confidence `1.0`), cross-module same-language (`0.6`), fuzzy
//!   prefix (`0.4`).
//! * [`OopResolver`] — `inherits` / `implements` edges parsed from the C# /
//!   Java type-declaration header (`: Base, IFoo`, `extends`/`implements`).
//!
//! `meta.dst_missing` convention: the resolver only ever emits an edge when
//! **both** endpoints are symbols present in the batch, so it never produces a
//! `dst_missing` edge. That flag is owned by the delete path
//! (`SymbolWriter::delete_symbol`): when a symbol is removed its inbound edges
//! are kept and flagged `meta.dst_missing = true` rather than erased.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
use regex::Regex;

// --- Confidence tiers (mirror `heuristic.py` / `oop.py`) -------------------

/// Same-file exact name match — unambiguous within module scope.
const CONF_SAME_FILE: f64 = 1.0;
/// Cross-module exact name match, same language.
const CONF_CROSS_MODULE: f64 = 0.6;
/// OOP base resolves to a single in-repo type.
const CONF_OOP_UNIQUE: f64 = 0.9;
/// OOP base name resolves to more than one in-repo type.
const CONF_OOP_AMBIGUOUS: f64 = 0.5;
/// Exact, byte-for-byte route-string match between an HTML/JS `Route` literal
/// and a code handler that declares the same literal (Req 6.4). The strongest
/// join in the graph — an exact string identity, no normalization — so it is
/// fixed at the ceiling `1.0` and is strictly above every normalized/heuristic
/// tier (SQL edges in `[0.50, 0.80]`, cross-module calls `0.6`, …).
const CONF_ROUTES_TO: f64 = 1.0;

/// Fixed confidence of a config `Reads` edge joining a config-key `Var`/`Const`
/// literal to a code reader site that references the same key string (Req 7.6).
///
/// This is a *heuristic string-identity* join — a code reader that mentions the
/// key string as a literal (`os.Getenv("PORT")`, `viper.GetString("db.host")`) is
/// very likely, but not provably, the consumer of that config key — so it sits
/// below the exact route-string identity ceiling (`CONF_ROUTES_TO = 1.0`) yet
/// clearly above the ambiguity floor. It is a **pre-declared constant**, chosen
/// once (never tuned to a benchmark sample) and satisfies the `[0.0, 1.0]` bound
/// required by `Edge::validate`.
const CONF_CONFIG_READS: f64 = 0.7;

/// Edges below this confidence are flagged `ambiguous` (design *Resolved Open
/// Questions → "Edge confidence threshold"*).
const AMBIGUOUS_THRESHOLD: f64 = 0.6;

/// Max number of *cross-file* same-language definitions a called name may
/// resolve to before we treat it as a common/ambiguous name and emit **no**
/// cross-module edges for it (same-file edges are still kept). This is the
/// single most important guard against edge explosion: a call to a ubiquitous
/// method name (`get`, `text`, `toString`, `run`) would otherwise fan out to
/// every same-named definition in the repo. Same-file resolution is unaffected.
const CROSS_FANOUT_CAP: usize = 8;

/// An edge produced by a resolver, before conversion to a persisted
/// [`Edge`]. Mirrors `cognis_indexer.resolver.base.ResolvedEdge`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEdge {
    /// Calling / importing / deriving symbol id.
    pub src_id: String,
    /// Called / imported / base symbol id.
    pub dst_id: String,
    /// Edge type (`calls`, `inherits`, `implements`).
    pub kind: EdgeKind,
    /// Resolution confidence in `[0.0, 1.0]`.
    pub confidence: f64,
    /// `true` when `confidence < 0.6` — persisted in `meta.ambiguous`.
    pub ambiguous: bool,
}

impl ResolvedEdge {
    fn new(src_id: String, dst_id: String, kind: EdgeKind, confidence: f64) -> Self {
        ResolvedEdge {
            src_id,
            dst_id,
            kind,
            confidence,
            ambiguous: confidence < AMBIGUOUS_THRESHOLD,
        }
    }
}

/// `\b([A-Za-z_][A-Za-z0-9_]*)\s*\(` — a **call site**: an identifier
/// immediately followed by `(`. Scanning call sites (not every identifier)
/// is the precision lever: it excludes types, variable reads, field accesses,
/// and keywords that are never invoked, so a `calls` edge is only proposed
/// where the body actually calls something by that name. Works across
/// languages (method-chain `.foo(` still captures `foo`).
fn call_site_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap())
}

/// Control-flow / operator keywords that are syntactically followed by `(` in
/// C-family and other languages but are never call targets. Skipped so a symbol
/// coincidentally named like a keyword can't be linked from every `if (`/`for
/// (` in the repo. (Most would fail the name-match anyway; this is defensive.)
fn is_stopword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "foreach"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "catch"
            | "try"
            | "finally"
            | "return"
            | "throw"
            | "throws"
            | "new"
            | "delete"
            | "sizeof"
            | "typeof"
            | "instanceof"
            | "await"
            | "yield"
            | "assert"
            | "lock"
            | "using"
            | "when"
            | "with"
            | "match"
            | "synchronized"
            | "super"
            | "this"
            | "self"
    )
}

/// Whether a symbol can be a `calls` **source**. Only callable symbols invoke
/// others; a `Class`/`Interface` symbol's `body_excerpt` spans its whole body
/// (including its methods), so treating it as a caller double-counts every call
/// its methods make and links the type to everything. Restricting sources to
/// callables removes that whole class of spurious edges.
fn is_call_source(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Route
    )
}

// --- Integration-edge candidacy (Req 9.1 / 9.2) ----------------------------

/// Language / id-prefix tag emitted by the Markdown artifact extractor
/// (`parser::artifact::markdown`) for every heading-section `Module` symbol and
/// for the Markdown whole-file textual fallback.
const MARKDOWN_LABEL: &str = "markdown";

/// Whether `sym` is a Markdown_Extractor-emitted heading-section symbol.
///
/// The Markdown extractor emits one [`SymbolKind::Module`] per ATX heading
/// section (and a single `Module` whole-file textual fallback), all tagged with
/// `language == "markdown"`. Such symbols are documentation content that must be
/// retrieved by semantic co-retrieval, **not** by manufactured graph edges:
/// per Req 9.1/9.2 no integration edge (`RoutesTo`/`Reads`/`Writes`/`Tests`) may
/// be incident to any of them, and they are excluded from every integration-edge
/// candidate set (see [`integration_candidates`]).
///
/// The check is deliberately narrow — kind `Module` **and** the Markdown
/// language tag — so it never excludes a code `Module` symbol (e.g. a Go package
/// module) or any other artifact symbol (e.g. an HTML `Route`).
#[allow(dead_code)] // consumed by the integration-edge resolvers (Tasks 9.2–9.4)
pub(crate) fn is_markdown_section(sym: &Symbol) -> bool {
    sym.kind == SymbolKind::Module && sym.language == MARKDOWN_LABEL
}

/// The candidate symbol set the integration-edge resolvers (RoutesTo — Task 9.2,
/// config Reads — Task 9.3, SQL Reads/Writes — Task 9.4) consume when matching
/// edge endpoints.
///
/// It is the full `symbols` batch with every Markdown heading-section symbol
/// removed (Req 9.2), guaranteeing by construction that no integration edge can
/// be incident to a Markdown `Module` symbol (Req 9.1). The RoutesTo / Reads /
/// Writes producers landing in Tasks 9.2–9.4 build their route/reader/SQL
/// indexes over this slice rather than the raw batch so the exclusion holds for
/// both endpoints without any per-producer guard.
#[allow(dead_code)] // consumed by the integration-edge resolvers (Tasks 9.2–9.4)
pub(crate) fn integration_candidates(symbols: &[Symbol]) -> Vec<&Symbol> {
    symbols.iter().filter(|s| !is_markdown_section(s)).collect()
}

// ---------------------------------------------------------------------------
// Heuristic resolver — `calls` edges
// ---------------------------------------------------------------------------

/// Resolves `calls` edges by scanning identifier references in symbol bodies.
/// Stateless; mirrors `cognis_indexer.resolver.heuristic.HeuristicResolver`.
#[derive(Debug, Default)]
pub struct HeuristicResolver;

impl HeuristicResolver {
    /// Return deduplicated `calls` edges for `symbols`. Never panics.
    ///
    /// For each callable source, scan its body for **call sites** (`name(`) and
    /// resolve each called name to in-batch definitions:
    /// * same-file exact name → confidence `1.0`,
    /// * cross-file, same-language exact name → `0.6`, **only** when the name
    ///   resolves to at most [`CROSS_FANOUT_CAP`] cross-file definitions
    ///   (common names like `get`/`toString` are skipped as unresolvably
    ///   ambiguous rather than fanned out across the repo),
    /// * cross-language matches are dropped (a Java `parse(` does not call a
    ///   Python `parse`).
    ///
    /// There is no fuzzy prefix phase (it was the dominant source of spurious
    /// edges and lowest value).
    pub fn resolve(&self, symbols: &[Symbol]) -> Vec<ResolvedEdge> {
        if symbols.is_empty() {
            return Vec::new();
        }

        // name -> symbols sharing it (multiple symbols can share a name).
        let mut name_to_symbols: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for sym in symbols {
            name_to_symbols
                .entry(sym.name.as_str())
                .or_default()
                .push(sym);
        }

        // Best edge per (src_id, dst_id) — only `calls` here. Keep highest conf.
        let mut best: HashMap<(&str, &str), ResolvedEdge> = HashMap::new();

        for caller in symbols {
            if !is_call_source(caller.kind) {
                continue;
            }
            let Some(excerpt) = caller.body_excerpt.as_deref() else {
                continue;
            };
            if excerpt.is_empty() {
                continue;
            }
            // Distinct called names at real call sites, minus keywords.
            let called: HashSet<&str> = call_site_re()
                .captures_iter(excerpt)
                .filter_map(|c| c.get(1).map(|m| m.as_str()))
                .filter(|n| !is_stopword(n))
                .collect();

            for name in &called {
                let Some(candidates) = name_to_symbols.get(name) else {
                    continue;
                };
                // How many cross-file, same-language definitions this name has:
                // above the cap it's a common name → no cross-module edges.
                let cross_count = candidates
                    .iter()
                    .filter(|c| {
                        c.id != caller.id
                            && c.file_path != caller.file_path
                            && c.language == caller.language
                    })
                    .count();
                let allow_cross = cross_count <= CROSS_FANOUT_CAP;

                for callee in candidates {
                    if callee.id == caller.id {
                        continue; // no self-loops
                    }
                    let confidence = if callee.file_path == caller.file_path {
                        CONF_SAME_FILE
                    } else if callee.language == caller.language {
                        if !allow_cross {
                            continue;
                        }
                        CONF_CROSS_MODULE
                    } else {
                        continue; // drop cross-language matches
                    };
                    merge_call_edge(&mut best, caller, callee, confidence);
                }
            }
        }

        best.into_values().collect()
    }
}

/// Insert or upgrade the best `calls` edge for `(caller, callee)`.
fn merge_call_edge<'a>(
    best: &mut HashMap<(&'a str, &'a str), ResolvedEdge>,
    caller: &'a Symbol,
    callee: &'a Symbol,
    confidence: f64,
) {
    let key = (caller.id.as_str(), callee.id.as_str());
    let upgrade = match best.get(&key) {
        Some(existing) => confidence > existing.confidence,
        None => true,
    };
    if upgrade {
        best.insert(
            key,
            ResolvedEdge::new(
                caller.id.clone(),
                callee.id.clone(),
                EdgeKind::Calls,
                confidence,
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// OOP resolver — `inherits` / `implements` edges (C# / Java)
// ---------------------------------------------------------------------------

/// Resolves inheritance/implementation edges from C#/Java type headers.
/// Mirrors `cognis_indexer.resolver.oop.OOPRelationshipResolver`.
#[derive(Debug, Default)]
pub struct OopResolver;

fn ident_simple_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap())
}

fn group_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Innermost balanced `<...>` (generics) or `(...)` (record params).
    RE.get_or_init(|| Regex::new(r"<[^<>]*>|\([^()]*\)").unwrap())
}

impl OopResolver {
    /// Return inheritance / implementation edges for `symbols`. Never panics.
    pub fn resolve(&self, symbols: &[Symbol]) -> Vec<ResolvedEdge> {
        if symbols.is_empty() {
            return Vec::new();
        }

        // name -> type symbols (class/interface) sharing it.
        let mut type_by_name: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for sym in symbols {
            if is_type_kind(sym.kind) {
                type_by_name.entry(sym.name.as_str()).or_default().push(sym);
            }
        }

        // Best edge per (src_id, dst_id, kind). Keep highest confidence.
        let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
        for sym in symbols {
            if !is_oop_language(&sym.language) || !is_type_kind(sym.kind) {
                continue;
            }
            let header = header(
                sym.body_excerpt
                    .as_deref()
                    .or(sym.signature.as_deref())
                    .unwrap_or(""),
            );
            for (base_name, keyword) in bases(&header, &sym.language) {
                let candidates: Vec<&&Symbol> = type_by_name
                    .get(base_name.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|c| c.id != sym.id)
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                let confidence = if candidates.len() == 1 {
                    CONF_OOP_UNIQUE
                } else {
                    CONF_OOP_AMBIGUOUS
                };
                for dst in candidates {
                    let kind = edge_kind_for(keyword, dst.kind);
                    let key = (sym.id.clone(), dst.id.clone(), kind);
                    let upgrade = match best.get(&key) {
                        Some(existing) => confidence > existing.confidence,
                        None => true,
                    };
                    if upgrade {
                        best.insert(
                            key,
                            ResolvedEdge::new(sym.id.clone(), dst.id.clone(), kind, confidence),
                        );
                    }
                }
            }
        }

        best.into_values().collect()
    }
}

fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Class | SymbolKind::Interface)
}

fn is_oop_language(language: &str) -> bool {
    language == "csharp" || language == "java"
}

/// The declaration header — everything before the body `{`.
fn header(text: &str) -> String {
    match text.find('{') {
        Some(idx) => text[..idx].to_string(),
        None => text.to_string(),
    }
}

/// Remove balanced `<...>` / `(...)` groups iteratively from innermost out.
fn strip_groups(text: &str) -> String {
    let re = group_re();
    let mut current = text.to_string();
    loop {
        let next = re.replace_all(&current, " ").into_owned();
        if next == current {
            return next;
        }
        current = next;
    }
}

/// Simple (unqualified) type name from a possibly-qualified base entry.
fn trailing_name(entry: &str) -> Option<String> {
    ident_simple_re()
        .find_iter(entry)
        .last()
        .map(|m| m.as_str().to_string())
}

/// Keyword tagging a base entry: `extends`/`implements` (Java) or `:` (C#).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseKeyword {
    Extends,
    Implements,
    Colon,
}

/// Parse `(simple_type_name, keyword)` pairs from a type `header`.
fn bases(header: &str, language: &str) -> Vec<(String, BaseKeyword)> {
    let cleaned = strip_groups(header);
    let mut out = Vec::new();

    if language == "java" {
        // `extends <list> [implements ...]`
        if let Some(ext) = java_extends_re().captures(&cleaned) {
            for entry in ext[1].split(',') {
                if let Some(name) = trailing_name(entry) {
                    out.push((name, BaseKeyword::Extends));
                }
            }
        }
        if let Some(imp) = java_implements_re().captures(&cleaned) {
            for entry in imp[1].split(',') {
                if let Some(name) = trailing_name(entry) {
                    out.push((name, BaseKeyword::Implements));
                }
            }
        }
        return out;
    }

    // C#: base list is whatever follows the first ':' up to a `where` clause.
    if let Some(colon) = cleaned.find(':') {
        let mut base_part = &cleaned[colon + 1..];
        if let Some(m) = csharp_where_re().find(base_part) {
            base_part = &base_part[..m.start()];
        }
        for entry in base_part.split(',') {
            if let Some(name) = trailing_name(entry) {
                out.push((name, BaseKeyword::Colon));
            }
        }
    }
    out
}

fn java_extends_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\bextends\b(.*?)(?:\bimplements\b|$)").unwrap())
}

fn java_implements_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\bimplements\b(.*)$").unwrap())
}

fn csharp_where_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bwhere\b").unwrap())
}

/// Map a base-list keyword + resolved target kind to an edge kind.
fn edge_kind_for(keyword: BaseKeyword, dst_kind: SymbolKind) -> EdgeKind {
    match keyword {
        BaseKeyword::Extends => EdgeKind::Inherits,
        BaseKeyword::Implements => EdgeKind::Implements,
        // C# `:` — disambiguate by what the target actually is.
        BaseKeyword::Colon => {
            if dst_kind == SymbolKind::Interface {
                EdgeKind::Implements
            } else {
                EdgeKind::Inherits
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RoutesTo resolver — `RoutesTo` edges (HTML/JS route literal → code handler)
// ---------------------------------------------------------------------------

/// Artifact language tags emitted by the `parser::artifact` extractors
/// (`kind_label`). A "code handler" is by definition a Code_File symbol, so any
/// symbol carrying one of these tags is excluded from the handler side of a
/// `RoutesTo` join: the edge must connect an HTML/JS route literal to a *code*
/// handler, never one artifact `Route` to another artifact symbol (Req 6 design
/// intent). The `Route` sources themselves are HTML-tagged and so are never
/// eligible as handlers.
fn is_artifact_language(language: &str) -> bool {
    matches!(language, "yaml" | "toml" | "sql" | "html" | "markdown")
}

/// A quoted string literal (`"…"`, `'…'`, or `` `…` ``). Capture groups 1/2/3
/// hold the inner value for each quote flavour. Backslash-escaped quotes are
/// treated as literal terminators (the excerpt is a best-effort snippet, not a
/// tokenised AST), which is deliberately conservative: a mis-split can only drop
/// a candidate literal, never invent a false route.
fn string_literal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""([^"\\]*)"|'([^'\\]*)'|`([^`\\]*)`"#).unwrap())
}

/// The set of route strings a **code handler** symbol declares.
///
/// Requirement 6.1 needs a discrete, byte-for-byte-comparable route string on
/// the handler side. The enricher's `http_route` `SymbolAttribute` is *not*
/// available in the resolver (which sees only `&[Symbol]`), so the most faithful
/// signal already carried on the `Symbol` is the route **literal declared in the
/// handler's own source text**: every `/`-prefixed quoted string literal found
/// in the symbol's `signature` and `body_excerpt`. This is exactly how a route
/// is declared at a code handler site (`@app.route("/api/x")`,
/// `mux.HandleFunc("/api/x", …)`, `app.get('/api/x', …)`), and comparing those
/// literals for exact equality against a `Route` symbol's route string is a
/// byte-for-byte, case-sensitive, un-normalized match (Req 6.1).
fn declared_route_literals(sym: &Symbol) -> HashSet<String> {
    let mut out = HashSet::new();
    for text in [sym.signature.as_deref(), sym.body_excerpt.as_deref()]
        .into_iter()
        .flatten()
    {
        for cap in string_literal_re().captures_iter(text) {
            // Whichever quote flavour matched supplies the inner value.
            let value = cap
                .get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .map(|m| m.as_str());
            if let Some(v) = value {
                if v.starts_with('/') {
                    out.insert(v.to_string());
                }
            }
        }
    }
    out
}

/// Emits [`EdgeKind::RoutesTo`] edges joining HTML/JS route literals to the code
/// handlers that declare the same route string (Req 6).
///
/// Stateless. For each [`SymbolKind::Route`] symbol with a non-empty,
/// non-whitespace route string (`name`), it emits one `RoutesTo` edge
/// `Route → handler` at confidence [`CONF_ROUTES_TO`] (`1.0`) for **every** code
/// handler whose declared route literal is byte-for-byte, case-sensitive equal to
/// that route string (Req 6.1/6.4/6.5). No matching handler, or an empty /
/// whitespace-only route string, yields no edge (Req 6.2/6.6). Self-loops are
/// never emitted.
#[derive(Debug, Default)]
pub struct RoutesToResolver;

impl RoutesToResolver {
    /// Return deduplicated `RoutesTo` edges for `symbols`. Never panics.
    pub fn resolve(&self, symbols: &[Symbol]) -> Vec<ResolvedEdge> {
        // Candidate set excludes Markdown heading-section symbols so no edge can
        // be incident to one (Req 9.1/9.2).
        let candidates = integration_candidates(symbols);
        if candidates.is_empty() {
            return Vec::new();
        }

        // Code-handler route index: declared route literal → code handler
        // symbols. Artifact-language symbols (including the HTML `Route` sources
        // themselves) are never handlers.
        let mut handlers_by_route: HashMap<String, Vec<&Symbol>> = HashMap::new();
        for sym in &candidates {
            if is_artifact_language(&sym.language) {
                continue;
            }
            for literal in declared_route_literals(sym) {
                handlers_by_route.entry(literal).or_default().push(sym);
            }
        }
        if handlers_by_route.is_empty() {
            return Vec::new();
        }

        // Best edge per (src_id, dst_id, kind) — dedups a handler that declares
        // the same literal more than once.
        let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
        for route_sym in &candidates {
            if route_sym.kind != SymbolKind::Route {
                continue;
            }
            let route_str = route_sym.name.as_str();
            // Empty or whitespace-only route string → no edge (Req 6.6).
            if route_str.trim().is_empty() {
                continue;
            }
            // Byte-for-byte, case-sensitive lookup (Req 6.1). No match → no edge
            // (Req 6.2).
            let Some(handlers) = handlers_by_route.get(route_str) else {
                continue;
            };
            for handler in handlers {
                if handler.id == route_sym.id {
                    continue; // never a self-loop
                }
                let key = (route_sym.id.clone(), handler.id.clone(), EdgeKind::RoutesTo);
                best.entry(key).or_insert_with(|| {
                    ResolvedEdge::new(
                        route_sym.id.clone(),
                        handler.id.clone(),
                        EdgeKind::RoutesTo,
                        CONF_ROUTES_TO,
                    )
                });
            }
        }

        best.into_values().collect()
    }
}

// ---------------------------------------------------------------------------
// Config Reads resolver — `Reads` edges (code reader site → config-key literal)
// ---------------------------------------------------------------------------

/// Whether `sym` is a config-key symbol emitted by the YAML/TOML extractor.
///
/// The YAML/TOML extractor (`parser::artifact::yaml`) emits one
/// [`SymbolKind::Var`] per leaf key, tagged `language == "yaml"` or `"toml"`; the
/// schema also declares [`SymbolKind::Const`], so both value kinds are accepted
/// on the config-key side. A config key is the **destination** of a config
/// `Reads` edge (code reader → config-key), never a reader itself.
fn is_config_key(sym: &Symbol) -> bool {
    matches!(sym.kind, SymbolKind::Var | SymbolKind::Const)
        && matches!(sym.language.as_str(), "yaml" | "toml")
}

/// Every quoted string-literal inner value referenced in a symbol's own source
/// text (`signature` + `body_excerpt`), for any quote flavour.
///
/// This is the general-purpose counterpart of [`declared_route_literals`] (which
/// keeps only `/`-prefixed values): a **code reader site** references a config
/// key by mentioning its key string as a literal — `os.Getenv("PORT")`,
/// `viper.GetString("db.host")`, `cfg["timeout"]` — so the set of string literals
/// a code symbol contains is exactly the set of key strings it can be said to
/// read. Comparing these against a config-key's matchable literal for byte-for-
/// byte equality is the un-normalized match Req 7.1 requires.
fn all_string_literals(sym: &Symbol) -> HashSet<String> {
    let mut out = HashSet::new();
    for text in [sym.signature.as_deref(), sym.body_excerpt.as_deref()]
        .into_iter()
        .flatten()
    {
        for cap in string_literal_re().captures_iter(text) {
            let value = cap
                .get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .map(|m| m.as_str());
            if let Some(v) = value {
                if !v.is_empty() {
                    out.insert(v.to_string());
                }
            }
        }
    }
    out
}

/// The set of key strings a config-key symbol can be matched against at a code
/// reader site (Req 7.1).
///
/// The extractor's `name` is the fully-qualified dotted leaf-key **path** (e.g.
/// `db.host`, `servers[0].port`, `PORT`). Code reads a config key by either its
/// full dotted path (`viper.GetString("db.host")`) or, when the key is read by
/// its leaf name, its final segment (`os.Getenv("PORT")`). We therefore derive
/// **two** faithful, byte-for-byte-comparable forms after stripping any `[N]`
/// sequence-index components (a code site never reads `servers[0]` by that
/// literal):
///
/// * the full cleaned dotted path (`db.host`), and
/// * its final dotted segment (`host`).
///
/// Whitespace-only or empty results are dropped, so a key with no usable literal
/// contributes nothing (Req 7.4). Returning a set keeps the match un-normalized:
/// a reader literal matches iff it equals one of these strings exactly.
fn config_key_literals(name: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    // Drop sequence-index components like `[0]` from every segment.
    let cleaned: String = name
        .split('.')
        .map(|seg| match seg.find('[') {
            Some(idx) => &seg[..idx],
            None => seg,
        })
        .filter(|seg| !seg.trim().is_empty())
        .collect::<Vec<_>>()
        .join(".");
    let full = cleaned.trim();
    if full.is_empty() {
        return out;
    }
    out.insert(full.to_string());
    if let Some(last) = full.rsplit('.').next() {
        let last = last.trim();
        if !last.is_empty() {
            out.insert(last.to_string());
        }
    }
    out
}

/// Emits [`EdgeKind::Reads`] edges joining code reader sites to the config-key
/// (`Var`/`Const`) literals they reference, guarded by the fan-out cap (Req 7).
///
/// Stateless. For each config-key symbol with a non-empty, non-whitespace key
/// literal (Req 7.4), it counts the **distinct code reader symbols** whose source
/// text references that key string byte-for-byte (a code symbol that mentions the
/// literal `"PORT"` / `"db.host"`). If the count is in `1..=CROSS_FANOUT_CAP`
/// (`8`), it emits one `Reads` edge `code reader → config-key` per reader at the
/// fixed [`CONF_CONFIG_READS`] confidence (Req 7.1). If the count exceeds the cap
/// (Req 7.2) or nothing matches (Req 7.3), it emits no edge for that key.
/// Markdown heading symbols are excluded from candidacy (Req 9.1/9.2) and
/// self-loops are never emitted.
#[derive(Debug, Default)]
pub struct ConfigReadsResolver;

impl ConfigReadsResolver {
    /// Return deduplicated config `Reads` edges for `symbols`. Never panics.
    pub fn resolve(&self, symbols: &[Symbol]) -> Vec<ResolvedEdge> {
        // Candidate set excludes Markdown heading-section symbols so no edge can
        // be incident to one (Req 9.1/9.2).
        let candidates = integration_candidates(symbols);
        if candidates.is_empty() {
            return Vec::new();
        }

        // Reader index: each code (non-artifact) symbol's referenced string
        // literals. Built once over the batch; artifact symbols (config keys,
        // routes, …) are never readers.
        let readers: Vec<(&Symbol, HashSet<String>)> = candidates
            .iter()
            .filter(|s| !is_artifact_language(&s.language))
            .map(|s| (*s, all_string_literals(s)))
            .filter(|(_, lits)| !lits.is_empty())
            .collect();
        if readers.is_empty() {
            return Vec::new();
        }

        let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
        for key_sym in &candidates {
            if !is_config_key(key_sym) {
                continue;
            }
            // Empty / whitespace-only key literal → no edge (Req 7.4).
            let literals = config_key_literals(&key_sym.name);
            if literals.is_empty() {
                continue;
            }

            // Distinct code reader sites referencing this key byte-for-byte.
            let matched: Vec<&Symbol> = readers
                .iter()
                .filter(|(reader, lits)| reader.id != key_sym.id && !lits.is_disjoint(&literals))
                .map(|(reader, _)| *reader)
                .collect();

            // Fan-out precision guard: no match → nothing (Req 7.3); more than
            // the cap → nothing (Req 7.2); within the cap → one edge each
            // (Req 7.1).
            if matched.is_empty() || matched.len() > CROSS_FANOUT_CAP {
                continue;
            }
            for reader in matched {
                let key = (reader.id.clone(), key_sym.id.clone(), EdgeKind::Reads);
                best.entry(key).or_insert_with(|| {
                    ResolvedEdge::new(
                        reader.id.clone(),
                        key_sym.id.clone(),
                        EdgeKind::Reads,
                        CONF_CONFIG_READS,
                    )
                });
            }
        }

        best.into_values().collect()
    }
}

// ---------------------------------------------------------------------------
// SQL edge resolver — `Reads` / `Writes` edges (code query site ↔ SQL table/col)
// ---------------------------------------------------------------------------

/// Fixed confidence of a SQL `Reads`/`Writes` edge joining a SQL table/column
/// symbol to a code query site whose normalized identifier matches (Req 8.3).
///
/// Unlike the exact, byte-for-byte route-string identity of `CONF_ROUTES_TO`
/// (`1.0`), a SQL edge is a **normalized-name** join: the SQL identifier and the
/// code identifier are compared only after CamelCase→snake_case normalization,
/// so a genuine cross-convention match (`user_account` ↔ `UserAccount`) is
/// recovered at the cost of some ambiguity (two logically-distinct names can
/// normalize equal). It is therefore fixed in the required `[0.50, 0.80]` band —
/// identical across every SQL edge and **strictly below** the exact-match
/// ceiling `CONF_ROUTES_TO` — and is a **pre-declared constant**, chosen once and
/// never tuned to a benchmark sample. It satisfies the `[0.0, 1.0]` bound of
/// `Edge::validate`.
const CONF_SQL_EDGE: f64 = 0.65;

/// SQL data-mutation verbs. A code query site whose text contains any of these
/// (case-insensitively, as a whole word) is a **write** query site → `Writes`.
/// A write verb takes precedence over a read verb at the same site, so a site
/// that both reads and mutates is classified as a writer.
fn is_sql_write_verb(upper: &str) -> bool {
    matches!(
        upper,
        "INSERT" | "UPDATE" | "DELETE" | "UPSERT" | "MERGE" | "REPLACE"
    )
}

/// SQL read verb. A code query site whose text contains `SELECT` (and no write
/// verb) is a **read** query site → `Reads`.
fn is_sql_read_verb(upper: &str) -> bool {
    upper == "SELECT"
}

/// CamelCase → snake_case identifier normalization (Req 8.2).
///
/// Inserts an underscore boundary at **each lower→upper transition** and at the
/// **end of a run of consecutive uppercase characters** that begins a new word
/// (an uppercase char whose predecessor is uppercase and whose successor is
/// lowercase), then lowercases and collapses to single, un-padded underscores:
///
/// * `UserID`     → `user_id`
/// * `HTTPServer` → `http_server`
/// * `userName`   → `user_name`
/// * `user_id`    → `user_id` (already snake_case is preserved)
///
/// The collapse/trim final pass makes the transform **idempotent**
/// (`normalize(normalize(x)) == normalize(x)`) and **convention-invariant** (the
/// CamelCase and snake_case spellings of one logical name normalize to the same
/// string), the two guarantees Property 15 (task 9.8) verifies. Applied to both
/// the SQL identifier and the code identifier before exact-equality comparison
/// (Req 8.2).
pub fn normalize_ident(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let mut buf = String::with_capacity(chars.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                // lower→upper transition (`userName` → `user_Name`)
                Some(p) if p.is_lowercase() || p.is_ascii_digit() => true,
                // end of an uppercase run that starts a new word
                // (`HTTPServer`: the `S` before `erver`)
                Some(p) if p.is_ascii_uppercase() => {
                    next.map(|n| n.is_lowercase()).unwrap_or(false)
                }
                _ => false,
            };
            if boundary {
                buf.push('_');
            }
            buf.push(c.to_ascii_lowercase());
        } else {
            buf.push(c);
        }
    }
    // Collapse consecutive/leading/trailing underscores and lowercase every
    // segment — this is what makes the transform idempotent and maps both
    // spellings of the same logical name onto the same string.
    buf.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Emits [`EdgeKind::Reads`] / [`EdgeKind::Writes`] edges joining SQL table and
/// column symbols to the code query sites that reference them, matched on
/// CamelCase→snake_case-normalized names (Req 8).
///
/// Stateless. The SQL side is the constrained index: every [`SymbolKind::Class`]
/// (table) and [`SymbolKind::Var`] (column) symbol tagged `language == "sql"` is
/// keyed by its [`normalize_ident`]-ed name. A **code query site** is any code
/// (non-artifact) symbol whose own source text (`signature` + `body_excerpt`)
/// contains a SQL verb: a write verb ([`is_sql_write_verb`]) classifies the site
/// as a writer → `Writes`, otherwise a `SELECT` ([`is_sql_read_verb`]) classifies
/// it as a reader → `Reads`; a site with no verb is not a query site and yields
/// no edge. For a query site, the matchable identifiers are its normalized
/// word tokens plus its normalized `name` (the struct/field name); a SQL symbol
/// matches when its normalized name equals one of them (Req 8.1/8.2). One edge
/// `code site → SQL symbol` is emitted per matched `(code site, SQL symbol)` pair
/// at the fixed [`CONF_SQL_EDGE`] confidence — so one SQL name matching several
/// code sites yields one edge each, and a code site matching several SQL symbols
/// yields one edge each (Req 8.6). No match → no edge, existing edges unchanged
/// (Req 8.5). Markdown heading symbols are excluded from candidacy (Req 9.1/9.2)
/// and self-loops are never emitted.
#[derive(Debug, Default)]
pub struct SqlEdgeResolver;

impl SqlEdgeResolver {
    /// Return deduplicated SQL `Reads`/`Writes` edges for `symbols`. Never panics.
    pub fn resolve(&self, symbols: &[Symbol]) -> Vec<ResolvedEdge> {
        // Candidate set excludes Markdown heading-section symbols so no edge can
        // be incident to one (Req 9.1/9.2).
        let candidates = integration_candidates(symbols);
        if candidates.is_empty() {
            return Vec::new();
        }

        // SQL identifier index: normalized table/column name → SQL symbols. The
        // whole-file SQL textual fallback is a `Module` and is excluded here.
        let mut sql_by_norm: HashMap<String, Vec<&Symbol>> = HashMap::new();
        for sym in &candidates {
            if sym.language != "sql" || !matches!(sym.kind, SymbolKind::Class | SymbolKind::Var) {
                continue;
            }
            let norm = normalize_ident(&sym.name);
            if norm.is_empty() {
                continue;
            }
            sql_by_norm.entry(norm).or_default().push(sym);
        }
        if sql_by_norm.is_empty() {
            return Vec::new();
        }

        let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
        for site in &candidates {
            // Code sites only — never join one artifact symbol to another.
            if is_artifact_language(&site.language) {
                continue;
            }

            // Scan the code site's own text once: classify read vs write by SQL
            // verb presence, and collect its normalized identifier tokens.
            let mut text = String::new();
            if let Some(s) = site.signature.as_deref() {
                text.push_str(s);
                text.push(' ');
            }
            if let Some(b) = site.body_excerpt.as_deref() {
                text.push_str(b);
            }

            let mut has_read = false;
            let mut has_write = false;
            let mut norm_tokens: HashSet<String> = HashSet::new();
            for m in ident_simple_re().find_iter(&text) {
                let tok = m.as_str();
                let upper = tok.to_ascii_uppercase();
                if is_sql_write_verb(&upper) {
                    has_write = true;
                } else if is_sql_read_verb(&upper) {
                    has_read = true;
                }
                let norm = normalize_ident(tok);
                if !norm.is_empty() {
                    norm_tokens.insert(norm);
                }
            }
            // The code struct/field name is itself a matchable identifier (Req 8.1).
            let name_norm = normalize_ident(&site.name);
            if !name_norm.is_empty() {
                norm_tokens.insert(name_norm);
            }

            // Only a query site (text carries a SQL verb) is eligible; a write
            // verb wins over a read verb (Req 8.1).
            let edge_kind = if has_write {
                EdgeKind::Writes
            } else if has_read {
                EdgeKind::Reads
            } else {
                continue;
            };

            for norm in &norm_tokens {
                let Some(matched) = sql_by_norm.get(norm) else {
                    continue;
                };
                for sql_sym in matched {
                    if sql_sym.id == site.id {
                        continue; // never a self-loop
                    }
                    let key = (site.id.clone(), sql_sym.id.clone(), edge_kind);
                    best.entry(key).or_insert_with(|| {
                        ResolvedEdge::new(
                            site.id.clone(),
                            sql_sym.id.clone(),
                            edge_kind,
                            CONF_SQL_EDGE,
                        )
                    });
                }
            }
        }

        best.into_values().collect()
    }
}

// ---------------------------------------------------------------------------
// Pipeline: merge resolvers + convert to persisted edges
// ---------------------------------------------------------------------------

/// Resolve every edge for a parsed `symbols` batch.
///
/// Runs the heuristic (`calls`), OOP (`inherits`/`implements`), RoutesTo
/// (`RoutesTo`, HTML/JS route literal → code handler), config Reads
/// (`Reads`, code reader site → config-key literal, fan-out-capped), and SQL
/// (`Reads`/`Writes`, code query site → SQL table/column, normalized-name match)
/// resolvers and merges them keeping the highest confidence per
/// `(src_id, dst_id, kind)`.
/// Integration edges are additive: they use a distinct `EdgeKind` and never
/// displace an existing calls/inherits/implements edge, so code-only batches are
/// unchanged. Output is sorted by `(src_id, dst_id, kind)` for deterministic
/// results (mirror `resolver.pipeline.resolve_edges`). The LSP resolver is a
/// post-MVP stub in the Python engine (always empty) and is intentionally
/// omitted here.
pub fn resolve_edges(symbols: &[Symbol]) -> Vec<ResolvedEdge> {
    let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
    for edge in HeuristicResolver
        .resolve(symbols)
        .into_iter()
        .chain(OopResolver.resolve(symbols))
        .chain(RoutesToResolver.resolve(symbols))
        .chain(ConfigReadsResolver.resolve(symbols))
        .chain(SqlEdgeResolver.resolve(symbols))
    {
        let key = (edge.src_id.clone(), edge.dst_id.clone(), edge.kind);
        let upgrade = match best.get(&key) {
            Some(existing) => edge.confidence > existing.confidence,
            None => true,
        };
        if upgrade {
            best.insert(key, edge);
        }
    }

    let mut out: Vec<ResolvedEdge> = best.into_values().collect();
    out.sort_by(|a, b| {
        (a.src_id.as_str(), a.dst_id.as_str(), edge_kind_str(a.kind)).cmp(&(
            b.src_id.as_str(),
            b.dst_id.as_str(),
            edge_kind_str(b.kind),
        ))
    });
    out
}

/// Convert resolved edges to persisted [`Edge`]s, flagging `meta.ambiguous`
/// when `confidence < 0.6` (mirror `resolver.pipeline.persist_edges` /
/// `writer._resolved_to_edge`). A non-ambiguous edge gets an empty `meta`
/// (the store maps that to a SQL `NULL`).
pub fn to_edges(resolved: &[ResolvedEdge]) -> Vec<Edge> {
    resolved.iter().map(to_edge).collect()
}

/// Convert a single [`ResolvedEdge`] to an [`Edge`].
pub fn to_edge(re: &ResolvedEdge) -> Edge {
    let meta = if re.ambiguous {
        serde_json::json!({ "ambiguous": true })
    } else {
        serde_json::Value::Null
    };
    Edge {
        src_id: re.src_id.clone(),
        dst_id: re.dst_id.clone(),
        kind: re.kind,
        confidence: re.confidence,
        meta,
    }
}

/// The DB string a fieldless `EdgeKind` serialises to, for sort ordering.
fn edge_kind_str(kind: EdgeKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_source;

    fn sym(out: &crate::ParseOutput, name: &str) -> Symbol {
        out.symbols.iter().find(|s| s.name == name).unwrap().clone()
    }

    #[test]
    fn same_file_call_is_confidence_one() {
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let edges = HeuristicResolver.resolve(&out.symbols);
        let caller = sym(&out, "caller");
        let helper = sym(&out, "helper");
        let e = edges
            .iter()
            .find(|e| e.src_id == caller.id && e.dst_id == helper.id)
            .expect("caller -> helper edge");
        assert_eq!(e.kind, EdgeKind::Calls);
        assert_eq!(e.confidence, CONF_SAME_FILE);
        assert!(!e.ambiguous);
    }

    #[test]
    fn no_self_loops() {
        let out = parse_source("m.py", "def recurse():\n    return recurse()\n");
        let edges = HeuristicResolver.resolve(&out.symbols);
        assert!(edges.iter().all(|e| e.src_id != e.dst_id));
    }

    #[test]
    fn cross_module_same_language_is_point_six() {
        let a = parse_source("a.py", "def target():\n    return 1\n");
        let b = parse_source("b.py", "def user():\n    return target()\n");
        let mut symbols = a.symbols.clone();
        symbols.extend(b.symbols.clone());
        let edges = HeuristicResolver.resolve(&symbols);
        let user = sym(&b, "user");
        let target = sym(&a, "target");
        let e = edges
            .iter()
            .find(|e| e.src_id == user.id && e.dst_id == target.id)
            .expect("cross-file edge");
        assert_eq!(e.confidence, CONF_CROSS_MODULE);
        assert!(e.ambiguous == (CONF_CROSS_MODULE < AMBIGUOUS_THRESHOLD));
    }

    #[test]
    fn java_extends_and_implements() {
        let src = r#"
package m;
interface Runnable { void run(); }
class Base { }
class Worker extends Base implements Runnable {
    public void run() {}
}
"#;
        let out = parse_source("m.java", src);
        let edges = resolve_edges(&out.symbols);
        let worker = sym(&out, "Worker");
        let base = sym(&out, "Base");
        let runnable = sym(&out, "Runnable");
        assert!(edges
            .iter()
            .any(|e| e.src_id == worker.id && e.dst_id == base.id && e.kind == EdgeKind::Inherits));
        assert!(edges.iter().any(|e| e.src_id == worker.id
            && e.dst_id == runnable.id
            && e.kind == EdgeKind::Implements));
    }

    #[test]
    fn csharp_colon_interface_is_implements_class_is_inherits() {
        let src = r#"
namespace M {
    public interface IClock { long Now(); }
    public class Base { }
    public class Clock : Base, IClock {
        public long Now() { return 0; }
    }
}
"#;
        let out = parse_source("M.cs", src);
        let edges = resolve_edges(&out.symbols);
        let clock = sym(&out, "Clock");
        let base = sym(&out, "Base");
        let iclock = sym(&out, "IClock");
        assert!(edges
            .iter()
            .any(|e| e.src_id == clock.id && e.dst_id == base.id && e.kind == EdgeKind::Inherits));
        assert!(edges.iter().any(|e| e.src_id == clock.id
            && e.dst_id == iclock.id
            && e.kind == EdgeKind::Implements));
    }

    #[test]
    fn to_edge_sets_ambiguous_meta_below_threshold() {
        // 0.4 is below the 0.6 ambiguity threshold → flagged ambiguous.
        let re = ResolvedEdge::new("a".into(), "b".into(), EdgeKind::Calls, 0.4);
        assert!(re.ambiguous);
        let edge = to_edge(&re);
        assert_eq!(edge.meta, serde_json::json!({ "ambiguous": true }));
        assert!(!edge.dst_missing());

        let re2 = ResolvedEdge::new("a".into(), "b".into(), EdgeKind::Calls, CONF_SAME_FILE);
        assert!(!re2.ambiguous);
        assert_eq!(to_edge(&re2).meta, serde_json::Value::Null);
    }

    #[test]
    fn resolve_edges_is_sorted_and_deterministic() {
        let out = parse_source(
            "m.py",
            "def a():\n    return b()\n\ndef b():\n    return a()\n",
        );
        let first = resolve_edges(&out.symbols);
        let second = resolve_edges(&out.symbols);
        assert_eq!(first, second);
        let keys: Vec<_> = first
            .iter()
            .map(|e| (e.src_id.clone(), e.dst_id.clone(), edge_kind_str(e.kind)))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn empty_batch_yields_no_edges() {
        assert!(resolve_edges(&[]).is_empty());
    }

    // --- Markdown-exclusion predicate + candidate filtering (Task 9.1) ------

    #[test]
    fn is_markdown_section_true_for_markdown_module() {
        // A real Markdown heading-section symbol from the artifact extractor.
        let out = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/GUIDE.md",
            "# Title\nbody line\n## Security\nsecure stuff\n",
        );
        assert!(!out.symbols.is_empty());
        for s in &out.symbols {
            assert_eq!(s.kind, SymbolKind::Module);
            assert_eq!(s.language, "markdown");
            assert!(
                is_markdown_section(s),
                "markdown Module heading section must be excluded: {}",
                s.name
            );
        }
    }

    #[test]
    fn is_markdown_section_true_for_markdown_textual_fallback() {
        // A heading-less Markdown file falls back to a single whole-file Module
        // symbol, still tagged `language == "markdown"` — also excluded.
        let out = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/NOTES.md",
            "just prose, no headings at all\n",
        );
        assert_eq!(out.symbols.len(), 1);
        assert!(out.fell_back);
        assert!(is_markdown_section(&out.symbols[0]));
    }

    #[test]
    fn is_markdown_section_false_for_code_symbols() {
        // Code symbols (functions, classes, methods) are never markdown sections.
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\nclass Widget:\n    def build(self):\n        return 2\n",
        );
        assert!(!out.symbols.is_empty());
        for s in &out.symbols {
            assert!(
                !is_markdown_section(s),
                "code symbol {} ({:?}) must not be treated as a markdown section",
                s.name,
                s.kind
            );
        }
    }

    #[test]
    fn is_markdown_section_false_for_go_module_symbol() {
        // A code `Module` symbol from a Go file shares the kind but not the
        // markdown language tag, so it must NOT be excluded.
        let mut go_module = callable("mypackage", "main.go", "package main\n");
        go_module.kind = SymbolKind::Module;
        go_module.language = "go".to_string();
        assert!(!is_markdown_section(&go_module));
    }

    #[test]
    fn is_markdown_section_false_for_html_route() {
        // An HTML `Route` symbol is an integration-edge endpoint, never excluded.
        let out = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Html,
            "web/index.html",
            "<a href=\"/api/world/state\">go</a>\n",
        );
        assert!(
            out.symbols.iter().any(|s| s.kind == SymbolKind::Route),
            "expected at least one Route symbol: {:?}",
            out.symbols
        );
        for s in &out.symbols {
            assert!(
                !is_markdown_section(s),
                "html symbol {} ({:?}) must not be excluded",
                s.name,
                s.kind
            );
        }
    }

    #[test]
    fn integration_candidates_excludes_only_markdown_sections() {
        // Mixed batch: Python code + a markdown doc section. The candidate slice
        // for the integration-edge resolvers must drop the markdown section and
        // keep every code symbol, preserving order.
        let code = parse_source("m.py", "def handler():\n    return 1\n");
        let md = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/GUIDE.md",
            "# Title\nbody\n",
        );
        let mut batch: Vec<Symbol> = code.symbols.clone();
        batch.extend(md.symbols.clone());

        let candidates = integration_candidates(&batch);

        // No markdown section survives.
        assert!(
            candidates.iter().all(|s| !is_markdown_section(s)),
            "candidate set must contain no markdown section"
        );
        // Every code symbol survives.
        for c in &code.symbols {
            assert!(
                candidates.iter().any(|s| s.id == c.id),
                "code symbol {} must remain a candidate",
                c.name
            );
        }
        // Exactly the markdown symbols were removed.
        assert_eq!(candidates.len(), batch.len() - md.symbols.len());
    }

    #[test]
    fn integration_candidates_all_kept_when_no_markdown() {
        let out = parse_source(
            "m.py",
            "def a():\n    return b()\n\ndef b():\n    return 1\n",
        );
        let candidates = integration_candidates(&out.symbols);
        assert_eq!(candidates.len(), out.symbols.len());
    }

    // --- RoutesTo resolver (Task 9.2) --------------------------------------

    /// A real HTML `Route` symbol for `route`, emitted by the artifact extractor.
    fn html_route(route: &str) -> Symbol {
        let src = format!("<script>fetch('{route}');</script>\n");
        let out = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Html,
            "web/index.html",
            &src,
        );
        out.symbols
            .into_iter()
            .find(|s| s.kind == SymbolKind::Route && s.name == route)
            .unwrap_or_else(|| panic!("expected a Route symbol for {route}"))
    }

    /// A hand-built `Route` symbol whose route string is exactly `route`. Used to
    /// reach empty / whitespace route strings the HTML extractor never emits.
    fn route_symbol(route: &str) -> Symbol {
        let mut s = callable("routelit", "web/index.html", "");
        s.id = format!("html:web/index.html:route@{}", route.len());
        s.kind = SymbolKind::Route;
        s.name = route.to_string();
        s.language = "html".to_string();
        s
    }

    #[test]
    fn routes_to_matches_code_handler_exact() {
        let route = html_route("/api/world/state");
        // A Go-style handler that declares the same route literal in its body.
        let handler = callable(
            "worldState",
            "server.go",
            "http.HandleFunc(\"/api/world/state\", worldState)\n",
        );
        let edges = RoutesToResolver.resolve(&[route.clone(), handler.clone()]);
        let e = edges
            .iter()
            .find(|e| e.src_id == route.id && e.dst_id == handler.id)
            .expect("Route -> handler RoutesTo edge");
        assert_eq!(e.kind, EdgeKind::RoutesTo);
        assert_eq!(e.confidence, CONF_ROUTES_TO);
        assert_eq!(e.confidence, 1.0);
        assert!(!e.ambiguous);
        // Directed Route(src) -> handler(dst), never the reverse.
        assert!(!edges.iter().any(|e| e.src_id == handler.id));
    }

    #[test]
    fn routes_to_no_match_emits_no_edge() {
        let route = html_route("/api/x");
        let handler = callable("other", "server.go", "http.HandleFunc(\"/api/y\", other)\n");
        let edges = RoutesToResolver.resolve(&[route, handler]);
        assert!(
            edges.is_empty(),
            "no byte-for-byte match => no edge: {edges:?}"
        );
    }

    #[test]
    fn routes_to_is_case_sensitive() {
        let route = html_route("/API/State");
        // Same route but lower-cased — must NOT match (no normalization, Req 6.1).
        let handler = callable("h", "server.go", "http.HandleFunc(\"/api/state\", h)\n");
        let edges = RoutesToResolver.resolve(&[route, handler]);
        assert!(
            edges.is_empty(),
            "case-different route must not match: {edges:?}"
        );
    }

    #[test]
    fn routes_to_empty_or_whitespace_route_emits_no_edge() {
        // A handler that declares a `/`-prefixed literal is present, but the Route
        // string is empty / whitespace-only, so no edge is emitted (Req 6.6).
        let handler = callable("h", "server.go", "http.HandleFunc(\"/\", h)\n");
        for empty in ["", "   ", "\t\n"] {
            let route = route_symbol(empty);
            let edges = RoutesToResolver.resolve(&[route, handler.clone()]);
            assert!(
                edges.is_empty(),
                "empty/whitespace route {empty:?} must emit no edge: {edges:?}"
            );
        }
    }

    #[test]
    fn routes_to_multiple_handlers_one_edge_each() {
        let route = html_route("/api/x");
        let h1 = callable("h1", "a.go", "http.HandleFunc(\"/api/x\", h1)\n");
        let h2 = callable("h2", "b.go", "router.get('/api/x', h2)\n");
        let edges = RoutesToResolver.resolve(&[route.clone(), h1.clone(), h2.clone()]);
        assert_eq!(edges.len(), 2, "one edge per matching handler: {edges:?}");
        assert!(edges
            .iter()
            .any(|e| e.src_id == route.id && e.dst_id == h1.id && e.kind == EdgeKind::RoutesTo));
        assert!(edges
            .iter()
            .any(|e| e.src_id == route.id && e.dst_id == h2.id && e.kind == EdgeKind::RoutesTo));
    }

    #[test]
    fn routes_to_handler_declaring_literal_twice_yields_one_edge() {
        let route = html_route("/api/x");
        // The same literal appears twice in the handler body.
        let handler = callable(
            "h",
            "a.go",
            "log(\"/api/x\"); http.HandleFunc(\"/api/x\", h)\n",
        );
        let edges = RoutesToResolver.resolve(&[route.clone(), handler.clone()]);
        assert_eq!(edges.len(), 1, "deduped to one edge: {edges:?}");
    }

    #[test]
    fn routes_to_ignores_artifact_handlers() {
        // A YAML config value equal to the route literal must NOT be treated as a
        // code handler — RoutesTo joins route literals to CODE handlers only.
        let route = html_route("/api/x");
        let mut yaml_sym = callable("server.path", "config.yaml", "\"/api/x\"");
        yaml_sym.id = "yaml:config.yaml:server.path@1".to_string();
        yaml_sym.kind = SymbolKind::Var;
        yaml_sym.language = "yaml".to_string();
        // Another HTML Route with the same string is also not a handler.
        let route2 = {
            let mut r = route_symbol("/api/x");
            r.id = "html:other.html:route@1".to_string();
            r
        };
        let edges = RoutesToResolver.resolve(&[route, yaml_sym, route2]);
        assert!(
            edges.is_empty(),
            "artifact symbols are never handlers: {edges:?}"
        );
    }

    #[test]
    fn routes_to_edges_pass_edge_validate() {
        let route = html_route("/api/world/state");
        let handler = callable(
            "worldState",
            "server.go",
            "http.HandleFunc(\"/api/world/state\", worldState)\n",
        );
        let resolved = RoutesToResolver.resolve(&[route, handler]);
        assert!(!resolved.is_empty());
        for e in to_edges(&resolved) {
            e.validate().expect("emitted RoutesTo edge must validate");
            assert_eq!(e.kind, EdgeKind::RoutesTo);
            assert_eq!(e.confidence, 1.0);
        }
    }

    #[test]
    fn routes_to_excludes_markdown_endpoints() {
        // A markdown section whose body contains the route literal must never be
        // an endpoint of a RoutesTo edge (Req 9.1/9.2).
        let route = html_route("/api/x");
        let md = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/API.md",
            "# Routes\nsee \"/api/x\" for the world state\n",
        );
        let mut batch = vec![route];
        batch.extend(md.symbols.clone());
        let edges = RoutesToResolver.resolve(&batch);
        assert!(
            edges.is_empty(),
            "markdown symbols must not be RoutesTo endpoints: {edges:?}"
        );
    }

    #[test]
    fn code_only_batch_emits_no_routes_to_edges() {
        // Integration edges are additive: a pure-code batch produces the same
        // calls/inherits/implements output and zero RoutesTo edges.
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let edges = resolve_edges(&out.symbols);
        assert!(!edges.is_empty(), "calls edges still resolve");
        assert!(
            edges.iter().all(|e| e.kind != EdgeKind::RoutesTo),
            "no RoutesTo edge for a code-only batch: {edges:?}"
        );
    }

    /// Build a minimal callable symbol with a given name/file/body for the
    /// precision + fan-out tests.
    fn callable(name: &str, file: &str, body: &str) -> Symbol {
        Symbol {
            id: format!("py:{file}:{name}@{name}"),
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: format!("py:{file}:{name}"),
            language: "python".to_string(),
            module: file.trim_end_matches(".py").to_string(),
            file_path: file.to_string(),
            line_start: 1,
            line_end: 3,
            signature: None,
            docstring: None,
            content_hash: "h".to_string(),
            body_excerpt: Some(body.to_string()),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 0,
        }
    }

    #[test]
    fn only_call_sites_create_edges_not_bare_mentions() {
        // `helper` is *mentioned* (a type/variable read) but never called, while
        // `used` is invoked. Only the call site should yield an edge.
        let caller = callable("caller", "a.py", "x: helper = 1\n    return used()\n");
        let helper = callable("helper", "a.py", "pass");
        let used = callable("used", "a.py", "pass");
        let edges = HeuristicResolver.resolve(&[caller.clone(), helper.clone(), used.clone()]);
        assert!(
            edges.iter().any(|e| e.dst_id == used.id),
            "call site `used()` should resolve"
        );
        assert!(
            !edges.iter().any(|e| e.dst_id == helper.id),
            "bare mention `helper` must NOT create an edge"
        );
    }

    #[test]
    fn class_bodies_are_not_call_sources() {
        // A Class symbol whose body mentions a call must not become a caller
        // (its methods are the real callers).
        let mut klass = callable("Widget", "w.py", "def build(self):\n    render()\n");
        klass.kind = SymbolKind::Class;
        let render = callable("render", "w.py", "pass");
        let edges = HeuristicResolver.resolve(&[klass.clone(), render]);
        assert!(
            edges.iter().all(|e| e.src_id != klass.id),
            "a class must not be a calls source"
        );
    }

    #[test]
    fn common_name_fanout_is_capped_cross_module() {
        // A call to `common()` where `common` is defined in more than the cap's
        // worth of *other* files: no cross-module edges (unresolvably common),
        // but a same-file definition still resolves.
        let mut batch = vec![callable("caller", "caller.py", "return common()\n")];
        batch.push(callable("common", "caller.py", "pass")); // same-file → 1.0
        for i in 0..(CROSS_FANOUT_CAP + 3) {
            batch.push(callable("common", &format!("other{i}.py"), "pass"));
        }
        let edges = HeuristicResolver.resolve(&batch);
        let caller_id = &batch[0].id;
        let cross: Vec<_> = edges
            .iter()
            .filter(|e| &e.src_id == caller_id && e.confidence == CONF_CROSS_MODULE)
            .collect();
        assert!(
            cross.is_empty(),
            "over-cap common name must emit no cross-module edges, got {}",
            cross.len()
        );
        assert!(
            edges
                .iter()
                .any(|e| &e.src_id == caller_id && e.confidence == CONF_SAME_FILE),
            "same-file definition should still resolve"
        );
    }

    #[test]
    fn cross_module_within_cap_resolves() {
        let batch = vec![
            callable("user", "a.py", "return target()\n"),
            callable("target", "b.py", "pass"),
        ];
        let edges = HeuristicResolver.resolve(&batch);
        assert!(edges
            .iter()
            .any(|e| e.dst_id == batch[1].id && e.confidence == CONF_CROSS_MODULE));
    }

    // --- Config Reads resolver (Task 9.3) ----------------------------------

    /// A config-key `Var` symbol as the YAML/TOML extractor emits it: `name` is
    /// the dotted leaf-key path, `language == "yaml"`, and `body_excerpt` carries
    /// the path plus scalar value.
    fn config_key(path: &str, value: &str) -> Symbol {
        let text = if value.is_empty() {
            path.to_string()
        } else {
            format!("{path} {value}")
        };
        let mut s = callable("k", "config/app.yaml", "");
        s.id = format!("yaml:config/app.yaml:{path}@{}", text.len());
        s.kind = SymbolKind::Var;
        s.name = path.to_string();
        s.qualified_name = format!("yaml:config/app.yaml:{path}");
        s.language = "yaml".to_string();
        s.body_excerpt = Some(text);
        s
    }

    /// A code reader site: a callable whose body references `literal` as a string.
    fn reader(name: &str, file: &str, literal: &str) -> Symbol {
        callable(name, file, &format!("return os.Getenv(\"{literal}\")\n"))
    }

    #[test]
    fn config_reads_exact_match_emits_reader_to_key_at_fixed_confidence() {
        let key = config_key("PORT", "8080");
        let rd = reader("loadPort", "server.go", "PORT");
        let edges = ConfigReadsResolver.resolve(&[key.clone(), rd.clone()]);
        let e = edges
            .iter()
            .find(|e| e.src_id == rd.id && e.dst_id == key.id)
            .expect("reader -> config-key Reads edge");
        assert_eq!(e.kind, EdgeKind::Reads);
        // Directed reader(src) -> config-key(dst), never the reverse.
        assert!(!edges.iter().any(|e| e.src_id == key.id));
        // Fixed, pre-declared confidence constant.
        assert_eq!(e.confidence, CONF_CONFIG_READS);
        assert_eq!(e.confidence, 0.7);
    }

    #[test]
    fn config_reads_matches_full_dotted_path_and_leaf_segment() {
        let key = config_key("db.host", "localhost");
        // One reader references the full dotted path, another the leaf segment.
        let full = reader("dial", "db.go", "db.host");
        let leaf = reader("connect", "net.go", "host");
        let edges = ConfigReadsResolver.resolve(&[key.clone(), full.clone(), leaf.clone()]);
        assert_eq!(edges.len(), 2, "both readers match: {edges:?}");
        assert!(edges
            .iter()
            .any(|e| e.src_id == full.id && e.dst_id == key.id && e.kind == EdgeKind::Reads));
        assert!(edges
            .iter()
            .any(|e| e.src_id == leaf.id && e.dst_id == key.id && e.kind == EdgeKind::Reads));
    }

    #[test]
    fn config_reads_at_cap_emits_edges() {
        // Exactly CROSS_FANOUT_CAP distinct reader sites → one edge each.
        let key = config_key("PORT", "8080");
        let mut batch = vec![key.clone()];
        for i in 0..CROSS_FANOUT_CAP {
            batch.push(reader(&format!("r{i}"), &format!("f{i}.go"), "PORT"));
        }
        let edges = ConfigReadsResolver.resolve(&batch);
        assert_eq!(
            edges.len(),
            CROSS_FANOUT_CAP,
            "one Reads edge per reader at the cap: {edges:?}"
        );
        assert!(edges
            .iter()
            .all(|e| e.dst_id == key.id && e.kind == EdgeKind::Reads));
    }

    #[test]
    fn config_reads_over_cap_emits_no_edge() {
        // More than CROSS_FANOUT_CAP matching sites → the key is too generic, so
        // no edge at all (Req 7.2).
        let key = config_key("PORT", "8080");
        let mut batch = vec![key.clone()];
        for i in 0..(CROSS_FANOUT_CAP + 1) {
            batch.push(reader(&format!("r{i}"), &format!("f{i}.go"), "PORT"));
        }
        let edges = ConfigReadsResolver.resolve(&batch);
        assert!(
            edges.is_empty(),
            "over-cap fan-out must emit no Reads edge: {} edges",
            edges.len()
        );
    }

    #[test]
    fn config_reads_no_match_emits_no_edge() {
        let key = config_key("PORT", "8080");
        let rd = reader("loadOther", "server.go", "TIMEOUT");
        let edges = ConfigReadsResolver.resolve(&[key, rd]);
        assert!(
            edges.is_empty(),
            "no byte-for-byte match => no edge: {edges:?}"
        );
    }

    #[test]
    fn config_reads_empty_or_whitespace_key_emits_no_edge() {
        // A reader that references a `/`-shaped literal is present, but the config
        // key literal is empty / whitespace-only, so no edge (Req 7.4).
        let rd = reader("r", "server.go", "PORT");
        for empty in ["", "   ", "\t", "[0]"] {
            let key = config_key(empty, "v");
            let edges = ConfigReadsResolver.resolve(&[key, rd.clone()]);
            assert!(
                edges.is_empty(),
                "empty/whitespace key {empty:?} must emit no edge: {edges:?}"
            );
        }
    }

    #[test]
    fn config_reads_excludes_markdown_readers() {
        // A markdown section whose body references the key literal must never be
        // a reader endpoint of a config Reads edge (Req 9.1/9.2).
        let key = config_key("PORT", "8080");
        let md = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/CONFIG.md",
            "# Settings\nthe server reads \"PORT\" from the environment\n",
        );
        let mut batch = vec![key];
        batch.extend(md.symbols.clone());
        let edges = ConfigReadsResolver.resolve(&batch);
        assert!(
            edges.is_empty(),
            "markdown symbols must not be config-Reads endpoints: {edges:?}"
        );
    }

    #[test]
    fn config_reads_ignores_artifact_readers_and_self() {
        // Another config-key (artifact) symbol that happens to contain the key
        // literal is not a code reader, and a config key never reads itself.
        let key = config_key("PORT", "8080");
        let other_yaml = config_key("mirror.PORT", "PORT");
        let edges = ConfigReadsResolver.resolve(&[key.clone(), other_yaml]);
        assert!(
            edges.is_empty(),
            "artifact symbols are never readers: {edges:?}"
        );
    }

    #[test]
    fn config_reads_edges_pass_edge_validate() {
        let key = config_key("PORT", "8080");
        let rd = reader("loadPort", "server.go", "PORT");
        let resolved = ConfigReadsResolver.resolve(&[key, rd]);
        assert!(!resolved.is_empty());
        for e in to_edges(&resolved) {
            e.validate().expect("emitted Reads edge must validate");
            assert_eq!(e.kind, EdgeKind::Reads);
            assert!((0.0..=1.0).contains(&e.confidence));
        }
    }

    #[test]
    fn code_only_batch_emits_no_config_reads_edges() {
        // Integration edges are additive: a pure-code batch produces its normal
        // calls edges and zero config `Reads` edges.
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let edges = resolve_edges(&out.symbols);
        assert!(!edges.is_empty(), "calls edges still resolve");
        assert!(
            edges.iter().all(|e| e.kind != EdgeKind::Reads),
            "no Reads edge for a code-only batch: {edges:?}"
        );
    }

    // --- SQL edge resolver (Task 9.4) --------------------------------------

    /// The real SQL table/column symbols the SQL extractor emits for `src`.
    fn sql_symbols(src: &str) -> Vec<Symbol> {
        crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Sql,
            "db/schema.sql",
            src,
        )
        .symbols
    }

    /// The emitted SQL `Class` (table) symbol named `name`.
    fn sql_table(syms: &[Symbol], name: &str) -> Symbol {
        syms.iter()
            .find(|s| s.kind == SymbolKind::Class && s.name == name)
            .unwrap_or_else(|| panic!("expected a SQL table symbol {name}"))
            .clone()
    }

    #[test]
    fn normalize_ident_camel_to_snake_examples() {
        assert_eq!(normalize_ident("UserID"), "user_id");
        assert_eq!(normalize_ident("HTTPServer"), "http_server");
        assert_eq!(normalize_ident("userName"), "user_name");
        assert_eq!(normalize_ident("UserAccount"), "user_account");
        // Already snake_case is preserved.
        assert_eq!(normalize_ident("user_account"), "user_account");
        // Screaming snake collapses to plain snake.
        assert_eq!(normalize_ident("USER_ID"), "user_id");
    }

    #[test]
    fn normalize_ident_is_idempotent_and_convention_invariant() {
        for id in [
            "UserID",
            "HTTPServer",
            "userName",
            "user_account",
            "getUserByID",
            "OrderLineItem",
        ] {
            let once = normalize_ident(id);
            assert_eq!(
                normalize_ident(&once),
                once,
                "normalize must be idempotent for {id}"
            );
        }
        // CamelCase and snake_case spellings map to the same string.
        assert_eq!(
            normalize_ident("UserAccount"),
            normalize_ident("user_account")
        );
        assert_eq!(normalize_ident("orderId"), normalize_ident("order_id"));
    }

    #[test]
    fn sql_read_site_emits_reads_at_fixed_confidence() {
        let syms = sql_symbols("CREATE TABLE users (id INTEGER, email TEXT);\n");
        let table = sql_table(&syms, "users");
        // A code query site that SELECTs from `users`.
        let site = callable(
            "listUsers",
            "store.go",
            "rows, _ := db.Query(\"SELECT id, email FROM users\")\n",
        );
        let mut batch = syms.clone();
        batch.push(site.clone());

        let edges = SqlEdgeResolver.resolve(&batch);
        let e = edges
            .iter()
            .find(|e| e.src_id == site.id && e.dst_id == table.id)
            .expect("code site -> users table Reads edge");
        assert_eq!(e.kind, EdgeKind::Reads);
        assert_eq!(e.confidence, CONF_SQL_EDGE);
        // Directed code-site(src) -> SQL symbol(dst), never the reverse.
        assert!(!edges.iter().any(|e| e.src_id == table.id));
    }

    #[test]
    fn sql_write_site_emits_writes() {
        let syms = sql_symbols("CREATE TABLE users (id INTEGER, email TEXT);\n");
        let table = sql_table(&syms, "users");
        let site = callable(
            "createUser",
            "store.go",
            "db.Exec(\"INSERT INTO users (email) VALUES (?)\", email)\n",
        );
        let mut batch = syms.clone();
        batch.push(site.clone());

        let edges = SqlEdgeResolver.resolve(&batch);
        let e = edges
            .iter()
            .find(|e| e.src_id == site.id && e.dst_id == table.id)
            .expect("code site -> users table Writes edge");
        assert_eq!(e.kind, EdgeKind::Writes);
        assert_eq!(e.confidence, CONF_SQL_EDGE);
    }

    #[test]
    fn sql_edge_matches_across_naming_conventions() {
        // SQL table `user_account`; code query site references it as `UserAccount`.
        let syms = sql_symbols("CREATE TABLE user_account (id INTEGER);\n");
        let table = sql_table(&syms, "user_account");
        let site = callable(
            "loadAccount",
            "store.go",
            "db.Query(\"SELECT * FROM UserAccount WHERE id = ?\")\n",
        );
        let mut batch = syms.clone();
        batch.push(site.clone());

        let edges = SqlEdgeResolver.resolve(&batch);
        assert!(
            edges
                .iter()
                .any(|e| e.src_id == site.id && e.dst_id == table.id && e.kind == EdgeKind::Reads),
            "normalized names must match across conventions: {edges:?}"
        );
    }

    #[test]
    fn sql_edge_no_match_emits_no_edge() {
        // SQL declares only `users`; the code query site references a different
        // table, so no SQL edge is emitted.
        let syms = sql_symbols("CREATE TABLE users (id INTEGER);\n");
        let site = callable(
            "listAccounts",
            "store.go",
            "db.Query(\"SELECT * FROM accounts\")\n",
        );
        let mut batch = syms.clone();
        batch.push(site);

        let edges = SqlEdgeResolver.resolve(&batch);
        assert!(
            edges.is_empty(),
            "no normalized-name match must emit no edge: {edges:?}"
        );
    }

    #[test]
    fn sql_edge_non_query_site_emits_no_edge() {
        // A struct whose name matches the table but whose text carries no SQL
        // verb is not a query site, so no read/write classification applies.
        let syms = sql_symbols("CREATE TABLE user_account (id INTEGER);\n");
        let mut strct = callable("UserAccount", "models.go", "field int\n");
        strct.kind = SymbolKind::Class;
        let mut batch = syms.clone();
        batch.push(strct);

        let edges = SqlEdgeResolver.resolve(&batch);
        assert!(
            edges.is_empty(),
            "a non-query code site must emit no SQL edge: {edges:?}"
        );
    }

    #[test]
    fn sql_edge_multiple_sites_one_edge_each() {
        let syms = sql_symbols("CREATE TABLE users (id INTEGER);\n");
        let table = sql_table(&syms, "users");
        let r1 = callable("a", "a.go", "db.Query(\"SELECT id FROM users\")\n");
        let r2 = callable("b", "b.go", "db.Query(\"SELECT id FROM users\")\n");
        let mut batch = syms.clone();
        batch.push(r1.clone());
        batch.push(r2.clone());

        let edges = SqlEdgeResolver.resolve(&batch);
        let to_table: Vec<_> = edges.iter().filter(|e| e.dst_id == table.id).collect();
        assert!(to_table.iter().any(|e| e.src_id == r1.id));
        assert!(to_table.iter().any(|e| e.src_id == r2.id));
        // One edge per (site, table) pair — no duplicates.
        assert_eq!(to_table.len(), 2, "one edge per matched site: {edges:?}");
    }

    #[test]
    fn sql_edges_excludes_markdown_endpoints() {
        // A markdown section whose body mentions a table name + a SQL verb must
        // never be an endpoint of a SQL edge (Req 9.1/9.2).
        let syms = sql_symbols("CREATE TABLE users (id INTEGER);\n");
        let md = crate::parser::artifact::extract_artifact(
            crate::pipeline::ArtifactKind::Markdown,
            "docs/DB.md",
            "# Schema\nWe SELECT from users often.\n",
        );
        let mut batch = syms.clone();
        batch.extend(md.symbols.clone());

        let edges = SqlEdgeResolver.resolve(&batch);
        for md_sym in &md.symbols {
            assert!(
                edges
                    .iter()
                    .all(|e| e.src_id != md_sym.id && e.dst_id != md_sym.id),
                "no SQL edge may touch a markdown symbol"
            );
        }
    }

    #[test]
    fn sql_edges_pass_edge_validate_with_strict_confidence_ordering() {
        let syms = sql_symbols("CREATE TABLE users (id INTEGER);\n");
        let site = callable(
            "listUsers",
            "store.go",
            "db.Query(\"SELECT id FROM users\")\n",
        );
        let mut batch = syms.clone();
        batch.push(site);

        let resolved = SqlEdgeResolver.resolve(&batch);
        assert!(!resolved.is_empty());
        for e in to_edges(&resolved) {
            e.validate().expect("emitted SQL edge must validate");
            assert!(matches!(e.kind, EdgeKind::Reads | EdgeKind::Writes));
            // Fixed, in [0.50, 0.80], and strictly below the RoutesTo ceiling.
            assert_eq!(e.confidence, CONF_SQL_EDGE);
            assert!((0.50..=0.80).contains(&e.confidence));
            assert!(e.confidence < CONF_ROUTES_TO);
        }
    }

    #[test]
    fn code_only_batch_emits_no_sql_edges() {
        // A pure-code batch (no SQL symbols) yields its normal calls edges and no
        // Reads/Writes edges.
        let out = parse_source(
            "m.py",
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        );
        let edges = resolve_edges(&out.symbols);
        assert!(
            edges
                .iter()
                .all(|e| !matches!(e.kind, EdgeKind::Reads | EdgeKind::Writes)),
            "no SQL edge for a code-only batch: {edges:?}"
        );
    }
}
