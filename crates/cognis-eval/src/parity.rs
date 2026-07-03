//! Differential parity harness — Property 2 (kernel parity vs the Python oracle).
//!
//! This is the **safety net** of the strangler-fig migration (Requirement 10.3):
//! a harness that runs the *same* query against two UCKG databases — one built
//! by the Python engine (the parity oracle) and one built by the Rust engine —
//! and asserts the three sub-claims of design **Property 2** hold:
//!
//! * **CSAR estimate** L1 distance `< 1e-9` ([`CSAR_L1_TOL`]) — Requirement 4.3.
//!   (`cognis-csar` already proved L1 = 0 bit-exact on the parity graphs.)
//! * **RRF / top-k ordering** byte-identical — Requirement 4.1 (P-PAR-FUSE).
//! * **lexical (FTS5)** and **semantic (vec KNN)** hit sets identical on the
//!   same DB — Requirement 4.2 (P-PAR-FTS / P-PAR-VEC).
//!
//! ## Honest comparison — never fabricate
//!
//! The comparison *primitives* ([`lexical_hit_sets`], [`semantic_topk`],
//! [`rrf_topk_byte_identical`], [`csar_estimate_l1`]) are pure functions over
//! already-computed results, so they carry no I/O and no environment
//! assumptions. The [`DifferentialHarness`] wires them to two live
//! [`Database`]s. It supports three modes the test harness uses, in increasing
//! strength:
//!
//! 1. **Rust-vs-Rust determinism** — point both sides at the *same* DB (or two
//!    copies). Every surface must be byte-identical and the CSAR L1 must be
//!    exactly 0; this proves the engine is deterministic and the harness
//!    mechanics are sound, and it runs fully offline with no Python.
//! 2. **Rust-vs-Python-oracle** — run the Rust engine on a Python-built DB and
//!    compare against the Python engine's *captured* outputs (golden JSON). This
//!    is the real Property-2 gate that needs no Python runtime at test time.
//! 3. **Python-build vs Rust-build** — two real DBs of the same repo, one from
//!    each indexer. The strongest differential; enabled when both DBs are
//!    present, skipped (never faked) otherwise.
//!
//! All comparisons are expressed in **symbol-id space**, so they are robust to
//! the two builds assigning different internal node indices to the same symbol.

use std::collections::{BTreeSet, HashMap};

use cognis_core::Hit;
use cognis_csar::{approximate_ppr_push, CodeGraph};
use cognis_retrieval::rrf_fuse;
use cognis_store::{Database, SymbolStore};

pub use cognis_core::Result;

/// Property-2 tolerance on the CSAR estimate L1 distance (Requirement 4.3).
pub const CSAR_L1_TOL: f64 = 1e-9;

/// Default forward-push restart probability (mirrors the CSAR layer default).
pub const DEFAULT_ALPHA: f64 = cognis_csar::DEFAULT_ALPHA;
/// Default forward-push residual threshold.
pub const DEFAULT_EPS: f64 = cognis_csar::DEFAULT_EPS;
/// Default RRF rank constant (mirrors `fusion.py`).
pub const DEFAULT_RRF_K: f64 = cognis_retrieval::DEFAULT_RRF_K;

/// Verdict for one retrieval surface compared across two engine builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceParity {
    /// The two sides produced identical results.
    Match,
    /// They diverged; carries a human-readable description of the divergence.
    Mismatch(String),
}

impl SurfaceParity {
    /// `true` when the two sides matched.
    pub fn is_match(&self) -> bool {
        matches!(self, SurfaceParity::Match)
    }

    /// The divergence description, when this is a mismatch.
    pub fn reason(&self) -> Option<&str> {
        match self {
            SurfaceParity::Mismatch(m) => Some(m.as_str()),
            SurfaceParity::Match => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Comparison primitives (pure, I/O-free).
// ---------------------------------------------------------------------------

/// Lexical (FTS5) parity: the two **hit sets** of symbol ids must be identical,
/// order-independent — P-PAR-FTS (Requirement 4.2). FTS5 BM25 ties between two
/// rows are not ordering-stable, so the gate is set equality, mirroring the
/// `cognis-store` `fts_parity` test.
pub fn lexical_hit_sets(a: &[Hit], b: &[Hit]) -> SurfaceParity {
    let sa: BTreeSet<&str> = a.iter().map(|h| h.symbol_id.as_str()).collect();
    let sb: BTreeSet<&str> = b.iter().map(|h| h.symbol_id.as_str()).collect();
    if sa == sb {
        return SurfaceParity::Match;
    }
    let only_a: Vec<&str> = sa.difference(&sb).copied().collect();
    let only_b: Vec<&str> = sb.difference(&sa).copied().collect();
    SurfaceParity::Mismatch(format!(
        "lexical hit sets differ: only_in_a={only_a:?}, only_in_b={only_b:?}"
    ))
}

/// Semantic (vec KNN) parity: the top-k **ordering** of symbol ids must match —
/// P-PAR-VEC (Requirement 4.2). KNN is ordered nearest-first, so unlike the
/// lexical set comparison this asserts the ranked sequence is identical.
pub fn semantic_topk(a: &[Hit], b: &[Hit]) -> SurfaceParity {
    let ia: Vec<&str> = a.iter().map(|h| h.symbol_id.as_str()).collect();
    let ib: Vec<&str> = b.iter().map(|h| h.symbol_id.as_str()).collect();
    if ia == ib {
        SurfaceParity::Match
    } else {
        SurfaceParity::Mismatch(format!(
            "semantic top-k ordering differs: a={ia:?}, b={ib:?}"
        ))
    }
}

/// RRF parity: the fused top-k must be **byte-identical** — same id ordering and
/// the same raw IEEE-754 f64 score bits — P-PAR-FUSE (Requirement 4.1). Scores
/// are compared on `to_bits()` so this is true byte-identity, not an epsilon
/// compare (the same discipline as the `cognis-retrieval` `fusion_parity` test).
pub fn rrf_topk_byte_identical(a: &[Hit], b: &[Hit]) -> SurfaceParity {
    if a.len() != b.len() {
        return SurfaceParity::Mismatch(format!(
            "fused length differs: a={}, b={}",
            a.len(),
            b.len()
        ));
    }
    for (i, (ha, hb)) in a.iter().zip(b.iter()).enumerate() {
        if ha.symbol_id != hb.symbol_id {
            return SurfaceParity::Mismatch(format!(
                "rank {i} id differs: a={:?}, b={:?}",
                ha.symbol_id, hb.symbol_id
            ));
        }
        if ha.score.to_bits() != hb.score.to_bits() {
            return SurfaceParity::Mismatch(format!(
                "rank {i} fused score not byte-identical: a={} ({:#018x}) vs b={} ({:#018x})",
                ha.score,
                ha.score.to_bits(),
                hb.score,
                hb.score.to_bits()
            ));
        }
    }
    SurfaceParity::Match
}

/// L1 distance between two CSAR estimate maps keyed by **symbol id**.
///
/// `Σ |a[id] − b[id]|` over the union of keys (a missing key counts as 0). The
/// symbol-id keying makes this invariant to the two builds numbering nodes
/// differently, which is the property the differential gate needs (Requirement
/// 4.3: `L1 < 1e-9`).
pub fn csar_estimate_l1(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let mut keys: BTreeSet<&String> = a.keys().collect();
    keys.extend(b.keys());
    keys.into_iter()
        .map(|k| {
            let va = a.get(k).copied().unwrap_or(0.0);
            let vb = b.get(k).copied().unwrap_or(0.0);
            (va - vb).abs()
        })
        .sum()
}

/// Run forward-push PPR over `g` from a seed expressed in **symbol-id** space
/// and return the estimate keyed by symbol id.
///
/// Seeds are given as `(symbol_id, mass)`; ids absent from `g` are ignored
/// (mirroring the kernel's seed filter). Resolving the seed per-graph and
/// re-keying the estimate by symbol id is what lets [`csar_estimate_l1`] compare
/// two builds whose internal node ordering may differ.
///
/// # Errors
/// Propagates [`approximate_ppr_push`]'s error when `alpha ∉ (0, 1]` or
/// `eps <= 0`.
pub fn csar_estimate_by_symbol(
    g: &CodeGraph,
    seed_ids: &[(String, f64)],
    alpha: f64,
    eps: f64,
) -> Result<HashMap<String, f64>> {
    let seed: Vec<(i32, f64)> = seed_ids
        .iter()
        .filter_map(|(id, m)| g.index.get(id).map(|&i| (i as i32, *m)))
        .collect();
    let push = approximate_ppr_push(g, &seed, alpha, eps)?;
    Ok(push
        .estimate
        .into_iter()
        .map(|(node, score)| (g.node_ids[node as usize].clone(), score))
        .collect())
}

// ---------------------------------------------------------------------------
// Query cases + per-case report.
// ---------------------------------------------------------------------------

/// One differential query case: which surfaces to exercise and with what input.
///
/// A surface is compared only when its input is present, so a case can target a
/// single surface (e.g. lexical-only) or all of them. RRF fusion is compared
/// when **both** a lexical query and a semantic query vector are supplied (it
/// fuses the two layers); CSAR is compared when `seeds` is non-empty.
#[derive(Debug, Clone, Default)]
pub struct QueryCase {
    /// Human-readable case name (for diagnostics).
    pub name: String,
    /// FTS5 query for the lexical surface, if exercised.
    pub lexical: Option<String>,
    /// Pre-computed query embedding for the semantic surface, if exercised.
    pub semantic: Option<Vec<f32>>,
    /// CSAR seed symbols `(symbol_id, mass)` for the structural surface.
    pub seeds: Vec<(String, f64)>,
    /// Top-k cap applied to every exercised surface.
    pub k: usize,
}

impl QueryCase {
    /// A lexical-only case.
    pub fn lexical(name: impl Into<String>, query: impl Into<String>, k: usize) -> Self {
        QueryCase {
            name: name.into(),
            lexical: Some(query.into()),
            k,
            ..Default::default()
        }
    }
}

/// Per-surface verdicts for one [`QueryCase`]. A surface absent from the case is
/// `None`; the CSAR entry carries the measured L1 alongside its verdict.
#[derive(Debug, Clone)]
pub struct CaseReport {
    /// The originating case name.
    pub name: String,
    /// Lexical (FTS5) hit-set verdict, when exercised.
    pub lexical: Option<SurfaceParity>,
    /// Semantic (vec KNN) top-k verdict, when exercised.
    pub semantic: Option<SurfaceParity>,
    /// RRF fused top-k byte-identity verdict, when exercised.
    pub rrf: Option<SurfaceParity>,
    /// CSAR `(L1 distance, verdict)`, when exercised.
    pub csar: Option<(f64, SurfaceParity)>,
}

impl CaseReport {
    /// `true` when every exercised surface matched (Property 2 holds for this
    /// case).
    pub fn all_match(&self) -> bool {
        self.mismatches().is_empty()
    }

    /// Human-readable descriptions of every surface that diverged.
    pub fn mismatches(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |surface: &str, p: &SurfaceParity| {
            if let Some(reason) = p.reason() {
                out.push(format!("[{}] {surface}: {reason}", self.name));
            }
        };
        if let Some(p) = &self.lexical {
            push("lexical", p);
        }
        if let Some(p) = &self.semantic {
            push("semantic", p);
        }
        if let Some(p) = &self.rrf {
            push("rrf", p);
        }
        if let Some((_, p)) = &self.csar {
            push("csar", p);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The harness.
// ---------------------------------------------------------------------------

/// Differential parity harness over two UCKG databases.
///
/// `a` is the reference build (conventionally the Python oracle DB) and `b` the
/// candidate build (the Rust DB); the comparison is symmetric, so a Rust-vs-Rust
/// determinism run just passes the same DB (or two copies) for both.
pub struct DifferentialHarness<'a> {
    a: &'a Database,
    b: &'a Database,
    alpha: f64,
    eps: f64,
    rrf_k: f64,
}

impl<'a> DifferentialHarness<'a> {
    /// Build a harness with the default forward-push / RRF parameters.
    pub fn new(a: &'a Database, b: &'a Database) -> Self {
        DifferentialHarness {
            a,
            b,
            alpha: DEFAULT_ALPHA,
            eps: DEFAULT_EPS,
            rrf_k: DEFAULT_RRF_K,
        }
    }

    /// Override the forward-push parameters used for the CSAR comparison.
    pub fn with_csar_params(mut self, alpha: f64, eps: f64) -> Self {
        self.alpha = alpha;
        self.eps = eps;
        self
    }

    /// Compare the lexical (FTS5) hit sets for `query` (Requirement 4.2).
    pub fn compare_lexical(&self, query: &str, k: usize) -> Result<SurfaceParity> {
        let ha = self.a.fts_search(query, k)?;
        let hb = self.b.fts_search(query, k)?;
        Ok(lexical_hit_sets(&ha, &hb))
    }

    /// Compare the semantic (vec KNN) top-k for `q` (Requirement 4.2).
    pub fn compare_semantic(&self, q: &[f32], k: usize) -> Result<SurfaceParity> {
        let ha = self.a.vec_search(q, k)?;
        let hb = self.b.vec_search(q, k)?;
        Ok(semantic_topk(&ha, &hb))
    }

    /// Fuse the lexical + semantic layers and compare the fused top-k
    /// byte-for-byte (Requirement 4.1).
    pub fn compare_rrf(&self, query: &str, q: &[f32], k: usize) -> Result<SurfaceParity> {
        let fa = self.fuse(self.a, query, q, k)?;
        let fb = self.fuse(self.b, query, q, k)?;
        Ok(rrf_topk_byte_identical(&fa, &fb))
    }

    /// Build each DB's resident CSR graph, diffuse from `seed_ids`, and compare
    /// the estimates by L1 (Requirement 4.3). Returns `(L1, verdict)`.
    pub fn compare_csar(&self, seed_ids: &[(String, f64)]) -> Result<(f64, SurfaceParity)> {
        let ga = self.a.build_code_graph(None)?;
        let gb = self.b.build_code_graph(None)?;
        let ea = csar_estimate_by_symbol(&ga, seed_ids, self.alpha, self.eps)?;
        let eb = csar_estimate_by_symbol(&gb, seed_ids, self.alpha, self.eps)?;
        let l1 = csar_estimate_l1(&ea, &eb);
        let verdict = if l1 < CSAR_L1_TOL {
            SurfaceParity::Match
        } else {
            SurfaceParity::Mismatch(format!(
                "CSAR estimate L1 {l1:.3e} ≥ tolerance {CSAR_L1_TOL:.0e}"
            ))
        };
        Ok((l1, verdict))
    }

    /// Run every surface a [`QueryCase`] targets and collect the verdicts.
    pub fn run_case(&self, case: &QueryCase) -> Result<CaseReport> {
        let lexical = match &case.lexical {
            Some(q) => Some(self.compare_lexical(q, case.k)?),
            None => None,
        };
        let semantic = match &case.semantic {
            Some(v) => Some(self.compare_semantic(v, case.k)?),
            None => None,
        };
        // RRF needs both layers to fuse.
        let rrf = match (&case.lexical, &case.semantic) {
            (Some(q), Some(v)) => Some(self.compare_rrf(q, v, case.k)?),
            _ => None,
        };
        let csar = if case.seeds.is_empty() {
            None
        } else {
            Some(self.compare_csar(&case.seeds)?)
        };
        Ok(CaseReport {
            name: case.name.clone(),
            lexical,
            semantic,
            rrf,
            csar,
        })
    }

    /// Run a batch of cases and collect every divergence (empty ⇒ Property 2
    /// holds across the whole batch).
    pub fn run_cases(&self, cases: &[QueryCase]) -> Result<Vec<CaseReport>> {
        cases.iter().map(|c| self.run_case(c)).collect()
    }

    /// Build the per-layer hit lists for one DB and fuse them with RRF — the
    /// exact `[lexical, semantic]` partition the capsule composer would hand
    /// `rrf_fuse`.
    fn fuse(&self, db: &Database, query: &str, q: &[f32], k: usize) -> Result<Vec<Hit>> {
        let lexical = db.fts_search(query, k)?;
        let semantic = db.vec_search(q, k)?;
        Ok(rrf_fuse(&[lexical, semantic], k, self.rrf_k))
    }
}

/// Collapse a batch of [`CaseReport`]s into the flat list of every divergence.
/// An empty result means Property 2 held across all cases.
pub fn collect_mismatches(reports: &[CaseReport]) -> Vec<String> {
    reports.iter().flat_map(CaseReport::mismatches).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, score: f64, layer: &str) -> Hit {
        Hit::new(id, score, layer, "t")
    }

    #[test]
    fn lexical_hit_sets_order_independent() {
        let a = vec![hit("x", 1.0, "lexical"), hit("y", 0.5, "lexical")];
        let b = vec![hit("y", 0.9, "lexical"), hit("x", 0.1, "lexical")];
        // Same id set, different order/scores → lexical parity is set-based.
        assert_eq!(lexical_hit_sets(&a, &b), SurfaceParity::Match);

        let c = vec![hit("x", 1.0, "lexical"), hit("z", 0.5, "lexical")];
        assert!(!lexical_hit_sets(&a, &c).is_match());
    }

    #[test]
    fn semantic_topk_is_order_sensitive() {
        let a = vec![hit("x", 1.0, "semantic"), hit("y", 0.5, "semantic")];
        let same = vec![hit("x", 0.0, "semantic"), hit("y", 0.0, "semantic")];
        let swapped = vec![hit("y", 0.5, "semantic"), hit("x", 1.0, "semantic")];
        assert_eq!(semantic_topk(&a, &same), SurfaceParity::Match);
        assert!(!semantic_topk(&a, &swapped).is_match());
    }

    #[test]
    fn rrf_byte_identity_distinguishes_score_bits() {
        let a = vec![hit("x", 0.123_456_789, "fused")];
        let same = vec![hit("x", 0.123_456_789, "fused")];
        assert_eq!(rrf_topk_byte_identical(&a, &same), SurfaceParity::Match);

        // A one-ULP perturbation must be caught (byte-identity, not epsilon).
        let nudged_bits = 0.123_456_789_f64.to_bits() + 1;
        let nudged = vec![hit("x", f64::from_bits(nudged_bits), "fused")];
        assert!(!rrf_topk_byte_identical(&a, &nudged).is_match());

        // Differing length is a mismatch.
        assert!(!rrf_topk_byte_identical(&a, &[]).is_match());
    }

    #[test]
    fn csar_l1_identity_and_union_of_keys() {
        let mut m = HashMap::new();
        m.insert("a".to_string(), 0.6);
        m.insert("b".to_string(), 0.4);
        // Identity: distance to itself is exactly 0.
        assert_eq!(csar_estimate_l1(&m, &m), 0.0);

        // Missing keys count as 0 on the other side.
        let mut n = HashMap::new();
        n.insert("a".to_string(), 0.6);
        // Only "b" differs, by 0.4.
        assert!((csar_estimate_l1(&m, &n) - 0.4).abs() < 1e-15);
    }

    #[test]
    fn case_report_aggregates_mismatches() {
        let report = CaseReport {
            name: "case1".to_string(),
            lexical: Some(SurfaceParity::Match),
            semantic: Some(SurfaceParity::Mismatch("ordering differs".to_string())),
            rrf: None,
            csar: Some((5e-3, SurfaceParity::Mismatch("L1 too big".to_string()))),
        };
        assert!(!report.all_match());
        let m = report.mismatches();
        assert_eq!(m.len(), 2);
        assert!(m.iter().any(|s| s.contains("semantic")));
        assert!(m.iter().any(|s| s.contains("csar")));
    }
}
