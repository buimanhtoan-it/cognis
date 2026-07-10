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
// Pipeline: merge resolvers + convert to persisted edges
// ---------------------------------------------------------------------------

/// Resolve every edge for a parsed `symbols` batch.
///
/// Runs the heuristic (`calls`) and OOP (`inherits`/`implements`) resolvers and
/// merges them keeping the highest confidence per `(src_id, dst_id, kind)`.
/// Output is sorted by `(src_id, dst_id, kind)` for deterministic results
/// (mirror `resolver.pipeline.resolve_edges`). The LSP resolver is a post-MVP
/// stub in the Python engine (always empty) and is intentionally omitted here.
pub fn resolve_edges(symbols: &[Symbol]) -> Vec<ResolvedEdge> {
    let mut best: HashMap<(String, String, EdgeKind), ResolvedEdge> = HashMap::new();
    for edge in HeuristicResolver
        .resolve(symbols)
        .into_iter()
        .chain(OopResolver.resolve(symbols))
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
}
