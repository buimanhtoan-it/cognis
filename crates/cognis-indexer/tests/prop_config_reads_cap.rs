//! Property-based test for the config `Reads` fan-out cap (Task 9.6).
//!
//! Feature: non-code-artifact-coverage, Property 13: Config Reads edges respect the fan-out cap
//!
//! Validates: Requirements 7.1, 7.2, 7.3
//!
//! ## The property
//!
//! *For any* config-key symbol and set of code reader sites, the
//! `Config_Reads_Resolver` emits exactly one `EdgeKind::Reads` edge
//! (code reader → config-key) per matching reader site **iff** the number of
//! matching reader sites is in `1..=CROSS_FANOUT_CAP` (8). If the count exceeds
//! the cap (Req 7.2), or the key literal is empty / whitespace-only, or nothing
//! matches (Req 7.3), it emits **no** `Reads` edge for that key.
//!
//! ## How it is driven
//!
//! The test builds a **known model** rather than parsing source text:
//!
//! * one config-key symbol — a `Var`/`Const` tagged `yaml`/`toml` whose `name`
//!   is a unique dotted key path (so `config_key_literals` yields the full path
//!   and its final segment, and nothing accidentally cross-matches), and
//! * `N` distinct code-reader symbols (non-artifact language `go`), each with a
//!   `body_excerpt` that quotes exactly one of the key's matchable literals
//!   (full path or last segment), so every reader is a genuine, distinct match.
//!
//! Because the model is known up front, the exact expected edge set is
//! computable: `N == 0` or `N > 8` → zero edges; `1 <= N <= 8` → exactly `N`
//! edges, one `reader → key` edge per reader. The key literal is also varied to
//! be empty / whitespace-only, which must yield zero edges regardless of `N`.
//!
//! Identifiers (symbol ids, file paths, key path) are unique by construction so
//! each reader site is counted exactly once and the direction `reader (src) →
//! config-key (dst)` is asserted precisely.

use std::collections::BTreeSet;

use cognis_core::{EdgeKind, Symbol, SymbolKind};
use cognis_indexer::resolver::ConfigReadsResolver;
use proptest::prelude::*;

/// The pre-declared fan-out cap the resolver enforces (mirrors
/// `resolver::CROSS_FANOUT_CAP`). Kept as a local constant so the test states
/// the boundary it verifies rather than importing a private item.
const CROSS_FANOUT_CAP: usize = 8;

/// Whether the config key literal is a normal (matchable) key, or an
/// empty / whitespace-only key that must never yield an edge (Req 7.4 boundary,
/// asserted here as part of the fan-out behaviour: no literal → no edge).
#[derive(Debug, Clone)]
enum KeyLiteral {
    /// A unique dotted key path, e.g. `zqwer.plkjh`; matchable literals are the
    /// full path and its last segment.
    Normal(String),
    /// The empty string.
    Empty,
    /// Whitespace only.
    Whitespace,
}

/// A generated scenario: the config key, its declared kind/language, and the
/// per-reader choice of which matchable literal (full path vs. last segment)
/// each reader references.
#[derive(Debug, Clone)]
struct Model {
    key: KeyLiteral,
    key_is_const: bool,
    key_is_toml: bool,
    /// One entry per reader site; `true` → the reader references the key's last
    /// dotted segment, `false` → the full dotted path. Its length is `N`.
    reader_uses_last: Vec<bool>,
}

/// A single lowercase identifier segment: lowercase letters only so it can never
/// collide with a keyword and is trivially unique within the generated batch.
fn seg() -> impl Strategy<Value = String> {
    "[a-z]{3,8}"
}

/// A unique dotted key path of 1–2 segments (so both the full-path and
/// last-segment matchable literals are exercised).
fn key_name() -> impl Strategy<Value = String> {
    prop::collection::vec(seg(), 1..=2).prop_map(|segs| segs.join("."))
}

fn key_literal() -> impl Strategy<Value = KeyLiteral> {
    prop_oneof![
        // Weight the normal case heavily so most iterations exercise the cap.
        8 => key_name().prop_map(KeyLiteral::Normal),
        1 => Just(KeyLiteral::Empty),
        1 => Just(KeyLiteral::Whitespace),
    ]
}

fn model() -> impl Strategy<Value = Model> {
    (
        key_literal(),
        any::<bool>(),
        any::<bool>(),
        // 0..=15 readers: spans N == 0, the whole in-cap band 1..=8, and the
        // over-cap band 9..=15.
        prop::collection::vec(any::<bool>(), 0..=15),
    )
        .prop_map(|(key, key_is_const, key_is_toml, reader_uses_last)| Model {
            key,
            key_is_const,
            key_is_toml,
            reader_uses_last,
        })
}

/// Build a minimal valid [`Symbol`] with a distinct id and the given fields.
fn mk_symbol(
    id: &str,
    kind: SymbolKind,
    name: &str,
    language: &str,
    file_path: &str,
    body: Option<String>,
) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: format!("{language}:{file_path}:{name}"),
        language: language.to_string(),
        module: String::new(),
        file_path: file_path.to_string(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: String::new(),
        body_excerpt: body,
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// The raw key `name` string for the config-key symbol.
fn key_name_str(key: &KeyLiteral) -> &str {
    match key {
        KeyLiteral::Normal(s) => s.as_str(),
        KeyLiteral::Empty => "",
        KeyLiteral::Whitespace => "   ",
    }
}

/// The literal a reader should quote so it matches the key.
///
/// For a normal key, `use_last` selects the final dotted segment, otherwise the
/// full path; both are in the key's matchable-literal set, so either is a match.
/// For empty/whitespace keys there is no matchable literal, so the reader quotes
/// the raw (non-matching) key string — it can never produce an edge.
fn reader_reference(key: &KeyLiteral, use_last: bool) -> String {
    match key {
        KeyLiteral::Normal(path) => {
            if use_last {
                path.rsplit('.').next().unwrap_or(path).to_string()
            } else {
                path.clone()
            }
        }
        KeyLiteral::Empty => "x".to_string(),
        KeyLiteral::Whitespace => "   ".to_string(),
    }
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 13.
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: non-code-artifact-coverage, Property 13: Config Reads edges respect the fan-out cap
    #[test]
    fn config_reads_respect_fanout_cap(model in model()) {
        let n = model.reader_uses_last.len();

        // --- Config-key symbol (destination). Its id is always non-empty even
        // when the key name is empty, so the Symbol stays valid. ---
        let key_lang = if model.key_is_toml { "toml" } else { "yaml" };
        let key_kind = if model.key_is_const {
            SymbolKind::Const
        } else {
            SymbolKind::Var
        };
        let key_sym = mk_symbol(
            "cfgkey:config/app.cfg:__the_key__",
            key_kind,
            key_name_str(&model.key),
            key_lang,
            "config/app.cfg",
            // The scalar value; no matchable literal of its own.
            Some("value: 1234".to_string()),
        );

        // --- N distinct code reader sites (source). ---
        let mut symbols = vec![key_sym.clone()];
        let mut reader_ids: BTreeSet<String> = BTreeSet::new();
        for (i, &use_last) in model.reader_uses_last.iter().enumerate() {
            let reference = reader_reference(&model.key, use_last);
            let id = format!("go:svc/reader{i}.go:read{i}");
            reader_ids.insert(id.clone());
            let body = format!("cfg := os.Getenv(\"{reference}\")\nreturn cfg\n");
            symbols.push(mk_symbol(
                &id,
                SymbolKind::Function,
                &format!("read{i}"),
                "go",
                &format!("svc/reader{i}.go"),
                Some(body),
            ));
        }

        // Every constructed symbol must be valid input.
        for s in &symbols {
            s.validate().expect("constructed symbol must be valid");
        }

        let edges = ConfigReadsResolver.resolve(&symbols);

        // --- Expected edge count from the known model. ---
        let key_has_literal = matches!(model.key, KeyLiteral::Normal(_));
        let expected = if key_has_literal && (1..=CROSS_FANOUT_CAP).contains(&n) {
            n
        } else {
            0
        };

        prop_assert_eq!(
            edges.len(),
            expected,
            "N={} key={:?}: expected {} Reads edge(s), got {}",
            n,
            model.key,
            expected,
            edges.len()
        );

        if expected > 0 {
            // One edge per reader site, all directed reader (src) → key (dst),
            // all of kind Reads with a valid confidence.
            let mut seen_src: BTreeSet<String> = BTreeSet::new();
            for e in &edges {
                prop_assert_eq!(e.kind, EdgeKind::Reads, "edge must be a Reads edge");
                prop_assert_eq!(
                    &e.dst_id,
                    &key_sym.id,
                    "edge destination must be the config-key symbol"
                );
                prop_assert!(
                    reader_ids.contains(&e.src_id),
                    "edge source {:?} must be one of the reader sites",
                    e.src_id
                );
                prop_assert!(
                    (0.0..=1.0).contains(&e.confidence),
                    "confidence {} must be within [0.0, 1.0]",
                    e.confidence
                );
                prop_assert!(
                    seen_src.insert(e.src_id.clone()),
                    "each reader site must contribute at most one edge (dup src {:?})",
                    e.src_id
                );
            }
            // Exactly the set of readers produced edges (one per site).
            prop_assert_eq!(
                seen_src,
                reader_ids,
                "every reader site must contribute exactly one Reads edge"
            );
        }
    }
}
