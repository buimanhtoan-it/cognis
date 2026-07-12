//! Fair-harness quality benchmark — design **Property 5** (Pillar-1 quality
//! non-regression, Requirements 6.1 / 6.2).
//!
//! This module reproduces, in Rust, the objective **PR-derived** fair-harness
//! that lives in `.benchmarks/public` (`bench.py`, `union_bench_pr.py`): it
//! computes **Recall@k**, **MRR** and **Contamination@k** for a retrieval
//! method over a golden set whose labels come from real bug-fix commits
//! (`relevant` = the symbols whose source lines a fix commit changed; the call
//! graph was *never* consulted, so the key is structure-blind). The same three
//! metrics, the same hub definition (top-`HUB_FRAC` by weighted degree) and the
//! same label-resolution rule (`(name, file_path_substring)` → symbol ids) as
//! the Python harness are used, so the numbers are directly comparable.
//!
//! ## What is computed where (honest scope)
//!
//! The full Python harness ranks five methods (BM25, DENSE, RRF, CSAR, UNION).
//! DENSE / RRF / UNION and the Python seed all require a **query embedding** of
//! the commit-subject query, which needs a live embedder. Following the same
//! offline discipline as `onnx_parity` / `index_parity` / `differential_parity`
//! (never fabricate a number), this harness computes the surfaces that are
//! genuinely reproducible **offline against the indexed UCKG alone**:
//!
//! * **lexical** — a tf-idf cosine ranker over `name + file_path +
//!   body_excerpt` (the Python `BM25` channel). The objective benchmark DBs are
//!   index-only — their `symbol_fts` is empty because the Python harness builds
//!   its *own* tf-idf rather than using FTS5 — so the faithful reproduction is
//!   that same tf-idf, computed offline from the `symbol` rows.
//! * **csar** — forward-push PPR seeded from the lexical top-`SEED_K`, the Rust
//!   counterpart of the Python `CSAR` channel (lexical-seeded so it needs no
//!   query embedding).
//!
//! The DENSE/RRF/UNION surfaces are left to the full-pipeline gate, which a test
//! enables only when a query-embedding provider is available; otherwise it skips
//! with a clear message rather than inventing query vectors.
//!
//! ## The non-regression gate (Property 5 / P-Q-NOREG)
//!
//! [`RegressionGate`] encodes the gate that blocks removing Python at K8: the
//! Rust engine must reproduce Python's Recall@k / MRR within a noise band and
//! not increase Contamination@k beyond it. The captured Python objective PR-key
//! numbers (from `.benchmarks/public/RESULTS.md`, EMPIRICALLY SUPPORTED on a
//! finite sample) are exposed by [`python_objective_macro`] /
//! [`python_objective_requests`] so a test can compare without a Python runtime.
//! The metric, golden, hub and gate machinery is pure and deterministic, so it
//! is unit/property-tested directly; the real-data run reports tier-labelled
//! numbers and evaluates the gate.

use std::collections::BTreeSet;
use std::path::Path;

use cognis_core::{CodeGraph, CognisError};
use cognis_csar::{diffuse_seed_hits, DEFAULT_ALPHA, DEFAULT_EPS};
use cognis_store::{Database, SymbolStore};

pub use cognis_core::Result;

/// Default cutoff `k` for Recall@k / Precision@k / Contamination@k (mirrors the
/// Python harness `_K`).
pub const DEFAULT_K: usize = 10;
/// Default number of lexical hits used to seed the CSAR diffusion (mirrors the
/// Python harness `_SEED_K`).
pub const DEFAULT_SEED_K: usize = 20;
/// Fraction of highest-degree nodes treated as "hubs" for Contamination@k
/// (mirrors the Python harness `_HUB_FRAC`).
pub const DEFAULT_HUB_FRAC: f64 = 0.10;

// ===========================================================================
// Metrics — pure functions, the exact definitions of the Python `_metrics`.
// ===========================================================================

/// The four per-query retrieval metrics (mirrors the Python `_metrics` tuple).
///
/// All are in `[0, 1]`. `recall` = relevant-found-in-top-`k` / total-relevant;
/// `precision` = relevant-found-in-top-`k` / `k`; `mrr` = reciprocal rank of the
/// first relevant hit over the **full** ranking; `contamination` = fraction of
/// the top-`k` that are hub (high-degree) nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Recall@k.
    pub recall: f64,
    /// Precision@k.
    pub precision: f64,
    /// Mean reciprocal rank (single-query reciprocal rank here).
    pub mrr: f64,
    /// Hub Contamination@k.
    pub contamination: f64,
}

/// Compute the per-query [`Metrics`] for a ranked `order` of symbol ids.
///
/// `relevant` is the query's ground-truth symbol-id set, `hubs` the high-degree
/// node id set, `k` the cutoff. Mirrors the Python `_metrics(order, rel, hubs,
/// k)` operation-for-operation: top-`k` slice for recall/precision/contamination
/// and a full-order scan for MRR. An empty `relevant` set yields `recall = 0`
/// (the harness skips such queries upstream, matching Python).
pub fn compute_metrics(
    order: &[String],
    relevant: &BTreeSet<String>,
    hubs: &BTreeSet<String>,
    k: usize,
) -> Metrics {
    let top = &order[..order.len().min(k)];
    let hit = top.iter().filter(|id| relevant.contains(*id)).count();
    let recall = if relevant.is_empty() {
        0.0
    } else {
        hit as f64 / relevant.len() as f64
    };
    let precision = if k == 0 { 0.0 } else { hit as f64 / k as f64 };
    let contamination = if k == 0 {
        0.0
    } else {
        top.iter().filter(|id| hubs.contains(*id)).count() as f64 / k as f64
    };
    let mut mrr = 0.0;
    for (rank, id) in order.iter().enumerate() {
        if relevant.contains(id) {
            mrr = 1.0 / (rank as f64 + 1.0);
            break;
        }
    }
    Metrics {
        recall,
        precision,
        mrr,
        contamination,
    }
}

/// Aggregated (mean) metrics for one method across a set of evaluated queries.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodAggregate {
    /// Method name (`"lexical"`, `"csar"`, …).
    pub method: String,
    /// Number of queries averaged.
    pub n_eval: usize,
    /// Mean Recall@k.
    pub recall: f64,
    /// Mean Precision@k.
    pub precision: f64,
    /// Mean MRR.
    pub mrr: f64,
    /// Mean Contamination@k.
    pub contamination: f64,
}

impl MethodAggregate {
    /// Mean over a slice of per-query [`Metrics`]. Empty input yields all-zero
    /// means with `n_eval = 0`.
    pub fn mean(method: impl Into<String>, per_query: &[Metrics]) -> Self {
        let n = per_query.len();
        let (mut r, mut p, mut m, mut c) = (0.0, 0.0, 0.0, 0.0);
        for q in per_query {
            r += q.recall;
            p += q.precision;
            m += q.mrr;
            c += q.contamination;
        }
        let d = if n == 0 { 1.0 } else { n as f64 };
        MethodAggregate {
            method: method.into(),
            n_eval: n,
            recall: r / d,
            precision: p / d,
            mrr: m / d,
            contamination: c / d,
        }
    }

    /// Project to the `(recall, mrr, contamination)` triple the gate compares.
    pub fn score(&self) -> MethodScore {
        MethodScore {
            recall: self.recall,
            mrr: self.mrr,
            contamination: self.contamination,
        }
    }
}

// ===========================================================================
// Golden set — the objective PR-derived key.
// ===========================================================================

/// One golden query: a bug-fix commit subject (`q`) and the symbols its diff
/// touched (`relevant`, each `(name, file_path_substring)`).
#[derive(Debug, Clone)]
pub struct GoldenQuery {
    /// The query string (the commit subject in the objective key).
    pub q: String,
    /// The originating commit sha, when recorded.
    pub sha: Option<String>,
    /// Ground-truth labels: `(symbol_name, file_path_substring)` pairs.
    pub relevant: Vec<(String, String)>,
}

/// A parsed golden set (`{ "repo": ..., "queries": [...] }`).
#[derive(Debug, Clone)]
pub struct GoldenSet {
    /// The originating repo path, when recorded.
    pub repo: Option<String>,
    /// The golden queries.
    pub queries: Vec<GoldenQuery>,
}

/// Parse a golden set from the harness JSON (`bench.py` / `union_bench_pr.py`
/// format).
///
/// Shape: `{ "repo"?: str, "queries": [ { "q": str, "sha"?: str, "relevant":
/// [[name, path_sub], ...] }, ... ] }`. Malformed `relevant` entries (not a
/// 2-element string array) are skipped rather than erroring, matching the
/// permissive Python loader.
///
/// # Errors
/// Returns [`CognisError::Eval`] when the JSON is invalid or lacks a `queries`
/// array.
pub fn parse_golden_set(json: &str) -> Result<GoldenSet> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CognisError::Eval(format!("golden JSON: {e}")))?;
    let repo = value
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let queries_json = value
        .get("queries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CognisError::Eval("golden set missing `queries` array".into()))?;

    let mut queries = Vec::with_capacity(queries_json.len());
    for item in queries_json {
        let Some(q) = item.get("q").and_then(|v| v.as_str()) else {
            continue;
        };
        let sha = item.get("sha").and_then(|v| v.as_str()).map(str::to_string);
        let mut relevant = Vec::new();
        if let Some(pairs) = item.get("relevant").and_then(|v| v.as_array()) {
            for pair in pairs {
                if let Some(arr) = pair.as_array() {
                    if let (Some(name), Some(path)) = (
                        arr.first().and_then(|v| v.as_str()),
                        arr.get(1).and_then(|v| v.as_str()),
                    ) {
                        relevant.push((name.to_string(), path.to_string()));
                    }
                }
            }
        }
        queries.push(GoldenQuery {
            q: q.to_string(),
            sha,
            relevant,
        });
    }
    Ok(GoldenSet { repo, queries })
}

/// Load and parse a golden set from `path`.
///
/// # Errors
/// Returns [`CognisError::Eval`] when the file cannot be read or parsed.
pub fn load_golden_set(path: &Path) -> Result<GoldenSet> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CognisError::Eval(format!("read golden {}: {e}", path.display())))?;
    parse_golden_set(&text)
}

// ===========================================================================
// Symbol table + label resolution (mirrors the Python `Repo.resolve`).
// ===========================================================================

/// `(symbol_id, name, normalized_file_path)` rows used to resolve golden labels.
///
/// `file_path` is stored normalized (`\` → `/`, lowercased) so substring
/// matching is path-separator and case insensitive — exactly the Python
/// `resolve` normalization.
#[derive(Debug, Clone)]
pub struct SymbolTable {
    rows: Vec<(String, String, String)>,
}

impl SymbolTable {
    /// Build from the DB's `symbol` rows.
    ///
    /// # Errors
    /// Propagates any store read error.
    pub fn from_db(db: &Database) -> Result<Self> {
        let rows = db
            .list_symbols()?
            .into_iter()
            .map(|s| (s.id, s.name, normalize_path(&s.file_path)))
            .collect();
        Ok(SymbolTable { rows })
    }

    /// Number of symbols.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Resolve a `(name, file_path_substring)` label to the set of matching
    /// symbol ids — exact name match AND normalized path-substring containment,
    /// mirroring the Python `Repo.resolve`.
    pub fn resolve(&self, name: &str, path_sub: &str) -> BTreeSet<String> {
        let sub = normalize_path(path_sub);
        self.rows
            .iter()
            .filter(|(_, n, fp)| n == name && fp.contains(&sub))
            .map(|(id, _, _)| id.clone())
            .collect()
    }

    /// The union of resolved ids over every label of `query` (the query's
    /// ground-truth relevant set).
    pub fn resolve_relevant(&self, query: &GoldenQuery) -> BTreeSet<String> {
        let mut rel = BTreeSet::new();
        for (name, path_sub) in &query.relevant {
            rel.extend(self.resolve(name, path_sub));
        }
        rel
    }
}

/// Normalize a file path for substring matching: `\` → `/`, lowercased.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

// ===========================================================================
// Hub set (mirrors the Python `Repo.hubs`).
// ===========================================================================

/// The set of hub symbol ids: the top `frac` of nodes by weighted degree.
///
/// Mirrors the Python `set(argsort(-degree)[:max(1, int(n*frac))])` but keyed by
/// symbol id (so it is comparable across builds with different node ordering).
/// Ties are broken by ascending node index for determinism. An empty graph
/// yields an empty set.
pub fn hub_ids(graph: &CodeGraph, frac: f64) -> BTreeSet<String> {
    let n = graph.n();
    if n == 0 {
        return BTreeSet::new();
    }
    let count = ((n as f64) * frac) as usize;
    let count = count.max(1).min(n);
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        graph.degree[b]
            .partial_cmp(&graph.degree[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });
    idx.into_iter()
        .take(count)
        .map(|i| graph.node_ids[i].clone())
        .collect()
}

// ===========================================================================
// Tokenization + tf-idf lexical channel (mirrors the Python `bench.py`).
// ===========================================================================
//
// The objective benchmark DBs are index-only: their `symbol_fts` is empty
// (the Python harness deliberately builds its *own* tf-idf over the `symbol`
// rows rather than using FTS5). So the faithful Rust reproduction of the
// fair-harness lexical channel is the same tf-idf — not the engine's FTS5 layer
// — computed over `name + file_path + body_excerpt`.

/// Split free text into the harness's identifier tokens: extract
/// `[A-Za-z][A-Za-z0-9]+` runs, split each on camelCase / acronym boundaries,
/// lowercase, and keep tokens of length > 1 — the Python `_camel(_TOK.findall())`
/// pipeline (`bench.py`).
pub fn camel_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in identifier_runs(text) {
        for word in split_camel(&raw) {
            if word.len() > 1 {
                out.push(word);
            }
        }
    }
    out
}

/// Extract maximal runs matching `[A-Za-z][A-Za-z0-9]+` (a letter then one or
/// more alphanumerics) — the Python `_TOK` pattern.
fn identifier_runs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            // A run must start with a letter (leading digits are dropped, so
            // "9abc" yields "abc" exactly like the Python regex).
            if cur.is_empty() {
                if c.is_ascii_alphabetic() {
                    cur.push(c);
                }
            } else {
                cur.push(c);
            }
        } else {
            if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// Split an identifier on camelCase / acronym boundaries, lowercasing each word
/// — approximating the Python `_camel` `[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])` split.
/// e.g. `"HTTPAdapter"` → `["http", "adapter"]`, `"parse_cors"` → `["parse",
/// "cors"]`.
fn split_camel(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    let n = chars.len();
    let mut words = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_ascii_uppercase() {
            // Gather the run of uppercase letters.
            let mut j = i + 1;
            while j < n && chars[j].is_ascii_uppercase() {
                j += 1;
            }
            if j < n && (chars[j].is_ascii_lowercase() || chars[j].is_ascii_digit()) {
                // The last uppercase begins the next word (Upper + lowers);
                // any earlier uppercase letters form an acronym word.
                if j - 1 > i {
                    words.push(chars[i..j - 1].iter().collect::<String>().to_lowercase());
                }
                let start = j - 1;
                let mut e = j;
                while e < n && (chars[e].is_ascii_lowercase() || chars[e].is_ascii_digit()) {
                    e += 1;
                }
                words.push(chars[start..e].iter().collect::<String>().to_lowercase());
                i = e;
            } else {
                // Pure acronym run (end of string or followed by non-lowercase).
                words.push(chars[i..j].iter().collect::<String>().to_lowercase());
                i = j;
            }
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            let mut e = i;
            while e < n && (chars[e].is_ascii_lowercase() || chars[e].is_ascii_digit()) {
                e += 1;
            }
            words.push(chars[i..e].iter().collect::<String>().to_lowercase());
            i = e;
        } else {
            i += 1;
        }
    }
    words
}

// ===========================================================================
// Offline ranking runners (no query embedder required).
// ===========================================================================

/// A tf-idf lexical index over the indexed symbols — the Rust reproduction of
/// the Python `bench.py` lexical channel (`BM25`).
///
/// Each symbol is a document whose bag-of-words is the [`camel_tokens`] of
/// `name + " " + file_path + " " + body_excerpt`; `idf[w] = ln((N+1)/(df+1)) +
/// 1`. A query is scored against every document by tf-idf cosine
/// (`Σ qf·tf·idf² / ‖doc‖`), exactly the `bench.py` formula, then ranked
/// descending (ties broken by symbol id for determinism — the Python
/// `argsort(-lex)` leaves ties unspecified).
#[derive(Debug, Clone)]
pub struct LexicalIndex {
    /// `(symbol_id, term-count bag, ‖doc‖)` per document.
    docs: Vec<(String, std::collections::HashMap<String, u32>, f64)>,
    /// Inverse document frequency per term.
    idf: std::collections::HashMap<String, f64>,
}

impl LexicalIndex {
    /// Build the index from the DB's symbols (the `bench.py` `Repo` tf-idf).
    ///
    /// # Errors
    /// Propagates any store read error.
    pub fn from_db(db: &Database) -> Result<Self> {
        Ok(Self::from_symbols(&db.list_symbols()?))
    }

    /// Build the index from a symbol slice (also the unit-test entry point).
    pub fn from_symbols(symbols: &[cognis_core::Symbol]) -> Self {
        use std::collections::HashMap;
        let mut bags: Vec<(String, HashMap<String, u32>)> = Vec::with_capacity(symbols.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        for s in symbols {
            let text = format!(
                "{} {} {}",
                s.name,
                s.file_path,
                s.body_excerpt.as_deref().unwrap_or("")
            );
            let mut bag: HashMap<String, u32> = HashMap::new();
            for tok in camel_tokens(&text) {
                *bag.entry(tok).or_insert(0) += 1;
            }
            for w in bag.keys() {
                *df.entry(w.clone()).or_insert(0) += 1;
            }
            bags.push((s.id.clone(), bag));
        }
        let n_docs = bags.len() as f64;
        let idf: HashMap<String, f64> = df
            .into_iter()
            .map(|(w, c)| (w, ((n_docs + 1.0) / (c as f64 + 1.0)).ln() + 1.0))
            .collect();

        // Precompute each document's tf-idf norm `‖doc‖` (the `nrm` in bench.py).
        let docs = bags
            .into_iter()
            .map(|(id, bag)| {
                let norm: f64 = bag
                    .iter()
                    .map(|(w, &tf)| {
                        let w_idf = idf.get(w).copied().unwrap_or(1.0);
                        (tf as f64 * w_idf).powi(2)
                    })
                    .sum::<f64>()
                    .sqrt();
                let norm = if norm > 0.0 { norm } else { 1.0 };
                (id, bag, norm)
            })
            .collect();
        LexicalIndex { docs, idf }
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Score every document against `query`, returning `(symbol_id, score)`
    /// pairs for documents with a **positive** score, ranked descending (ties
    /// by symbol id). Mirrors the `bench.py` `signals` lexical computation.
    pub fn scores(&self, query: &str) -> Vec<(String, f64)> {
        use std::collections::HashMap;
        let mut qset: HashMap<String, u32> = HashMap::new();
        for tok in camel_tokens(query) {
            *qset.entry(tok).or_insert(0) += 1;
        }
        let mut scored: Vec<(String, f64)> = Vec::new();
        for (id, bag, norm) in &self.docs {
            let mut dot = 0.0;
            for (w, &qf) in &qset {
                if let Some(&tf) = bag.get(w) {
                    let w_idf = self.idf.get(w).copied().unwrap_or(1.0);
                    dot += qf as f64 * tf as f64 * w_idf.powi(2);
                }
            }
            if dot > 0.0 {
                scored.push((id.clone(), dot / norm));
            }
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored
    }

    /// The ranked symbol ids for `query`, best-first (positive-score documents
    /// only — the Python `argsort` places zero-score docs arbitrarily after the
    /// positives, which never affects Recall@k / MRR for resolvable queries).
    pub fn rank(&self, query: &str) -> Vec<String> {
        self.scores(query).into_iter().map(|(id, _)| id).collect()
    }
}

/// Rank symbols by **CSAR** forward-push PPR seeded from the lexical top-`seed_k`.
///
/// The Rust counterpart of the Python `CSAR` channel, made offline by seeding
/// from the tf-idf lexical layer instead of the (embedding-dependent) fused
/// signal. Diffuses over `graph` and returns symbols ranked by diffused mass
/// best-first. Returns an empty ranking when the query yields no lexical seed.
///
/// # Errors
/// Propagates the kernel's error on invalid `alpha`/`eps`.
pub fn rank_csar(
    lexical: &LexicalIndex,
    graph: &CodeGraph,
    query_text: &str,
    seed_k: usize,
    limit: usize,
    alpha: f64,
    eps: f64,
) -> Result<Vec<String>> {
    let seed_scores = lexical.scores(query_text);
    if seed_scores.is_empty() {
        return Ok(Vec::new());
    }
    let seeds: Vec<cognis_core::Hit> = seed_scores
        .into_iter()
        .take(seed_k)
        .map(|(id, score)| cognis_core::Hit::new(id, score, "lexical", "tf-idf seed"))
        .collect();
    let hits = diffuse_seed_hits(graph, &[seeds], limit, alpha, eps)?;
    Ok(hits.into_iter().map(|h| h.symbol_id).collect())
}

// ===========================================================================
// The fair harness.
// ===========================================================================

/// Outcome of running the fair harness over one golden set.
#[derive(Debug, Clone)]
pub struct HarnessReport {
    /// Number of symbols in the indexed DB.
    pub n_symbols: usize,
    /// Total golden queries in the set.
    pub n_queries: usize,
    /// Queries that resolved at least one relevant symbol (and were evaluated).
    pub n_eval: usize,
    /// Queries skipped because no relevant symbol resolved (mirrors the Python
    /// `[skip] no ground-truth resolved` path — an honest, expected outcome
    /// when the golden snapshot predates the indexed checkout).
    pub n_skipped: usize,
    /// Per-method aggregated metrics over the evaluated queries.
    pub methods: Vec<MethodAggregate>,
}

impl HarnessReport {
    /// The aggregate for `method`, if present.
    pub fn method(&self, method: &str) -> Option<&MethodAggregate> {
        self.methods.iter().find(|m| m.method == method)
    }
}

/// The objective-key fair harness over one indexed UCKG.
///
/// Holds the resident CSR graph, the symbol table (for label resolution) and the
/// hub set, all derived once from the DB, then evaluates a golden set across the
/// offline-reproducible methods.
pub struct FairHarness {
    graph: CodeGraph,
    symbols: SymbolTable,
    lexical: LexicalIndex,
    hubs: BTreeSet<String>,
    k: usize,
    seed_k: usize,
    hub_frac: f64,
    alpha: f64,
    eps: f64,
}

impl FairHarness {
    /// Build a harness over `db` with the default harness parameters (the same
    /// constants as the Python harness).
    ///
    /// # Errors
    /// Propagates store errors building the graph / symbol table.
    pub fn new(db: &Database) -> Result<Self> {
        let graph = db.build_code_graph(None)?;
        let symbols = SymbolTable::from_db(db)?;
        let lexical = LexicalIndex::from_db(db)?;
        let hubs = hub_ids(&graph, DEFAULT_HUB_FRAC);
        Ok(FairHarness {
            graph,
            symbols,
            lexical,
            hubs,
            k: DEFAULT_K,
            seed_k: DEFAULT_SEED_K,
            hub_frac: DEFAULT_HUB_FRAC,
            alpha: DEFAULT_ALPHA,
            eps: DEFAULT_EPS,
        })
    }

    /// Override the cutoff `k`.
    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Override the hub fraction (recomputes the hub set).
    pub fn with_hub_frac(mut self, frac: f64) -> Self {
        self.hub_frac = frac;
        self.hubs = hub_ids(&self.graph, frac);
        self
    }

    /// The resident code graph.
    pub fn graph(&self) -> &CodeGraph {
        &self.graph
    }

    /// The hub symbol-id set.
    pub fn hubs(&self) -> &BTreeSet<String> {
        &self.hubs
    }

    /// The symbol table used for label resolution.
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    /// Run the offline-reproducible methods (`lexical`, `csar`) over `golden`
    /// and aggregate the metrics.
    ///
    /// Queries whose labels resolve to no indexed symbol are skipped and counted
    /// in [`HarnessReport::n_skipped`] (never silently dropped or fabricated).
    /// `limit` for the rankings is the symbol count, so MRR sees the full order.
    ///
    /// # Errors
    /// Propagates store / kernel errors.
    pub fn run_offline(&self, golden: &GoldenSet) -> Result<HarnessReport> {
        let limit = self.symbols.len().max(self.k);
        let mut lexical: Vec<Metrics> = Vec::new();
        let mut csar: Vec<Metrics> = Vec::new();
        let mut skipped = 0usize;

        for query in &golden.queries {
            let relevant = self.symbols.resolve_relevant(query);
            if relevant.is_empty() {
                skipped += 1;
                continue;
            }
            let lex_order = self.lexical.rank(&query.q);
            lexical.push(compute_metrics(&lex_order, &relevant, &self.hubs, self.k));

            let csar_order = rank_csar(
                &self.lexical,
                &self.graph,
                &query.q,
                self.seed_k,
                limit,
                self.alpha,
                self.eps,
            )?;
            csar.push(compute_metrics(&csar_order, &relevant, &self.hubs, self.k));
        }

        let n_eval = lexical.len();
        Ok(HarnessReport {
            n_symbols: self.symbols.len(),
            n_queries: golden.queries.len(),
            n_eval,
            n_skipped: skipped,
            methods: vec![
                MethodAggregate::mean("lexical", &lexical),
                MethodAggregate::mean("csar", &csar),
            ],
        })
    }
}

// ===========================================================================
// Captured Python baseline + the non-regression gate (Property 5).
// ===========================================================================

/// The `(recall, mrr, contamination)` triple the non-regression gate compares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MethodScore {
    /// Recall@k (fraction).
    pub recall: f64,
    /// MRR.
    pub mrr: f64,
    /// Contamination@k (fraction).
    pub contamination: f64,
}

impl MethodScore {
    /// Convenience constructor.
    pub const fn new(recall: f64, mrr: f64, contamination: f64) -> Self {
        MethodScore {
            recall,
            mrr,
            contamination,
        }
    }
}

/// A captured Python objective PR-key result for the five harness methods.
///
/// These are the numbers the Rust engine must not regress against (the Python
/// oracle for Property 5). They are **EMPIRICALLY SUPPORTED on a finite sample**
/// (recorded in `.benchmarks/public/RESULTS.md`), never PROVEN — see
/// [`PythonBaseline::provenance`].
#[derive(Debug, Clone)]
pub struct PythonBaseline {
    /// Human-readable provenance / evidence-tier label.
    pub provenance: String,
    /// Number of evaluated queries behind these numbers.
    pub n: usize,
    /// Python `BM25` channel.
    pub bm25: MethodScore,
    /// Python `DENSE` channel.
    pub dense: MethodScore,
    /// Python `RRF` channel.
    pub rrf: MethodScore,
    /// Python `CSAR` channel.
    pub csar: MethodScore,
    /// Python `UNION` channel.
    pub union: MethodScore,
}

/// Captured balanced macro over 4 repos (168 objective PR queries) — the
/// canonical objective-key headline from `RESULTS.md`
/// ("Balanced macro + the structure-as-ranking question, settled").
///
/// EMPIRICALLY SUPPORTED (n=168, requests-dominated / Python-heavy; finite
/// sample, not a population estimate).
// The literals are recorded benchmark measurements, not mathematical constants
// (clippy::approx_constant otherwise flags e.g. 0.311 as ≈ 1/π).
#[allow(clippy::approx_constant)]
pub fn python_objective_macro() -> PythonBaseline {
    PythonBaseline {
        provenance: "RESULTS.md objective PR-key balanced macro \
                     (4 repos; EMPIRICALLY SUPPORTED, finite sample)"
            .to_string(),
        n: 168,
        bm25: MethodScore::new(0.383, 0.299, 0.067),
        dense: MethodScore::new(0.311, 0.337, 0.055),
        rrf: MethodScore::new(0.384, 0.364, 0.067),
        csar: MethodScore::new(0.401, 0.253, 0.311),
        union: MethodScore::new(0.359, 0.325, 0.060),
    }
}

/// Captured per-repo result for `psf/requests` (147 objective PR queries) — the
/// single statistically-robust large objective sample from `RESULTS.md`.
///
/// EMPIRICALLY SUPPORTED (n=147; the only individually large objective sample,
/// with a tight random-leakage CI).
// Recorded benchmark measurements, not mathematical constants (clippy otherwise
// flags e.g. 0.318 as ≈ 1/π).
#[allow(clippy::approx_constant)]
pub fn python_objective_requests() -> PythonBaseline {
    PythonBaseline {
        provenance: "RESULTS.md objective PR-key, psf/requests per-repo \
                     (EMPIRICALLY SUPPORTED, n=147)"
            .to_string(),
        n: 147,
        bm25: MethodScore::new(0.324, 0.316, 0.123),
        dense: MethodScore::new(0.256, 0.262, 0.046),
        rrf: MethodScore::new(0.315, 0.318, 0.095),
        csar: MethodScore::new(0.322, 0.231, 0.484),
        union: MethodScore::new(0.295, 0.320, 0.076),
    }
}

/// The non-regression gate (design Property 5 / P-Q-NOREG).
///
/// A Rust method does **not** regress against the Python oracle when, within the
/// configured noise band:
///
/// * `recall_rust ≥ recall_py − recall_noise`, and
/// * `mrr_rust ≥ mrr_py − mrr_noise`, and
/// * `contamination_rust ≤ contamination_py + contamination_noise`.
///
/// The noise band reflects that a benchmark pass on a finite sample is
/// empirically-supported, not exact: small differences are within measurement
/// noise. A failure on any axis is a regression that blocks removing Python at
/// K8 (Requirement 6.2).
#[derive(Debug, Clone, Copy)]
pub struct RegressionGate {
    /// Allowed recall shortfall (fraction).
    pub recall_noise: f64,
    /// Allowed MRR shortfall.
    pub mrr_noise: f64,
    /// Allowed contamination excess (fraction).
    pub contamination_noise: f64,
}

impl Default for RegressionGate {
    /// A 5-point (0.05) band on every axis — the default noise tolerance.
    fn default() -> Self {
        RegressionGate {
            recall_noise: 0.05,
            mrr_noise: 0.05,
            contamination_noise: 0.05,
        }
    }
}

/// The gate's verdict for one method comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum GateVerdict {
    /// No regression on any axis.
    Pass,
    /// At least one axis regressed; carries human-readable reasons.
    Regress(Vec<String>),
}

impl GateVerdict {
    /// `true` when no axis regressed.
    pub fn is_pass(&self) -> bool {
        matches!(self, GateVerdict::Pass)
    }

    /// The regression reasons (empty on pass).
    pub fn reasons(&self) -> &[String] {
        match self {
            GateVerdict::Regress(r) => r,
            GateVerdict::Pass => &[],
        }
    }
}

impl RegressionGate {
    /// Evaluate Property 5 for one method: does `rust` reproduce `python` within
    /// the noise band on recall, MRR and contamination?
    pub fn evaluate(&self, rust: MethodScore, python: MethodScore) -> GateVerdict {
        let mut reasons = Vec::new();
        if rust.recall < python.recall - self.recall_noise {
            reasons.push(format!(
                "Recall@k regressed: rust {:.3} < python {:.3} − {:.3} noise",
                rust.recall, python.recall, self.recall_noise
            ));
        }
        if rust.mrr < python.mrr - self.mrr_noise {
            reasons.push(format!(
                "MRR regressed: rust {:.3} < python {:.3} − {:.3} noise",
                rust.mrr, python.mrr, self.mrr_noise
            ));
        }
        if rust.contamination > python.contamination + self.contamination_noise {
            reasons.push(format!(
                "Contamination@k regressed: rust {:.3} > python {:.3} + {:.3} noise",
                rust.contamination, python.contamination, self.contamination_noise
            ));
        }
        if reasons.is_empty() {
            GateVerdict::Pass
        } else {
            GateVerdict::Regress(reasons)
        }
    }
}

// ===========================================================================
// The non-code-artifact-coverage non-regression gate (Requirements 14/15/16).
//
// This is a SECOND, distinct decision layer, additive to the Property-5
// `RegressionGate` above. Where `RegressionGate` compares a Rust method to a
// captured Python oracle within a symmetric noise band, this layer decides
// ship/no-ship for the *artifact-coverage* feature: it blocks any candidate
// that regresses code MRR / Contamination@k beyond a pre-declared tolerance ε,
// or that fails to strictly improve the non-code Coverage and Recall@10, and
// permits only when code is preserved AND non-code strictly improves.
// ===========================================================================

/// The fixed, pre-declared non-regression tolerance ε for the coverage gate
/// (Requirement 14.1 / 16.4).
///
/// ε is expressed in the **same absolute units** as code MRR and code
/// Contamination@k (both fractions in `[0, 1]`), declared here as a compile-time
/// constant *before any measurement is taken* and never tuned to a benchmark
/// sample. A candidate is blocked when `ΔMRR < −ε` or `ΔContam > +ε`.
pub const DEFAULT_COVERAGE_EPSILON: f64 = 0.01;

/// The evidence tier recorded for the coverage-improvement claim
/// (Requirements 16.1 / 16.3).
///
/// The claim ships **conjectured** until a passing before/after measurement is
/// produced under the gate; a permit records it **verified**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    /// The improvement claim is backed by a passing before/after measurement
    /// under the gate (recorded on a permit — Requirement 16.3).
    Verified,
    /// The improvement claim is not (yet) backed by a passing measurement
    /// (the default / recorded on any block — Requirement 16.1).
    Conjectured,
}

/// Code-retrieval metrics measured on the Code_Golden_Sets at the gate's fixed
/// cutoff `k` (= [`DEFAULT_K`], 10), for one build (Requirement 14.2).
///
/// Both `mrr` and `contamination` are the code MRR and code Contamination@k
/// measured with identical golden sets, benchmark repos, and `k` for the
/// baseline and the candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodeMetrics {
    /// Code MRR on the Code_Golden_Sets.
    pub mrr: f64,
    /// Code Contamination@k on the Code_Golden_Sets.
    pub contamination: f64,
}

impl CodeMetrics {
    /// Convenience constructor.
    pub const fn new(mrr: f64, contamination: f64) -> Self {
        CodeMetrics { mrr, contamination }
    }
}

/// Mean non-code metrics across the md/yaml/html/sql file types for one build
/// (Requirement 14.4 / 14.5).
///
/// `coverage` is the mean non-code Coverage_Metric and `recall` the mean
/// non-code Recall@k, each averaged across the four non-code file types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NonCodeMetrics {
    /// Mean non-code Coverage_Metric across md/yaml/html/sql.
    pub coverage: f64,
    /// Mean non-code Recall@k across md/yaml/html/sql.
    pub recall: f64,
}

impl NonCodeMetrics {
    /// Convenience constructor.
    pub const fn new(coverage: f64, recall: f64) -> Self {
        NonCodeMetrics { coverage, recall }
    }
}

/// A complete measurement of one build: its code metrics on the Code_Golden_Sets
/// and its mean non-code metrics across md/yaml/html/sql.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverageMeasurement {
    /// Code MRR + Contamination@k on the Code_Golden_Sets.
    pub code: CodeMetrics,
    /// Mean non-code Coverage + Recall@k across md/yaml/html/sql.
    pub non_code: NonCodeMetrics,
}

impl CoverageMeasurement {
    /// Convenience constructor.
    pub const fn new(code: CodeMetrics, non_code: NonCodeMetrics) -> Self {
        CoverageMeasurement { code, non_code }
    }
}

/// The gate's inputs: the baseline and candidate measurements.
///
/// Each side is an [`Option`] so that an **unmeasured** build is represented
/// explicitly; the gate blocks when either side is unmeasured (Requirement
/// 16.2). Both measurements must have been taken with identical golden sets,
/// repos, and `k` (Requirement 14.2) — a precondition the caller establishes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CoverageGateInput {
    /// The pre-change baseline measurement, or `None` if unmeasured.
    pub baseline: Option<CoverageMeasurement>,
    /// The candidate build measurement, or `None` if unmeasured.
    pub candidate: Option<CoverageMeasurement>,
}

impl CoverageGateInput {
    /// A fully-measured input (both baseline and candidate present).
    pub const fn measured(baseline: CoverageMeasurement, candidate: CoverageMeasurement) -> Self {
        CoverageGateInput {
            baseline: Some(baseline),
            candidate: Some(candidate),
        }
    }
}

/// The coverage gate's verdict.
///
/// `Permit` records the improvement claim as [`ClaimStatus::Verified`];
/// `Block` (carrying the human-readable reasons, each naming the failed axis)
/// leaves the claim [`ClaimStatus::Conjectured`].
#[derive(Debug, Clone, PartialEq)]
pub enum CoverageGateVerdict {
    /// Code preserved within ε AND both non-code metrics strictly improved
    /// (Requirement 14.5). The improvement claim is recorded verified.
    Permit,
    /// At least one gate condition failed; carries reasons naming each failed
    /// axis. The improvement claim stays conjectured; the baseline is left
    /// unchanged (this decision function is side-effect-free).
    Block(Vec<String>),
}

impl CoverageGateVerdict {
    /// `true` when the release is permitted.
    pub fn is_permit(&self) -> bool {
        matches!(self, CoverageGateVerdict::Permit)
    }

    /// The block reasons (empty on permit).
    pub fn reasons(&self) -> &[String] {
        match self {
            CoverageGateVerdict::Block(r) => r,
            CoverageGateVerdict::Permit => &[],
        }
    }

    /// The evidence tier this verdict records for the improvement claim:
    /// verified on permit (Requirement 16.3), conjectured on block
    /// (Requirement 16.1).
    pub fn claim_status(&self) -> ClaimStatus {
        match self {
            CoverageGateVerdict::Permit => ClaimStatus::Verified,
            CoverageGateVerdict::Block(_) => ClaimStatus::Conjectured,
        }
    }
}

/// The hard non-regression gate for the non-code-artifact-coverage feature
/// (Requirements 14, 15, 16).
///
/// A pure, side-effect-free decision function comparing a baseline and a
/// candidate measurement. It **blocks** when:
///
/// * either build is unmeasured (Requirement 16.2), or
/// * `ΔMRR < −ε` (code MRR regressed beyond ε) — naming the MRR axis
///   (Requirements 14.3 / 15.3), or
/// * `ΔContam > +ε` (code Contamination@k rose beyond ε) — naming the
///   Contamination axis (Requirements 14.3 / 15.3 / 15.4), or
/// * candidate mean non-code Coverage does not **strictly** exceed baseline, or
/// * candidate mean non-code Recall@k does not **strictly** exceed baseline
///   (Requirement 14.4).
///
/// It **permits** only when both non-code metrics strictly improve AND
/// `ΔMRR ≥ −ε` AND `ΔContam ≤ +ε` (Requirement 14.5). Because the function is
/// pure it never mutates or persists the baseline; on a block the caller simply
/// leaves the recorded baseline unchanged (Requirements 15.4 / 16).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverageRegressionGate {
    /// The fixed, pre-declared non-negative tolerance ε (Requirement 14.1),
    /// expressed in the same absolute units as code MRR and Contamination@k.
    pub epsilon: f64,
}

impl Default for CoverageRegressionGate {
    /// The default gate uses the pre-declared [`DEFAULT_COVERAGE_EPSILON`]
    /// (0.01), never tuned to a sample.
    fn default() -> Self {
        CoverageRegressionGate {
            epsilon: DEFAULT_COVERAGE_EPSILON,
        }
    }
}

impl CoverageRegressionGate {
    /// Decide ship/no-ship for a candidate against a baseline.
    ///
    /// Pure and side-effect-free: on a block the baseline is left untouched
    /// (the caller must not persist anything — Requirements 15.4 / 16). The
    /// returned verdict's [`CoverageGateVerdict::claim_status`] records the
    /// improvement claim as verified on permit and conjectured on block.
    pub fn decide(&self, input: &CoverageGateInput) -> CoverageGateVerdict {
        // Block when unmeasured (Requirement 16.2): a missing baseline or
        // candidate measurement cannot support a verified claim.
        let (base, cand) = match (input.baseline, input.candidate) {
            (Some(b), Some(c)) => (b, c),
            (None, None) => {
                return CoverageGateVerdict::Block(vec![
                    "unmeasured: neither baseline nor candidate metrics were measured".to_string(),
                ]);
            }
            (None, Some(_)) => {
                return CoverageGateVerdict::Block(vec![
                    "unmeasured: baseline metrics were not measured".to_string(),
                ]);
            }
            (Some(_), None) => {
                return CoverageGateVerdict::Block(vec![
                    "unmeasured: candidate metrics were not measured".to_string(),
                ]);
            }
        };

        let mut reasons = Vec::new();

        // Code non-regression on the Code_Golden_Sets (Requirements 14.3 /
        // 15.3 / 15.4), each reason naming the failed axis.
        let d_mrr = cand.code.mrr - base.code.mrr;
        let d_contam = cand.code.contamination - base.code.contamination;
        if d_mrr < -self.epsilon {
            reasons.push(format!(
                "code MRR regressed: ΔMRR {d_mrr:.4} < −ε ({:.4}) \
                 (candidate {:.4} vs baseline {:.4})",
                self.epsilon, cand.code.mrr, base.code.mrr
            ));
        }
        if d_contam > self.epsilon {
            reasons.push(format!(
                "code Contamination@k regressed: ΔContam {d_contam:.4} > +ε ({:.4}) \
                 (candidate {:.4} vs baseline {:.4})",
                self.epsilon, cand.code.contamination, base.code.contamination
            ));
        }

        // Non-code metrics must STRICTLY improve (Requirement 14.4). Using `<=`
        // means "does not strictly exceed" → block.
        if cand.non_code.coverage <= base.non_code.coverage {
            reasons.push(format!(
                "non-code Coverage did not strictly improve: \
                 candidate {:.4} <= baseline {:.4}",
                cand.non_code.coverage, base.non_code.coverage
            ));
        }
        if cand.non_code.recall <= base.non_code.recall {
            reasons.push(format!(
                "non-code Recall@k did not strictly improve: \
                 candidate {:.4} <= baseline {:.4}",
                cand.non_code.recall, base.non_code.recall
            ));
        }

        if reasons.is_empty() {
            // Both non-code strictly improve AND ΔMRR ≥ −ε AND ΔContam ≤ +ε
            // (Requirement 14.5): permit, recording the claim verified.
            CoverageGateVerdict::Permit
        } else {
            // Block, leaving the baseline unchanged; claim stays conjectured.
            CoverageGateVerdict::Block(reasons)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn set(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // ---- metrics --------------------------------------------------------

    #[test]
    fn metrics_match_hand_computed_example() {
        // order: a b c d e ; relevant {a, d, x} ; hubs {b} ; k = 3.
        let order = ids(&["a", "b", "c", "d", "e"]);
        let rel = set(&["a", "d", "x"]);
        let hubs = set(&["b"]);
        let m = compute_metrics(&order, &rel, &hubs, 3);
        // top-3 = a,b,c → 1 relevant (a) of 3 total → recall 1/3, prec 1/3.
        assert!((m.recall - 1.0 / 3.0).abs() < 1e-12);
        assert!((m.precision - 1.0 / 3.0).abs() < 1e-12);
        // first relevant is at rank 1 → mrr 1.0.
        assert!((m.mrr - 1.0).abs() < 1e-12);
        // 1 hub (b) in top-3 → contamination 1/3.
        assert!((m.contamination - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn mrr_uses_full_order_not_just_topk() {
        // Only relevant item is at rank 4, beyond k=2 → recall@2 = 0 but
        // mrr = 1/4 (full-order scan, matching the Python harness).
        let order = ids(&["a", "b", "c", "d"]);
        let rel = set(&["d"]);
        let hubs = BTreeSet::new();
        let m = compute_metrics(&order, &rel, &hubs, 2);
        assert_eq!(m.recall, 0.0);
        assert!((m.mrr - 0.25).abs() < 1e-12);
    }

    #[test]
    fn metrics_bounds_hold() {
        // Property-style: metrics are always in [0,1] for arbitrary-ish inputs.
        let order = ids(&["a", "b", "c", "d", "e", "f"]);
        for k in 1..=8 {
            for rel in [set(&[]), set(&["a"]), set(&["c", "f"]), set(&["z"])] {
                let hubs = set(&["b", "e"]);
                let m = compute_metrics(&order, &rel, &hubs, k);
                for v in [m.recall, m.precision, m.mrr, m.contamination] {
                    assert!((0.0..=1.0).contains(&v), "metric out of range: {v}");
                }
            }
        }
    }

    #[test]
    fn aggregate_is_the_mean() {
        let per = [
            Metrics {
                recall: 1.0,
                precision: 0.5,
                mrr: 1.0,
                contamination: 0.0,
            },
            Metrics {
                recall: 0.0,
                precision: 0.5,
                mrr: 0.0,
                contamination: 0.2,
            },
        ];
        let agg = MethodAggregate::mean("m", &per);
        assert_eq!(agg.n_eval, 2);
        assert!((agg.recall - 0.5).abs() < 1e-12);
        assert!((agg.precision - 0.5).abs() < 1e-12);
        assert!((agg.mrr - 0.5).abs() < 1e-12);
        assert!((agg.contamination - 0.1).abs() < 1e-12);
    }

    // ---- golden parsing + resolution ------------------------------------

    #[test]
    fn parse_golden_and_resolve_labels() {
        let json = r#"{
            "repo": "repos/x",
            "queries": [
                {"q": "Fix prepare_body", "sha": "abc",
                 "relevant": [["prepare_body", "src/requests/models.py"]]},
                {"q": "bad", "relevant": [["only-one"], "notarray"]}
            ]
        }"#;
        let g = parse_golden_set(json).unwrap();
        assert_eq!(g.repo.as_deref(), Some("repos/x"));
        assert_eq!(g.queries.len(), 2);
        assert_eq!(g.queries[0].relevant.len(), 1);
        // malformed relevant entries are dropped.
        assert_eq!(g.queries[1].relevant.len(), 0);

        let table = SymbolTable {
            rows: vec![
                (
                    "python:src/requests/models.py:PreparedRequest.prepare_body@h".into(),
                    "prepare_body".into(),
                    "src/requests/models.py".into(),
                ),
                (
                    "python:src/other.py:prepare_body@h2".into(),
                    "prepare_body".into(),
                    "src/other.py".into(),
                ),
            ],
        };
        let rel = table.resolve_relevant(&g.queries[0]);
        // Only the models.py symbol matches the (name, path_sub) label.
        assert_eq!(rel.len(), 1);
        assert!(rel
            .iter()
            .next()
            .unwrap()
            .contains("models.py:PreparedRequest.prepare_body"));
    }

    #[test]
    fn resolve_is_path_separator_and_case_insensitive() {
        let table = SymbolTable {
            rows: vec![(
                "python:src/requests/models.py:Response@h".into(),
                "Response".into(),
                "src/requests/models.py".into(),
            )],
        };
        // Backslashes and different case in the label still match.
        assert_eq!(
            table.resolve("Response", "SRC\\Requests\\Models.py").len(),
            1
        );
        assert_eq!(table.resolve("Response", "models.py").len(), 1);
        assert_eq!(table.resolve("Response", "no/such/path").len(), 0);
        assert_eq!(table.resolve("Other", "models.py").len(), 0);
    }

    #[test]
    fn missing_queries_array_errors() {
        assert!(parse_golden_set(r#"{"repo":"x"}"#).is_err());
        assert!(parse_golden_set("not json").is_err());
    }

    // ---- hubs -----------------------------------------------------------

    #[test]
    fn hub_ids_picks_highest_degree_top_fraction() {
        let g = CodeGraph {
            indptr: vec![0],
            indices: vec![],
            weights: vec![],
            degree: vec![5.0, 1.0, 9.0, 2.0, 7.0, 3.0, 8.0, 4.0, 6.0, 0.5],
            node_ids: (0..10).map(|i| format!("s{i}")).collect(),
            index: (0..10).map(|i| (format!("s{i}"), i)).collect(),
        };
        // 10 nodes, frac 0.10 → top 1 by degree = node 2 (degree 9).
        let hubs = hub_ids(&g, 0.10);
        assert_eq!(hubs, set(&["s2"]));
        // frac 0.30 → top 3 = nodes 2,6,4 (9,8,7).
        let hubs3 = hub_ids(&g, 0.30);
        assert_eq!(hubs3, set(&["s2", "s4", "s6"]));
    }

    #[test]
    fn hub_ids_at_least_one_and_empty_graph_safe() {
        let g = CodeGraph {
            indptr: vec![0, 1],
            indices: vec![0],
            weights: vec![1.0],
            degree: vec![1.0],
            node_ids: vec!["only".into()],
            index: [("only".to_string(), 0)].into_iter().collect(),
        };
        // tiny frac still yields at least one hub.
        assert_eq!(hub_ids(&g, 0.0001), set(&["only"]));

        let empty = CodeGraph {
            indptr: vec![0],
            indices: vec![],
            weights: vec![],
            degree: vec![],
            node_ids: vec![],
            index: Default::default(),
        };
        assert!(hub_ids(&empty, 0.10).is_empty());
    }

    // ---- tokenization + tf-idf lexical ----------------------------------

    #[test]
    fn camel_tokens_split_acronyms_and_drop_short() {
        // "HTTPAdapter.send" → http, adapter, send; single-char dropped; a
        // digit-leading run ("1672") is not a token (must start with a letter).
        assert_eq!(
            camel_tokens("Fix HTTPAdapter.send (#1672)"),
            vec!["fix", "http", "adapter", "send"]
        );
        assert!(camel_tokens("   ##  - !").is_empty());
        assert_eq!(camel_tokens("parse_cors"), vec!["parse", "cors"]);
    }

    #[test]
    fn lexical_index_ranks_token_overlap_first() {
        let symbols = vec![
            sym_doc(
                "py:m.py:prepare_body@h",
                "prepare_body",
                "src/m.py",
                "prepare the body",
            ),
            sym_doc(
                "py:m.py:Response@h2",
                "Response",
                "src/m.py",
                "the response object",
            ),
            sym_doc(
                "py:u.py:helper@h3",
                "helper",
                "src/u.py",
                "unrelated helper",
            ),
        ];
        let idx = LexicalIndex::from_symbols(&symbols);
        assert_eq!(idx.len(), 3);
        let ranked = idx.rank("Fix prepare_body");
        // The symbol whose name/body carries the query tokens ranks first.
        assert_eq!(
            ranked.first().map(String::as_str),
            Some("py:m.py:prepare_body@h")
        );
        // A query with no shared token returns no positive-score document.
        assert!(idx.rank("zzz qqq").is_empty());
    }

    fn sym_doc(id: &str, name: &str, file_path: &str, body: &str) -> cognis_core::Symbol {
        cognis_core::Symbol {
            id: id.into(),
            kind: cognis_core::SymbolKind::Function,
            name: name.into(),
            qualified_name: name.into(),
            language: "python".into(),
            module: "m".into(),
            file_path: file_path.into(),
            line_start: 1,
            line_end: 2,
            signature: None,
            docstring: None,
            content_hash: "h".into(),
            body_excerpt: Some(body.into()),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: vec![],
            updated_at: 0,
        }
    }

    // ---- gate -----------------------------------------------------------

    #[test]
    fn gate_passes_within_noise_band() {
        let gate = RegressionGate::default();
        let python = MethodScore::new(0.50, 0.45, 0.07);
        // Rust slightly worse on recall/mrr, slightly more contam — all within 0.05.
        let rust = MethodScore::new(0.46, 0.41, 0.11);
        assert_eq!(gate.evaluate(rust, python), GateVerdict::Pass);
        // Rust strictly better passes trivially.
        assert!(gate
            .evaluate(MethodScore::new(0.9, 0.9, 0.0), python)
            .is_pass());
    }

    #[test]
    fn gate_flags_each_regressed_axis() {
        let gate = RegressionGate::default();
        let python = MethodScore::new(0.50, 0.45, 0.07);

        // Recall far below band.
        let v = gate.evaluate(MethodScore::new(0.30, 0.45, 0.07), python);
        assert!(!v.is_pass());
        assert!(v.reasons().iter().any(|r| r.contains("Recall@k")));

        // MRR below band.
        let v = gate.evaluate(MethodScore::new(0.50, 0.20, 0.07), python);
        assert!(v.reasons().iter().any(|r| r.contains("MRR")));

        // Contamination above band.
        let v = gate.evaluate(MethodScore::new(0.50, 0.45, 0.30), python);
        assert!(v.reasons().iter().any(|r| r.contains("Contamination@k")));

        // All three at once.
        let v = gate.evaluate(MethodScore::new(0.10, 0.10, 0.40), python);
        assert_eq!(v.reasons().len(), 3);
    }

    #[test]
    fn captured_python_baselines_are_well_formed() {
        for b in [python_objective_macro(), python_objective_requests()] {
            assert!(b.n > 0);
            assert!(!b.provenance.is_empty());
            for s in [b.bm25, b.dense, b.rrf, b.csar, b.union] {
                for v in [s.recall, s.mrr, s.contamination] {
                    assert!((0.0..=1.0).contains(&v));
                }
            }
        }
    }

    // ---- coverage / non-regression gate (Req 14/15/16) ------------------

    /// Build a measurement from `(code_mrr, code_contam, nc_coverage, nc_recall)`.
    fn meas(mrr: f64, contam: f64, cov: f64, rec: f64) -> CoverageMeasurement {
        CoverageMeasurement::new(CodeMetrics::new(mrr, contam), NonCodeMetrics::new(cov, rec))
    }

    #[test]
    fn coverage_gate_permits_when_noncode_improves_and_code_within_epsilon() {
        let gate = CoverageRegressionGate::default();
        // Baseline: no non-code coverage/recall. Candidate: both strictly up,
        // code MRR down by 0.005 (within ε, ΔMRR ≥ −ε) and contam up by 0.005
        // (within ε, ΔContam ≤ +ε).
        let base = meas(0.45, 0.07, 0.00, 0.00);
        let cand = meas(0.445, 0.075, 0.30, 0.25);
        let input = CoverageGateInput::measured(base, cand);
        let v = gate.decide(&input);
        assert_eq!(v, CoverageGateVerdict::Permit);
        // On permit the claim is recorded verified (Req 16.3).
        assert_eq!(v.claim_status(), ClaimStatus::Verified);
    }

    #[test]
    fn coverage_gate_blocks_on_mrr_regression_naming_axis() {
        let gate = CoverageRegressionGate::default(); // ε = 0.01
                                                      // ΔMRR = 0.40 − 0.45 = −0.05 < −ε → block naming MRR. Non-code improves.
        let base = meas(0.45, 0.07, 0.00, 0.00);
        let cand = meas(0.40, 0.07, 0.30, 0.25);
        let v = gate.decide(&CoverageGateInput::measured(base, cand));
        assert!(!v.is_permit());
        assert!(v.reasons().iter().any(|r| r.contains("MRR")));
        assert!(v.reasons().iter().any(|r| r.contains("ΔMRR")));
        assert_eq!(v.claim_status(), ClaimStatus::Conjectured);
    }

    #[test]
    fn coverage_gate_blocks_on_contamination_regression_naming_axis() {
        let gate = CoverageRegressionGate::default();
        // ΔContam = 0.20 − 0.07 = 0.13 > +ε → block naming Contamination.
        let base = meas(0.45, 0.07, 0.00, 0.00);
        let cand = meas(0.45, 0.20, 0.30, 0.25);
        let v = gate.decide(&CoverageGateInput::measured(base, cand));
        assert!(!v.is_permit());
        assert!(v.reasons().iter().any(|r| r.contains("Contamination")));
        assert!(v.reasons().iter().any(|r| r.contains("ΔContam")));
    }

    #[test]
    fn coverage_gate_blocks_when_coverage_not_strictly_improved() {
        let gate = CoverageRegressionGate::default();
        // Coverage equal to baseline (not strictly greater) → block. Recall up,
        // code preserved.
        let base = meas(0.45, 0.07, 0.30, 0.20);
        let cand = meas(0.45, 0.07, 0.30, 0.25);
        let v = gate.decide(&CoverageGateInput::measured(base, cand));
        assert!(!v.is_permit());
        assert!(v.reasons().iter().any(|r| r.contains("Coverage")));
    }

    #[test]
    fn coverage_gate_blocks_when_recall_not_strictly_improved() {
        let gate = CoverageRegressionGate::default();
        // Recall equal to baseline (not strictly greater) → block. Coverage up.
        let base = meas(0.45, 0.07, 0.20, 0.25);
        let cand = meas(0.45, 0.07, 0.30, 0.25);
        let v = gate.decide(&CoverageGateInput::measured(base, cand));
        assert!(!v.is_permit());
        assert!(v.reasons().iter().any(|r| r.contains("Recall")));
    }

    #[test]
    fn coverage_gate_blocks_when_unmeasured() {
        let gate = CoverageRegressionGate::default();
        let good = meas(0.45, 0.07, 0.30, 0.25);

        // Neither side measured.
        let v = gate.decide(&CoverageGateInput::default());
        assert!(!v.is_permit());
        assert!(v.reasons().iter().any(|r| r.contains("unmeasured")));
        assert_eq!(v.claim_status(), ClaimStatus::Conjectured);

        // Baseline missing.
        let v = gate.decide(&CoverageGateInput {
            baseline: None,
            candidate: Some(good),
        });
        assert!(v.reasons().iter().any(|r| r.contains("baseline")));

        // Candidate missing.
        let v = gate.decide(&CoverageGateInput {
            baseline: Some(good),
            candidate: None,
        });
        assert!(v.reasons().iter().any(|r| r.contains("candidate")));
    }

    #[test]
    fn coverage_gate_reports_every_failed_axis_at_once() {
        let gate = CoverageRegressionGate::default();
        // Code MRR down > ε, contam up > ε, and neither non-code metric improves.
        let base = meas(0.50, 0.07, 0.30, 0.25);
        let cand = meas(0.40, 0.20, 0.30, 0.25);
        let v = gate.decide(&CoverageGateInput::measured(base, cand));
        assert!(!v.is_permit());
        // MRR + Contamination + Coverage + Recall = 4 reasons.
        assert_eq!(v.reasons().len(), 4);
    }

    #[test]
    fn coverage_epsilon_is_the_fixed_pre_declared_constant() {
        // ε is the pre-declared default, not sample-derived (Req 14.1 / 16.4).
        assert_eq!(DEFAULT_COVERAGE_EPSILON, 0.01);
        assert_eq!(
            CoverageRegressionGate::default().epsilon,
            DEFAULT_COVERAGE_EPSILON
        );
    }
}
