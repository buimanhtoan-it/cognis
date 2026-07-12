//! Structural-understanding eval — does the engine *understand code structure*?
//!
//! The fair harness ([`crate::bench`]) answers "can the engine find the right
//! symbols for a query" (Recall@k / MRR over PR-derived golden). This module
//! answers a different, complementary question that keyword/semantic retrieval
//! does not: **does the engine correctly recover the relationships between
//! symbols** — who calls whom, what imports what — the call/dependency graph a
//! human reads code to build. That is the substance behind the `dependency_trace`
//! MCP tool, and it had no quality instrument until now.
//!
//! ## Three evidence tiers (honest by construction, never fabricated)
//!
//! Mirroring the discipline of the fair harness and `docs/development-criteria.md`
//! (label every number **proven** / **empirically supported** / **conjectured**,
//! and never invent data), this module measures three things at three tiers:
//!
//! 1. **Structural coverage** (`CoverageStats`) — *descriptive*, always
//!    computable from the engine's own extracted graph, on real repos. How much
//!    structure did the indexer actually recover: the **edge resolution rate**
//!    (fraction of edges linked to a real indexed callee vs a dangling
//!    reference), **connectivity** (fraction of symbols on ≥1 resolved edge),
//!    average out-degree, and the per-`EdgeKind` breakdown. These are not a
//!    correctness *claim* — they are a coverage *trend* to benchmark
//!    release-over-release (a drop in resolution rate is an indexer regression).
//!    Evidence tier: **empirically supported (descriptive)**, quote the repo + n.
//!
//! 2. **Edge recall vs an independent golden** (`EdgeGoldenResult`) — the real
//!    comprehension score. Given a hand-authored, source-verified set of true
//!    edges (`(src, dst)` symbol labels, resolved the same `(name, path)` way as
//!    the retrieval golden), what fraction did the engine extract (**edge
//!    recall**), and — only when the golden is marked `complete` for its source
//!    nodes — the **precision** over those nodes. Runs only when a golden edge
//!    file is present; otherwise skipped with a clear message (never a fabricated
//!    number). Evidence tier: **empirically supported**, quote n = golden edges.
//!
//! 3. **`dependency_trace` reachability mechanics** (`reachable_within` /
//!    `reachability_recall`) — that the depth-bounded directed BFS the tool runs
//!    is *correct*: on a graph whose reachability is known by construction, the
//!    trace reaches exactly the right set. Evidence tier: **proven by
//!    construction** (unit-tested on synthetic graphs).
//!
//! Everything here is pure/deterministic and expressed in **symbol-id space**,
//! so it is robust across index builds and unit-testable without any corpus. The
//! serializable [`StructureReport`] persists the result (with repo provenance)
//! for the benchmark trend.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use cognis_core::{CognisError, Edge, EdgeKind, Symbol};
use cognis_store::Database;
use serde::Serialize;

pub use cognis_core::Result;

/// Default trace depth for reachability (mirrors the mcpd `caps.clamp_depth`
/// default of 3).
pub const DEFAULT_DEPTH: u8 = 3;

/// The report/artifact schema version (bump on a breaking field change so old
/// committed baselines are recognizably stale).
pub const SCHEMA_VERSION: u32 = 1;

// ===========================================================================
// Directed edge helpers.
// ===========================================================================

/// The stable snake_case label of an [`EdgeKind`] (matches the on-disk / MCP
/// serialization, so report `by_kind` keys are the same strings agents see).
pub fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Implements => "implements",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::RoutesTo => "routes_to",
        EdgeKind::Tests => "tests",
    }
}

/// A resolved edge is one whose callee/destination links to a real indexed
/// symbol — i.e. `meta.dst_missing` is not set. Dangling edges (the resolver saw
/// a reference but could not bind it to a definition) are the structural-layer's
/// "I don't understand this yet" marker.
fn is_resolved(edge: &Edge) -> bool {
    !edge.dst_missing()
}

/// The set of resolved directed `(src_id, dst_id)` edge pairs the engine
/// extracted, optionally filtered to a single [`EdgeKind`] (e.g. `Calls` for a
/// call-graph comprehension check). Dangling edges are excluded — they are not
/// claims about a real relationship.
pub fn resolved_edge_pairs(edges: &[Edge], kind: Option<EdgeKind>) -> BTreeSet<(String, String)> {
    edges
        .iter()
        .filter(|e| is_resolved(e))
        .filter(|e| match kind {
            Some(k) => e.kind == k,
            None => true,
        })
        .map(|e| (e.src_id.clone(), e.dst_id.clone()))
        .collect()
}

/// Build the out-direction adjacency map (`src_id -> [dst_id, ...]`) over the
/// resolved edges, faithful to `StoreEngine::dependency_trace`'s `"out"` walk
/// (it iterates `list_edges()` and skips `dst_missing`). Used for reachability.
pub fn out_adjacency(edges: &[Edge]) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for e in edges.iter().filter(|e| is_resolved(e)) {
        adj.entry(e.src_id.clone())
            .or_default()
            .push(e.dst_id.clone());
    }
    adj
}

/// The set of symbols reachable from `start` within `depth` hops over `adj`,
/// **excluding** `start` — exactly the node set `dependency_trace(start, "out",
/// depth)` returns (BFS by hop, first-visit wins). Deterministic.
pub fn reachable_within(
    adj: &HashMap<String, Vec<String>>,
    start: &str,
    depth: u8,
) -> BTreeSet<String> {
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(start.to_string());
    let mut frontier: VecDeque<String> = VecDeque::new();
    frontier.push_back(start.to_string());
    let mut reached: BTreeSet<String> = BTreeSet::new();

    for _hop in 1..=depth {
        let mut next: VecDeque<String> = VecDeque::new();
        while let Some(node) = frontier.pop_front() {
            if let Some(neighbors) = adj.get(&node) {
                for n in neighbors {
                    if visited.insert(n.clone()) {
                        reached.insert(n.clone());
                        next.push_back(n.clone());
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    reached
}

// ===========================================================================
// Metrics — pure functions.
// ===========================================================================

/// Precision / recall / F1 of a predicted edge set against a golden edge set,
/// with the raw counts. All rates are in `[0, 1]`; an empty golden yields
/// `recall = 0`, an empty prediction yields `precision = 0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct EdgeSetMetrics {
    /// `matched / predicted` (fraction of predicted edges that are true).
    pub precision: f64,
    /// `matched / golden` (fraction of true edges the engine extracted).
    pub recall: f64,
    /// Harmonic mean of precision and recall (0 when either is 0).
    pub f1: f64,
    /// Number of predicted edges.
    pub predicted: usize,
    /// Number of golden edges.
    pub golden: usize,
    /// Number of golden edges that are also predicted (the intersection size).
    pub matched: usize,
}

/// Compute [`EdgeSetMetrics`] for `predicted` vs `golden` edge-pair sets.
pub fn edge_set_metrics(
    predicted: &BTreeSet<(String, String)>,
    golden: &BTreeSet<(String, String)>,
) -> EdgeSetMetrics {
    let matched = golden.iter().filter(|e| predicted.contains(*e)).count();
    let precision = if predicted.is_empty() {
        0.0
    } else {
        matched as f64 / predicted.len() as f64
    };
    let recall = if golden.is_empty() {
        0.0
    } else {
        matched as f64 / golden.len() as f64
    };
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    EdgeSetMetrics {
        precision,
        recall,
        f1,
        predicted: predicted.len(),
        golden: golden.len(),
        matched,
    }
}

/// Reachability recall: of the golden `(src, dst)` pairs, the fraction whose
/// `dst` is reachable from `src` within `depth` hops over `adj`. This is the
/// `dependency_trace`-level comprehension metric (can the tool actually surface
/// a known dependency), distinct from single-hop edge recall.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReachMetrics {
    /// `reached / pairs`.
    pub recall: f64,
    /// Number of golden dependency pairs evaluated.
    pub pairs: usize,
    /// Number reached within `depth`.
    pub reached: usize,
    /// The trace depth used.
    pub depth: u8,
}

/// Compute [`ReachMetrics`] over `golden_pairs` at `depth`. Groups by source so
/// each source's BFS runs once.
pub fn reachability_recall(
    adj: &HashMap<String, Vec<String>>,
    golden_pairs: &[(String, String)],
    depth: u8,
) -> ReachMetrics {
    // Group destinations by source, then one BFS per unique source.
    let mut by_src: HashMap<&str, Vec<&str>> = HashMap::new();
    for (s, d) in golden_pairs {
        by_src.entry(s.as_str()).or_default().push(d.as_str());
    }
    let mut reached = 0usize;
    for (src, dsts) in &by_src {
        let cone = reachable_within(adj, src, depth);
        for d in dsts {
            if cone.contains(*d) {
                reached += 1;
            }
        }
    }
    let pairs = golden_pairs.len();
    ReachMetrics {
        recall: if pairs == 0 {
            0.0
        } else {
            reached as f64 / pairs as f64
        },
        pairs,
        reached,
        depth,
    }
}

/// Descriptive structural-coverage statistics over the engine's own extracted
/// graph — how much structure the indexer recovered. Not a correctness claim; a
/// benchmark trend (see module docs).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageStats {
    /// Indexed symbols.
    pub n_symbols: usize,
    /// Total extracted edges (resolved + dangling).
    pub n_edges: usize,
    /// Edges linked to a real indexed callee (`!dst_missing`).
    pub n_edges_resolved: usize,
    /// Edges the resolver could not bind to a definition (`dst_missing`).
    pub n_edges_dangling: usize,
    /// `n_edges_resolved / n_edges` — the resolver's link rate (0 when no edges).
    pub resolution_rate: f64,
    /// Fraction of symbols that touch ≥1 resolved edge (as src or dst).
    pub connectivity: f64,
    /// `n_edges_resolved / n_symbols` — mean resolved out-edges per symbol.
    pub avg_out_degree: f64,
    /// Count of resolved edges per [`EdgeKind`] label (`calls`, `imports`, …).
    pub by_kind: BTreeMap<String, usize>,
}

/// Compute [`CoverageStats`] from the indexed symbols and edges.
pub fn coverage_stats(symbols: &[Symbol], edges: &[Edge]) -> CoverageStats {
    let n_symbols = symbols.len();
    let n_edges = edges.len();
    let n_edges_resolved = edges.iter().filter(|e| is_resolved(e)).count();
    let n_edges_dangling = n_edges - n_edges_resolved;

    let mut connected: BTreeSet<&str> = BTreeSet::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for e in edges.iter().filter(|e| is_resolved(e)) {
        connected.insert(e.src_id.as_str());
        connected.insert(e.dst_id.as_str());
        *by_kind
            .entry(edge_kind_label(e.kind).to_string())
            .or_insert(0) += 1;
    }

    CoverageStats {
        n_symbols,
        n_edges,
        n_edges_resolved,
        n_edges_dangling,
        resolution_rate: ratio(n_edges_resolved, n_edges),
        connectivity: ratio(connected.len(), n_symbols),
        avg_out_degree: if n_symbols == 0 {
            0.0
        } else {
            n_edges_resolved as f64 / n_symbols as f64
        },
        by_kind,
    }
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

// ===========================================================================
// Golden edge set — independent, source-verified true relationships.
// ===========================================================================

/// One golden dependency edge, labelled the same `(name, path_substring)` way as
/// the retrieval golden so it is human-authorable and index-build independent.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenEdge {
    /// Source symbol label `(name, path_substring)`.
    pub src: (String, String),
    /// Destination symbol label `(name, path_substring)`.
    pub dst: (String, String),
    /// Edge kind label (`"calls"`, `"imports"`, …); informational.
    pub kind: Option<String>,
}

/// An independent golden edge set for one repo.
///
/// Schema: `{ "repo"?: str, "complete"?: bool, "edges": [ { "src": [name, path],
/// "dst": [name, path], "kind"?: str }, ... ] }`. `complete = true` asserts the
/// `edges` enumerate **all** true out-edges of every listed source node, which
/// makes precision meaningful; otherwise the golden is partial and only recall
/// is reported (the honest default). Malformed entries are skipped, mirroring
/// the permissive retrieval-golden loader.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenEdges {
    /// Optional repo tag.
    pub repo: Option<String>,
    /// Whether the golden is complete for its source nodes (enables precision).
    pub complete: bool,
    /// The golden edges.
    pub edges: Vec<GoldenEdge>,
}

/// Parse a golden edge set from JSON.
///
/// # Errors
/// Returns [`CognisError::Eval`] when the JSON is invalid or lacks an `edges`
/// array.
pub fn parse_golden_edges(json: &str) -> Result<GoldenEdges> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CognisError::Eval(format!("golden edges JSON: {e}")))?;
    let repo = value
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let complete = value
        .get("complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let arr = value
        .get("edges")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CognisError::Eval("golden edge set missing `edges` array".into()))?;

    let label = |v: &serde_json::Value| -> Option<(String, String)> {
        let a = v.as_array()?;
        let name = a.first()?.as_str()?.to_string();
        let path = a.get(1)?.as_str()?.to_string();
        Some((name, path))
    };

    let mut edges = Vec::with_capacity(arr.len());
    for item in arr {
        let (Some(src), Some(dst)) = (
            item.get("src").and_then(&label),
            item.get("dst").and_then(&label),
        ) else {
            continue;
        };
        let kind = item
            .get("kind")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        edges.push(GoldenEdge { src, dst, kind });
    }
    Ok(GoldenEdges {
        repo,
        complete,
        edges,
    })
}

/// Load and parse a golden edge set from `path`.
///
/// # Errors
/// Returns [`CognisError::Eval`] when the file cannot be read or parsed.
pub fn load_golden_edges(path: &std::path::Path) -> Result<GoldenEdges> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CognisError::Eval(format!("read golden edges {}: {e}", path.display())))?;
    parse_golden_edges(&text)
}

/// The outcome of comparing the engine's edges to an independent golden.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeGoldenResult {
    /// Number of golden edges whose *both* endpoints resolved to indexed
    /// symbols (only these are evaluable — the rest are skipped, never faked).
    pub resolvable: usize,
    /// Number of golden edges skipped because an endpoint did not resolve
    /// (label predates the indexed checkout, or the symbol was not indexed).
    pub skipped: usize,
    /// Whether the golden was declared `complete` (precision reported iff true).
    pub complete: bool,
    /// Edge recall (and, when `complete`, precision/F1) over the resolvable
    /// golden edges, single-hop.
    pub edge: EdgeSetMetrics,
    /// Reachability recall at the configured depth (can the trace surface it).
    pub reach: ReachMetrics,
}

// ===========================================================================
// Harness — build once from a DB, evaluate coverage + optional golden.
// ===========================================================================

/// The structural-understanding harness over one indexed UCKG. Holds the
/// symbols, edges and the `(name, path)` → id resolver, all read once.
pub struct StructureHarness {
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
    table: crate::bench::SymbolTable,
}

impl StructureHarness {
    /// Build the harness over `db`.
    ///
    /// # Errors
    /// Propagates store read errors.
    pub fn new(db: &Database) -> Result<Self> {
        Ok(StructureHarness {
            symbols: db.list_symbols()?,
            edges: db.list_edges()?,
            table: crate::bench::SymbolTable::from_db(db)?,
        })
    }

    /// The indexed symbols.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// The extracted edges.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Descriptive structural coverage over the engine's own graph.
    pub fn coverage(&self) -> CoverageStats {
        coverage_stats(&self.symbols, &self.edges)
    }

    /// Evaluate the engine's edges against an independent `golden` at `depth`.
    ///
    /// Resolves each golden `(name, path)` endpoint to indexed symbol ids; an
    /// edge is *resolvable* only when both endpoints resolve (others are counted
    /// in `skipped`, never fabricated). Edge recall uses the full resolved edge
    /// set; precision is computed (and reported) only when `golden.complete`,
    /// restricted to the golden's source nodes.
    pub fn evaluate_golden(&self, golden: &GoldenEdges, depth: u8) -> EdgeGoldenResult {
        let predicted = resolved_edge_pairs(&self.edges, None);

        // Resolve golden edge labels to id-pair ground truth.
        let mut golden_pairs: BTreeSet<(String, String)> = BTreeSet::new();
        let mut golden_srcs: BTreeSet<String> = BTreeSet::new();
        let mut resolvable = 0usize;
        let mut skipped = 0usize;
        for g in &golden.edges {
            let src_ids = self.table.resolve(&g.src.0, &g.src.1);
            let dst_ids = self.table.resolve(&g.dst.0, &g.dst.1);
            if src_ids.is_empty() || dst_ids.is_empty() {
                skipped += 1;
                continue;
            }
            resolvable += 1;
            for s in &src_ids {
                golden_srcs.insert(s.clone());
                for d in &dst_ids {
                    golden_pairs.insert((s.clone(), d.clone()));
                }
            }
        }

        // Edge recall over the full predicted set.
        let recall_metrics = edge_set_metrics(&predicted, &golden_pairs);

        // Precision only when the golden is complete for its source nodes:
        // restrict the prediction to edges leaving a golden source node, so
        // precision measures over/under-prediction on annotated nodes.
        let edge = if golden.complete {
            let restricted: BTreeSet<(String, String)> = predicted
                .iter()
                .filter(|(s, _)| golden_srcs.contains(s))
                .cloned()
                .collect();
            let m = edge_set_metrics(&restricted, &golden_pairs);
            EdgeSetMetrics {
                // Keep recall from the unrestricted comparison (identical
                // matched/golden), take precision/f1 from the restricted one.
                recall: recall_metrics.recall,
                precision: m.precision,
                f1: m.f1,
                predicted: m.predicted,
                golden: golden_pairs.len(),
                matched: recall_metrics.matched,
            }
        } else {
            // Partial golden: recall is meaningful, precision is not — zero it
            // and let the report mark `complete = false` so it is not misread.
            EdgeSetMetrics {
                precision: 0.0,
                f1: 0.0,
                ..recall_metrics
            }
        };

        let adj = out_adjacency(&self.edges);
        let pairs: Vec<(String, String)> = golden_pairs.iter().cloned().collect();
        let reach = reachability_recall(&adj, &pairs, depth);

        EdgeGoldenResult {
            resolvable,
            skipped,
            complete: golden.complete,
            edge,
            reach,
        }
    }

    /// Build a full [`StructureReport`] for `repo`, at `depth`, with an optional
    /// independent golden and optional repo provenance.
    pub fn report(
        &self,
        repo: impl Into<String>,
        depth: u8,
        golden: Option<&GoldenEdges>,
        provenance: Option<Provenance>,
    ) -> StructureReport {
        let coverage = self.coverage();
        let edge_vs_golden = golden.map(|g| self.evaluate_golden(g, depth));

        let (mode, evidence_tier) = match &edge_vs_golden {
            Some(r) => (
                "golden".to_string(),
                format!(
                    "empirically supported (edge recall on n={} resolvable golden edges)",
                    r.resolvable
                ),
            ),
            None => (
                "coverage".to_string(),
                "empirically supported (descriptive structural coverage; not a correctness claim)"
                    .to_string(),
            ),
        };

        let mut notes = vec![
            "Structural coverage is descriptive (how much structure the indexer \
             recovered), not a retrieval-quality claim — Pillar-1 quality remains \
             the .benchmarks PR-derived harness."
                .to_string(),
        ];
        if edge_vs_golden.is_none() {
            notes.push(
                "No independent golden edge file present: edge recall/precision \
                 skipped (never fabricated). Add .benchmarks/public/golden/\
                 <repo>_edges.json to enable the comprehension score."
                    .to_string(),
            );
        } else if let Some(r) = &edge_vs_golden {
            if !r.complete {
                notes.push(
                    "Golden is partial (`complete=false`): edge recall is \
                     meaningful, precision is not reported."
                        .to_string(),
                );
            }
            if r.skipped > 0 {
                notes.push(format!(
                    "{} golden edge(s) skipped: an endpoint label did not resolve \
                     against the indexed DB (expected when the golden predates the \
                     indexed checkout).",
                    r.skipped
                ));
            }
        }

        StructureReport {
            schema_version: SCHEMA_VERSION,
            generated_at: now_iso8601(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            repo: repo.into(),
            repo_provenance: provenance,
            mode,
            evidence_tier,
            depth,
            coverage,
            edge_vs_golden,
            notes,
        }
    }
}

// ===========================================================================
// Report artifact (serializable, diffable benchmark baseline).
// ===========================================================================

/// Repo-under-test provenance, following the `repo_provenance` convention of the
/// committed baselines (`tests/e2e/baselines/*.json`,
/// `docs/development-criteria.md`): origin URL, exact HEAD commit, `git
/// describe` version, and whether the tree was dirty.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    /// Origin remote URL (`git remote get-url origin`), if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Full HEAD commit sha.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// `git describe --tags --always` output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether the working tree had uncommitted changes.
    pub dirty: bool,
}

impl Provenance {
    /// Best-effort provenance by shelling `git` in `repo_dir`. Returns `None`
    /// when the directory is not a git repo or `git` is unavailable — provenance
    /// is recorded when it can be, never fabricated.
    pub fn from_git(repo_dir: &std::path::Path) -> Option<Provenance> {
        if !repo_dir.is_dir() {
            return None;
        }
        let git = |args: &[&str]| -> Option<String> {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo_dir)
                .args(args)
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        };
        let commit = git(&["rev-parse", "HEAD"]);
        // Must at least be inside a git work tree to claim provenance.
        commit.as_ref()?;
        let url = git(&["remote", "get-url", "origin"]);
        let version = git(&["describe", "--tags", "--always"]);
        let dirty = git(&["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        Some(Provenance {
            url,
            commit,
            version,
            dirty,
        })
    }
}

/// The serializable structural-understanding report — persisted under
/// `.benchmarks/` as the diffable benchmark baseline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StructureReport {
    /// [`SCHEMA_VERSION`] the report was written with.
    pub schema_version: u32,
    /// ISO-8601 UTC generation timestamp.
    pub generated_at: String,
    /// `cognis-eval` package version that produced the report.
    pub tool_version: String,
    /// Repo tag under test.
    pub repo: String,
    /// Repo-under-test provenance (origin/commit/version/dirty), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_provenance: Option<Provenance>,
    /// `"coverage"` (no golden) or `"golden"` (independent golden evaluated).
    pub mode: String,
    /// Evidence-tier label (per `docs/development-criteria.md`).
    pub evidence_tier: String,
    /// Trace depth used for reachability.
    pub depth: u8,
    /// Descriptive structural coverage.
    pub coverage: CoverageStats,
    /// Edge recall/precision + reachability vs an independent golden, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_vs_golden: Option<EdgeGoldenResult>,
    /// Honest caveats / skip notes.
    pub notes: Vec<String>,
}

impl StructureReport {
    /// Serialize to pretty JSON (the committed artifact form).
    ///
    /// # Errors
    /// Propagates a serialization error (should not happen for this shape).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| CognisError::Eval(format!("serialize structure report: {e}")))
    }

    /// Render a human-readable Markdown summary (RESULTS.md style).
    pub fn to_markdown(&self) -> String {
        let c = &self.coverage;
        let mut s = String::new();
        s.push_str(&format!("### {} — structural understanding\n\n", self.repo));
        s.push_str(&format!(
            "- mode: `{}` · tier: {} · depth: {}\n",
            self.mode, self.evidence_tier, self.depth
        ));
        if let Some(p) = &self.repo_provenance {
            s.push_str(&format!(
                "- provenance: commit `{}`{}{}\n",
                p.commit.as_deref().unwrap_or("?"),
                p.version
                    .as_deref()
                    .map(|v| format!(" ({v})"))
                    .unwrap_or_default(),
                if p.dirty { " [dirty]" } else { "" }
            ));
        }
        s.push_str("\n**Structural coverage**\n\n");
        s.push_str(
            "| symbols | edges | resolved | dangling | resolution | connectivity | avg out-deg |\n",
        );
        s.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        s.push_str(&format!(
            "| {} | {} | {} | {} | {:.1}% | {:.1}% | {:.2} |\n",
            c.n_symbols,
            c.n_edges,
            c.n_edges_resolved,
            c.n_edges_dangling,
            c.resolution_rate * 100.0,
            c.connectivity * 100.0,
            c.avg_out_degree
        ));
        if !c.by_kind.is_empty() {
            let kinds: Vec<String> = c.by_kind.iter().map(|(k, v)| format!("{k}={v}")).collect();
            s.push_str(&format!(
                "\n- resolved edges by kind: {}\n",
                kinds.join(", ")
            ));
        }
        if let Some(r) = &self.edge_vs_golden {
            s.push_str("\n**Edge comprehension vs independent golden**\n\n");
            s.push_str(&format!(
                "- resolvable golden edges: {} (skipped {})\n",
                r.resolvable, r.skipped
            ));
            s.push_str(&format!("- edge recall: {:.1}%\n", r.edge.recall * 100.0));
            if r.complete {
                s.push_str(&format!(
                    "- edge precision: {:.1}% · F1: {:.1}%\n",
                    r.edge.precision * 100.0,
                    r.edge.f1 * 100.0
                ));
            }
            s.push_str(&format!(
                "- reachability recall@{}: {:.1}% ({} / {})\n",
                r.reach.depth,
                r.reach.recall * 100.0,
                r.reach.reached,
                r.reach.pairs
            ));
        }
        if !self.notes.is_empty() {
            s.push_str("\n_Notes_\n");
            for n in &self.notes {
                s.push_str(&format!("- {n}\n"));
            }
        }
        s.push('\n');
        s
    }
}

// ===========================================================================
// Dependency-free ISO-8601 UTC timestamp (avoids a chrono/time dependency).
// ===========================================================================

/// Current time as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`). On the
/// (impossible) pre-epoch clock error, returns the epoch.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_to_iso8601(secs)
}

/// Format Unix `secs` (UTC) as `YYYY-MM-DDTHH:MM:SSZ` using Howard Hinnant's
/// days-from-civil algorithm. Pure and unit-tested.
pub fn unix_to_iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);

    // days since 1970-01-01 -> civil (y, m, d). Hinnant, "chrono-Compatible
    // Low-Level Date Algorithms".
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

// ===========================================================================
// Unit tests — the metrics are pure, so they are exercised by construction.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn e(src: &str, dst: &str, kind: EdgeKind, missing: bool) -> Edge {
        Edge {
            src_id: src.to_string(),
            dst_id: dst.to_string(),
            kind,
            confidence: 1.0,
            meta: if missing {
                serde_json::json!({ "dst_missing": true })
            } else {
                serde_json::json!({})
            },
        }
    }

    fn set(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn edge_set_metrics_exact() {
        let predicted = set(&[("a", "b"), ("b", "c"), ("a", "c")]);
        let golden = set(&[("a", "b"), ("b", "c"), ("c", "d")]);
        let m = edge_set_metrics(&predicted, &golden);
        // matched = {(a,b),(b,c)} = 2. precision 2/3, recall 2/3.
        assert_eq!(m.matched, 2);
        assert_eq!(m.predicted, 3);
        assert_eq!(m.golden, 3);
        assert!((m.precision - 2.0 / 3.0).abs() < 1e-12);
        assert!((m.recall - 2.0 / 3.0).abs() < 1e-12);
        assert!((m.f1 - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn edge_set_metrics_empty_edges() {
        let empty = BTreeSet::new();
        let golden = set(&[("a", "b")]);
        let m = edge_set_metrics(&empty, &golden);
        assert_eq!(m.precision, 0.0);
        assert_eq!(m.recall, 0.0);
        assert_eq!(m.f1, 0.0);
        // Empty golden → recall defined as 0.
        assert_eq!(edge_set_metrics(&golden, &empty).recall, 0.0);
    }

    #[test]
    fn reachable_within_respects_depth_and_excludes_start() {
        // a -> b -> c -> d, plus a -> e.
        let edges = vec![
            e("a", "b", EdgeKind::Calls, false),
            e("b", "c", EdgeKind::Calls, false),
            e("c", "d", EdgeKind::Calls, false),
            e("a", "e", EdgeKind::Calls, false),
        ];
        let adj = out_adjacency(&edges);
        let d1 = reachable_within(&adj, "a", 1);
        assert_eq!(d1, set_ids(&["b", "e"]));
        let d2 = reachable_within(&adj, "a", 2);
        assert_eq!(d2, set_ids(&["b", "e", "c"]));
        let d3 = reachable_within(&adj, "a", 3);
        assert_eq!(d3, set_ids(&["b", "e", "c", "d"]));
        // start never appears in its own reachable set.
        assert!(!d3.contains("a"));
    }

    #[test]
    fn dangling_edges_are_excluded_everywhere() {
        let edges = vec![
            e("a", "b", EdgeKind::Calls, false),
            e("a", "ghost", EdgeKind::Calls, true), // dst_missing
        ];
        let adj = out_adjacency(&edges);
        assert_eq!(reachable_within(&adj, "a", 3), set_ids(&["b"]));
        let pairs = resolved_edge_pairs(&edges, None);
        assert_eq!(pairs, set(&[("a", "b")]));
    }

    #[test]
    fn reachability_recall_groups_by_source() {
        let edges = vec![
            e("a", "b", EdgeKind::Calls, false),
            e("b", "c", EdgeKind::Calls, false),
        ];
        let adj = out_adjacency(&edges);
        // (a,c) reachable at depth 2 but not depth 1; (a,b) always.
        let golden = vec![("a".into(), "b".into()), ("a".into(), "c".into())];
        assert_eq!(reachability_recall(&adj, &golden, 1).reached, 1);
        assert_eq!(reachability_recall(&adj, &golden, 2).reached, 2);
        assert!((reachability_recall(&adj, &golden, 2).recall - 1.0).abs() < 1e-12);
    }

    #[test]
    fn coverage_stats_counts_resolution_and_kinds() {
        let symbols = vec![
            symbol("a"),
            symbol("b"),
            symbol("c"),
            symbol("d"), // isolated
        ];
        let edges = vec![
            e("a", "b", EdgeKind::Calls, false),
            e("b", "c", EdgeKind::Imports, false),
            e("a", "ghost", EdgeKind::Calls, true), // dangling
        ];
        let cov = coverage_stats(&symbols, &edges);
        assert_eq!(cov.n_symbols, 4);
        assert_eq!(cov.n_edges, 3);
        assert_eq!(cov.n_edges_resolved, 2);
        assert_eq!(cov.n_edges_dangling, 1);
        assert!((cov.resolution_rate - 2.0 / 3.0).abs() < 1e-12);
        // connected = {a,b,c} of 4 → 0.75.
        assert!((cov.connectivity - 0.75).abs() < 1e-12);
        assert!((cov.avg_out_degree - 0.5).abs() < 1e-12);
        assert_eq!(cov.by_kind.get("calls"), Some(&1));
        assert_eq!(cov.by_kind.get("imports"), Some(&1));
    }

    #[test]
    fn parse_golden_edges_is_permissive() {
        let json = r#"{
            "repo": "x", "complete": true,
            "edges": [
                {"src": ["f", "a.py"], "dst": ["g", "b.py"], "kind": "calls"},
                {"src": ["bad"], "dst": ["g", "b.py"]},
                {"dst": ["g", "b.py"]}
            ]
        }"#;
        let g = parse_golden_edges(json).unwrap();
        assert!(g.complete);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].kind.as_deref(), Some("calls"));

        assert!(parse_golden_edges(r#"{"repo":"x"}"#).is_err());
    }

    #[test]
    fn iso8601_known_epochs() {
        assert_eq!(unix_to_iso8601(0), "1970-01-01T00:00:00Z");
        // 2001-09-09T01:46:40Z, the classic 1e9 epoch.
        assert_eq!(unix_to_iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2021-01-01T00:00:00Z.
        assert_eq!(unix_to_iso8601(1_609_459_200), "2021-01-01T00:00:00Z");
    }

    #[test]
    fn report_serializes_and_renders() {
        let symbols = vec![symbol("a"), symbol("b")];
        let edges = vec![e("a", "b", EdgeKind::Calls, false)];
        let cov = coverage_stats(&symbols, &edges);
        let report = StructureReport {
            schema_version: SCHEMA_VERSION,
            generated_at: unix_to_iso8601(0),
            tool_version: "test".into(),
            repo: "synthetic".into(),
            repo_provenance: None,
            mode: "coverage".into(),
            evidence_tier: "empirically supported (descriptive)".into(),
            depth: DEFAULT_DEPTH,
            coverage: cov,
            edge_vs_golden: None,
            notes: vec![],
        };
        let json = report.to_json().unwrap();
        assert!(json.contains("\"resolution_rate\""));
        assert!(json.contains("\"schema_version\": 1"));
        // Round-trips as valid JSON.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["repo"], "synthetic");
        let md = report.to_markdown();
        assert!(md.contains("structural understanding"));
    }

    // --- helpers ---

    fn set_ids(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn symbol(id: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            kind: cognis_core::SymbolKind::Function,
            name: id.to_string(),
            qualified_name: id.to_string(),
            language: "python".to_string(),
            module: "m".to_string(),
            file_path: format!("src/{id}.py"),
            line_start: 1,
            line_end: 2,
            signature: None,
            docstring: None,
            content_hash: "h".to_string(),
            body_excerpt: None,
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: vec![],
            updated_at: 0,
        }
    }
}
