//! Property-based test for SQL Reads/Writes normalized-name matching (Task 9.7).
//!
//! Feature: non-code-artifact-coverage, Property 14: SQL Reads/Writes edges match on normalized names
//!
//! Validates: Requirements 8.1, 8.5, 8.6
//!
//! ## The property
//!
//! *For any* SQL table/column symbol and set of code sites, the
//! [`SqlEdgeResolver`] emits **one `Reads` edge per read site** and **one
//! `Writes` edge per write site** whose CamelCase→snake_case-normalized
//! identifier equals the normalized SQL identifier, and emits **no edge**
//! (leaving existing edges unchanged) when no site matches. Every edge is
//! directed `code-site (src) → SQL-symbol (dst)` and carries the fixed SQL-edge
//! confidence in `[0.50, 0.80]`.
//!
//! ## How it is driven — a KNOWN model with an exact oracle
//!
//! Rather than emit SQL/DDL and code and hope to re-derive the answer, the test
//! builds a *known* model and computes the exact expected edge set:
//!
//! * A **pool of logical names** — each a vector of lowercase words. Its
//!   `canonical` spelling is `words.join("_")` (already snake_case). Names are
//!   deduplicated by canonical and any name colliding with a SQL verb / query
//!   template token is dropped, so every logical name is globally unique and can
//!   never be confused with a keyword. The pool is split into **SQL names**
//!   (which become real `SymbolKind::Class`/`Var` symbols tagged `language ==
//!   "sql"`) and **decoy names** (referenced by code but never declared in SQL).
//!
//! * A set of **code query sites**. Each site is classified up front as a
//!   *read* site (`SELECT … FROM …`), a *write* site (`INSERT INTO …`), or a
//!   *non-query* site (no SQL verb → must yield no edge). Each site references a
//!   chosen subset of pool names, rendering each reference in either snake_case
//!   (`user_account`) or PascalCase (`UserAccount`) — two conventions that both
//!   normalize to the same canonical string, exercising cross-convention
//!   matching (Req 8.1).
//!
//! Because the model is known, the oracle is exact: a site contributes one edge
//! per referenced name **that is a declared SQL name**, of kind `Reads` for a
//! read site and `Writes` for a write site; a decoy reference or a non-query
//! site contributes nothing. The oracle keys matches on *logical-name identity*
//! (not by re-running the resolver's normalization), so a resolver that failed
//! to normalize the PascalCase spelling would under-produce, and a resolver that
//! matched a decoy would over-produce — either way the exact set comparison
//! catches it.

use std::collections::{BTreeSet, HashMap, HashSet};

use cognis_core::{EdgeKind, Symbol, SymbolKind};
use cognis_indexer::resolver::SqlEdgeResolver;
use proptest::prelude::*;

/// SQL verbs and query-template tokens. A single-word logical name whose
/// canonical spelling collides with one of these is dropped from the pool so a
/// generated name can never be mistaken for a keyword (which would change a
/// site's read/write classification) or for a template token (which would
/// manufacture a spurious match).
const RESERVED: &[&str] = &[
    "select", "insert", "update", "delete", "upsert", "merge", "replace", "from", "into", "values",
    "where", "set", "db", "query", "exec", "rows", "process", "and", "or", "id",
];

/// SQL confidence band required by the design (Req 8.3): identical across every
/// SQL edge and strictly below the exact-match ceiling `1.0`.
const SQL_CONF_LO: f64 = 0.50;
const SQL_CONF_HI: f64 = 0.80;

/// Read / write / non-query classification chosen for a generated site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Read,
    Write,
    None,
}

/// A resolved logical name after filtering: its snake_case canonical spelling
/// and whether it is a declared SQL symbol (vs a decoy).
#[derive(Debug, Clone)]
struct PoolEntry {
    words: Vec<String>,
    canonical: String,
    /// `Some(sql_symbol_id)` when this name is a declared SQL table/column.
    sql_id: Option<String>,
}

/// A word: lowercase letters, length 2..=6 so PascalCase→snake_case is
/// unambiguous (no single-letter uppercase runs), and never a bare keyword.
fn word() -> impl Strategy<Value = String> {
    "[a-z]{2,6}"
}

/// A logical name: 1..=3 words.
fn logical() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(word(), 1..4)
}

/// A generated site: a verb classification plus a list of `(pool_index, use_pascal)`
/// references. Indices are taken modulo the pool length in the test body.
fn site_spec() -> impl Strategy<Value = (Verb, Vec<(usize, bool)>)> {
    let verb = prop_oneof![Just(Verb::Read), Just(Verb::Write), Just(Verb::None)];
    (
        verb,
        prop::collection::vec((any::<usize>(), any::<bool>()), 0..6),
    )
}

/// snake_case (canonical) spelling of a logical name.
fn snake(words: &[String]) -> String {
    words.join("_")
}

/// PascalCase spelling: capitalize the first letter of each word, concatenate.
/// For lowercase words of length >= 2 this normalizes back to the canonical
/// snake_case string, so it is a genuine cross-convention alias.
fn pascal(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            let mut cs = w.chars();
            let first = cs.next().unwrap().to_ascii_uppercase();
            format!("{first}{}", cs.as_str())
        })
        .collect()
}

/// Build a code query site symbol whose body references `names` (each already
/// rendered in a chosen convention) inside a verb-appropriate template.
fn code_site(idx: usize, verb: Verb, names: &[String]) -> Symbol {
    let joined = names.join(", ");
    let body = match verb {
        Verb::Read => format!("rows := db.Query(\"SELECT * FROM {joined}\")\n"),
        Verb::Write => format!("db.Exec(\"INSERT INTO {joined} VALUES (?)\")\n"),
        Verb::None => format!("process({joined})\n"),
    };
    let name = format!("site_{idx}");
    Symbol {
        id: format!("py:svc/store_{idx}.go:{name}@{idx}"),
        kind: SymbolKind::Function,
        name,
        qualified_name: format!("py:svc/store_{idx}.go:site_{idx}"),
        language: "python".to_string(),
        module: format!("svc/store_{idx}"),
        file_path: format!("svc/store_{idx}.go"),
        line_start: 1,
        line_end: 3,
        signature: None,
        docstring: None,
        content_hash: "h".to_string(),
        body_excerpt: Some(body),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// Build a declared SQL table/column symbol named `canonical` (snake_case),
/// tagged `language == "sql"`. `is_table` picks `Class` (table) vs `Var`
/// (column).
fn sql_symbol(idx: usize, canonical: &str, is_table: bool) -> Symbol {
    let kind = if is_table {
        SymbolKind::Class
    } else {
        SymbolKind::Var
    };
    Symbol {
        id: format!("sql:db/schema.sql:{canonical}@{idx}"),
        kind,
        name: canonical.to_string(),
        qualified_name: format!("sql:db/schema.sql:{canonical}"),
        language: "sql".to_string(),
        module: "db/schema".to_string(),
        file_path: "db/schema.sql".to_string(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: "h".to_string(),
        body_excerpt: Some(canonical.to_string()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// Stable string tag for an edge kind, for set comparison / messages.
fn kind_str(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        _ => "other",
    }
}

proptest! {
    // Minimum 100 iterations per the spec; one test for Property 14.
    #![proptest_config(ProptestConfig::with_cases(128))]

    // Feature: non-code-artifact-coverage, Property 14: SQL Reads/Writes edges match on normalized names
    #[test]
    fn sql_reads_writes_match_on_normalized_names(
        raw_pool in prop::collection::vec(logical(), 1..10),
        site_specs in prop::collection::vec(site_spec(), 0..6),
    ) {
        // --- Build the pool: dedup by canonical, drop reserved collisions, split
        //     into SQL symbols (Class/Var, alternating) and decoys. ---
        let reserved: HashSet<&str> = RESERVED.iter().copied().collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut pool: Vec<PoolEntry> = Vec::new();
        let mut sql_symbols: Vec<Symbol> = Vec::new();

        for words in raw_pool {
            let canonical = snake(&words);
            if reserved.contains(canonical.as_str()) || !seen.insert(canonical.clone()) {
                continue; // keyword collision or duplicate logical name
            }
            let pool_idx = pool.len();
            // Alternate: even → declared SQL symbol, odd → decoy. Alternate the
            // SQL kind too so both tables (Class) and columns (Var) appear.
            let sql_id = if pool_idx.is_multiple_of(2) {
                let is_table = (pool_idx / 2).is_multiple_of(2);
                let sym = sql_symbol(pool_idx, &canonical, is_table);
                let id = sym.id.clone();
                sql_symbols.push(sym);
                Some(id)
            } else {
                None
            };
            pool.push(PoolEntry { words, canonical, sql_id });
        }

        // --- Build the code sites and the exact oracle edge set. ---
        // Oracle: (src_id, dst_id, kind_str). One edge per (query-site,
        // declared-SQL-name) pair the site references.
        let mut oracle: HashSet<(String, String, &'static str)> = HashSet::new();
        let mut batch: Vec<Symbol> = sql_symbols.clone();

        if !pool.is_empty() {
            for (j, (verb, refs)) in site_specs.iter().enumerate() {
                // Resolve references to distinct pool entries; render each in the
                // chosen convention (snake vs Pascal — both alias to canonical).
                let mut rendered: Vec<String> = Vec::new();
                let mut referenced_canon: BTreeSet<String> = BTreeSet::new();
                for &(idx, use_pascal) in refs {
                    let entry = &pool[idx % pool.len()];
                    if !referenced_canon.insert(entry.canonical.clone()) {
                        continue; // already referenced this logical name at this site
                    }
                    rendered.push(if use_pascal {
                        pascal(&entry.words)
                    } else {
                        snake(&entry.words)
                    });
                }

                let site = code_site(j, *verb, &rendered);

                // Oracle contribution: only read/write sites emit edges, one per
                // referenced *declared* SQL name.
                let kind = match verb {
                    Verb::Read => Some("reads"),
                    Verb::Write => Some("writes"),
                    Verb::None => None,
                };
                if let Some(kstr) = kind {
                    for canon in &referenced_canon {
                        if let Some(entry) = pool.iter().find(|e| &e.canonical == canon) {
                            if let Some(sql_id) = &entry.sql_id {
                                oracle.insert((site.id.clone(), sql_id.clone(), kstr));
                            }
                        }
                    }
                }

                batch.push(site);
            }
        }

        // --- Run the resolver and compare against the oracle exactly. ---
        let edges = SqlEdgeResolver.resolve(&batch);

        let actual: HashSet<(String, String, &'static str)> = edges
            .iter()
            .map(|e| (e.src_id.clone(), e.dst_id.clone(), kind_str(e.kind)))
            .collect();

        prop_assert_eq!(
            &actual,
            &oracle,
            "SQL Reads/Writes edge set must match the known-model oracle exactly\nsql={:?}",
            sql_symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        // Set of declared SQL symbol ids, to assert edge direction.
        let sql_ids: HashSet<&str> = sql_symbols.iter().map(|s| s.id.as_str()).collect();

        for e in &edges {
            // Only Reads/Writes are ever produced by this resolver (Req 8.1).
            prop_assert!(
                e.kind == EdgeKind::Reads || e.kind == EdgeKind::Writes,
                "SQL resolver must only emit Reads/Writes, got {:?}",
                e.kind
            );
            // Direction: code-site (src) → SQL-symbol (dst), never reversed
            // (Req 8.6). The SQL symbol is always the destination.
            prop_assert!(
                sql_ids.contains(e.dst_id.as_str()),
                "edge dst must be a declared SQL symbol: {e:?}"
            );
            prop_assert!(
                !sql_ids.contains(e.src_id.as_str()),
                "edge src must be a code site, never a SQL symbol: {e:?}"
            );
            prop_assert_ne!(&e.src_id, &e.dst_id, "no self-loops");
            // Fixed confidence in the required band, identical across edges (Req 8.3).
            prop_assert!(
                (SQL_CONF_LO..=SQL_CONF_HI).contains(&e.confidence),
                "SQL edge confidence {} out of [{}, {}]",
                e.confidence,
                SQL_CONF_LO,
                SQL_CONF_HI
            );
        }
        // Confidence is identical across every emitted SQL edge (Req 8.3).
        if let Some(first) = edges.first() {
            let c0 = first.confidence;
            for e in &edges {
                prop_assert_eq!(e.confidence, c0, "SQL edge confidence must be fixed");
            }
        }

        // When no site matches (empty oracle), the resolver emits no SQL edge,
        // leaving the (pre-existing, here empty) edge set unchanged (Req 8.5).
        if oracle.is_empty() {
            prop_assert!(
                edges.is_empty(),
                "no normalized-name match must emit no edge: {edges:?}"
            );
        }

        // Guard the oracle against duplicate SQL canonicals (would break the
        // one-name→one-id assumption). Cheap and makes the test self-checking.
        let mut canon_ids: HashMap<&str, usize> = HashMap::new();
        for s in &sql_symbols {
            *canon_ids.entry(s.name.as_str()).or_default() += 1;
        }
        prop_assert!(
            canon_ids.values().all(|&c| c == 1),
            "test invariant: SQL canonical names must be unique"
        );
    }
}
