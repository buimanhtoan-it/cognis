//! `diffuse_context` / CSAR kernel latency bench (Task 9.3 — Requirement 11.2).
//!
//! This is the runnable methodology behind Requirement 11.2: the
//! `diffuse_context` p50 latency *SHALL not be worse than the Python version at
//! the Pillar-2 reference, and SHALL improve once the graph build is native
//! (K2)*. It measures the real Rust code paths with Criterion and prints the
//! recorded Python/Pillar-2 baseline alongside, so the comparison is explicit —
//! it never fabricates a timing.
//!
//! ## What it measures (three groups, all real code paths)
//!
//! 1. **`csar_kernel/forward_push`** — the proven Andersen-Chung-Lang
//!    forward-push kernel ([`cognis_csar::approximate_ppr_push`]) over a
//!    **resident** CSR graph (built once, outside the timed loop). This is the
//!    exact quantity the Pillar-2 reference calls `rust_solver_ms` (§12 of
//!    `docs/native-core-rust.md`), which showed **15–123×** over the Python push
//!    at **L1 = 0** (empirically-supported, n=4). Requirement 11.1.
//!
//! 2. **`diffuse_context/resident`** — [`cognis_csar::diffuse_seed_hits`] over a
//!    resident graph: seed-distribution build + forward push + ranking, i.e. the
//!    `diffuse_context` compute once the CSR graph is already native and held
//!    between queries (the K2 end state). This is the interactive-regime p50.
//!
//! 3. **`diffuse_context/end_to_end`** — [`SymbolStore::build_code_graph`]
//!    (native CSR build, K2) **plus** [`cognis_csar::diffuse_seed_hits`], the
//!    full read path from an indexed UCKG. The Pillar-2 reference's Python
//!    `diffuse_context` p50 pays the per-query CSR marshalling (`csr_ms`, which
//!    grew to 379 ms at n=100k); the native build folds that cost in, which is
//!    what Requirement 11.2's "improve once the graph build is native" is about.
//!
//! ## Graphs — real where possible, deterministic-synthetic otherwise
//!
//! Per the discipline in the task and `docs/development-criteria.md`: benchmarks
//! must measure real code paths, and where a real indexed DB is needed but
//! unavailable we generate a **deterministic synthetic graph** rather than
//! fabricate numbers.
//!
//! * **Synthetic** (always available, offline): a Barabási–Albert-style
//!   preferential-attachment graph (`m = 4`), the same hub-heavy size/density
//!   class as the §12 reference rows (`n ∈ {320, 1000, 10000}`; n=320 mirrors
//!   the real `requests.db`'s 320 nodes / 2,678 edges). It is deterministic
//!   (fixed LCG seed) so runs are reproducible, but its topology is **not
//!   bit-identical** to the numpy-generated reference graph — so the
//!   cross-language comparison is **by size class**, labelled
//!   *empirically-supported (n=…)* / *conjectured*, never *proven*.
//! * **Real** (opt-in): set `COGNIS_DIFFUSE_DB` to a real `.cognis/uckg.db` and
//!   the end-to-end / resident groups additionally run on it — the strongest,
//!   genuinely apples-to-apples measurement against the Pillar-2 Python p50 on
//!   the same DB.
//!
//! Run:
//!   cargo bench -p cognis-eval --bench diffuse_latency
//!   COGNIS_DIFFUSE_DB=.cognis/uckg.db cargo bench -p cognis-eval --bench diffuse_latency

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;

use cognis_core::{CodeGraph, Edge, EdgeKind, Hit, Symbol, SymbolKind};
use cognis_csar::{approximate_ppr_push, diffuse_seed_hits, DEFAULT_ALPHA, DEFAULT_EPS};
use cognis_store::{Database, SymbolStore, SymbolWriter};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Kernel parameters — identical to the Pillar-2 reference (§12): `alpha=0.15`,
/// `eps=1e-5`, seed = 5 nodes of equal mass.
const ALPHA: f64 = DEFAULT_ALPHA; // 0.15
const EPS: f64 = DEFAULT_EPS; // 1e-5
/// Number of seed nodes (matches the reference `_make_seed(k=5)`).
const SEED_K: usize = 5;
/// Preferential-attachment edges per new node (matches the reference `m = 4`).
const BA_M: usize = 4;
/// Top-k returned by `diffuse_seed_hits` (the `diffuse_context` contract default).
const TOPK: usize = 20;

/// Synthetic graph sizes, chosen to line up with the §12 reference rows
/// (`requests.db` ≈ 320 nodes; scale-free 1k/10k). 100k is left to the kernel
/// reference table — building a 100k-node SQLite DB per bench run is needless.
const SIZES: &[usize] = &[320, 1_000, 10_000];
/// Sizes for the DB-backed end-to-end group (kept modest so the one-time DB
/// build stays quick; the resident group covers the larger sizes).
const E2E_SIZES: &[usize] = &[320, 1_000];

// ===========================================================================
// Deterministic synthetic scale-free graph (Rust port of the reference
// `.benchmarks/native/bench_csar_native.py::scale_free_graph`).
// ===========================================================================

/// A tiny deterministic LCG (Numerical Recipes constants) so the synthetic
/// graph is reproducible across runs/platforms without pulling an RNG crate.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1))
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    /// Uniform in `[0, n)`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        // Top 53 bits → f64 in [0,1).
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Build a Barabási–Albert-style preferential-attachment [`CodeGraph`] in CSR
/// form: hub-heavy, symmetrized, parallel edges summed, neighbours sorted
/// ascending, isolated nodes carry a unit self-loop — exactly the invariants
/// `build_code_graph` establishes (so the kernel sees the same graph shape).
fn scale_free_graph(n: usize, m: usize, seed: u64) -> CodeGraph {
    let mut rng = Lcg::new(seed);
    let mut acc: Vec<BTreeMap<i32, f64>> = vec![BTreeMap::new(); n];
    let mut repeated: Vec<i32> = Vec::new();
    let start = m.min(n);

    for new in start..n {
        let mut chosen: Vec<i32> = Vec::with_capacity(m);
        while chosen.len() < m {
            let t = if !repeated.is_empty() && rng.unit() < 0.8 {
                repeated[rng.below(repeated.len())]
            } else {
                rng.below(new) as i32
            };
            if !chosen.contains(&t) {
                chosen.push(t);
            }
        }
        for &t in &chosen {
            *acc[new].entry(t).or_insert(0.0) += 1.0;
            *acc[t as usize].entry(new as i32).or_insert(0.0) += 1.0;
            repeated.push(t);
        }
        repeated.push(new as i32);
    }

    finalize_csr(acc, n)
}

/// Convert adjacency maps to a CSR [`CodeGraph`] with the `build_code_graph`
/// invariants (sorted rows, weighted degree, self-loop for isolated nodes).
fn finalize_csr(acc: Vec<BTreeMap<i32, f64>>, n: usize) -> CodeGraph {
    let node_ids: Vec<String> = (0..n).map(node_id).collect();
    let index = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();

    let mut indptr = Vec::with_capacity(n + 1);
    let mut indices = Vec::new();
    let mut weights = Vec::new();
    let mut degree = Vec::with_capacity(n);
    indptr.push(0i32);

    for (u, nb) in acc.into_iter().enumerate() {
        if nb.is_empty() {
            // Isolated node → single unit self-loop (column-stochastic).
            indices.push(u as i32);
            weights.push(1.0);
            degree.push(1.0);
        } else {
            let mut d = 0.0;
            // BTreeMap iterates ascending → rows already sorted.
            for (&v, &w) in &nb {
                indices.push(v);
                weights.push(w);
                d += w;
            }
            degree.push(d);
        }
        indptr.push(indices.len() as i32);
    }

    CodeGraph {
        indptr,
        indices,
        weights,
        degree,
        node_ids,
        index,
    }
}

/// Synthetic symbol id in the engine's `<lang>:<path>:<qname>@<hash>` shape.
fn node_id(i: usize) -> String {
    format!("rust:src/g.rs:g.f{i}@hash{i:08x}")
}

/// The 5 deterministic seed node indices, spread across the graph (replaces the
/// reference's `rng.choice`; deterministic so the bench is reproducible).
fn seed_indices(n: usize) -> Vec<usize> {
    (1..=SEED_K).map(|j| (j * n) / (SEED_K + 1)).collect()
}

/// Seed as `(node_index, mass)` with equal mass (for the kernel bench).
fn seed_pairs(n: usize) -> Vec<(i32, f64)> {
    let idx = seed_indices(n);
    let mass = 1.0 / idx.len() as f64;
    idx.into_iter().map(|i| (i as i32, mass)).collect()
}

/// Seed as a single layer of lexical [`Hit`]s (for `diffuse_seed_hits`).
fn seed_hits(g: &CodeGraph) -> Vec<Vec<Hit>> {
    let hits = seed_indices(g.n())
        .into_iter()
        .map(|i| Hit::new(g.node_ids[i].clone(), 1.0, "lexical", "bench seed"))
        .collect();
    vec![hits]
}

// ===========================================================================
// Synthetic UCKG (real `build_code_graph` path for the end-to-end group).
// ===========================================================================

/// Materialise a synthetic scale-free graph as a real UCKG on disk, so the
/// end-to-end bench drives the genuine [`SymbolStore::build_code_graph`] (native
/// K2) code path rather than a hand-built CSR. Returns the open [`Database`]
/// (its `tempfile` dir is leaked for the bench's lifetime so the file outlives
/// this call).
fn synthetic_db(n: usize, seed: u64) -> Database {
    let g = scale_free_graph(n, BA_M, seed);
    let dir = Box::leak(Box::new(
        tempfile::tempdir().expect("tempdir for synthetic uckg"),
    ));
    let mut db = Database::open(dir.path().join(format!("g{n}.db"))).expect("open synthetic uckg");

    let symbols: Vec<Symbol> = (0..n).map(make_symbol).collect();
    db.upsert_symbols(&symbols).expect("upsert symbols");

    // Insert each undirected pair once (u < v); build_code_graph re-symmetrizes.
    // Self-loops from isolated nodes are skipped (build_code_graph adds them).
    let mut edges = Vec::new();
    for u in 0..n {
        let (idx, w) = g.neighbors(u);
        for (e, &v) in idx.iter().enumerate() {
            if (v as usize) > u {
                edges.push(Edge {
                    src_id: node_id(u),
                    dst_id: node_id(v as usize),
                    kind: EdgeKind::Calls,
                    confidence: w[e],
                    meta: serde_json::Value::Null,
                });
            }
        }
    }
    db.upsert_edges(&edges).expect("upsert edges");
    db
}

/// A minimal valid [`Symbol`] for node `i`.
fn make_symbol(i: usize) -> Symbol {
    Symbol {
        id: node_id(i),
        kind: SymbolKind::Function,
        name: format!("f{i}"),
        qualified_name: format!("g.f{i}"),
        language: "rust".into(),
        module: "g".into(),
        file_path: "src/g.rs".into(),
        line_start: 1,
        line_end: 2,
        signature: Some("fn ()".into()),
        docstring: None,
        content_hash: format!("hash{i:08x}"),
        body_excerpt: Some(format!("body of f{i}")),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: vec![],
        updated_at: 0,
    }
}

// ===========================================================================
// Pillar-2 reference (recorded — NOT measured here). Printed for context so the
// Criterion-measured Rust p50 can be read against the Python baseline.
// ===========================================================================

/// One §12 reference row: `(label, n, edges, python_ms, rust_solver_ms, speedup)`.
/// EMPIRICALLY SUPPORTED (n=4 graphs incl. 1 real; single machine) — recorded in
/// `docs/native-core-rust.md` §12, reproduced here only to print as context.
const PILLAR2_REFERENCE: &[(&str, usize, usize, f64, f64, &str)] = &[
    ("requests.db (real)", 320, 2_678, 259.7, 2.18, "119x"),
    ("scale-free", 1_000, 7_968, 247.2, 2.01, "123x"),
    ("scale-free", 10_000, 79_968, 88.4, 2.63, "34x"),
    ("scale-free", 100_000, 799_968, 38.5, 2.55, "15x"),
];

fn print_reference_banner() {
    eprintln!(
        "\n=== Pillar-2 reference (docs/native-core-rust.md §12) — \
         CSAR forward-push, alpha=0.15 eps=1e-5, seed=5 nodes ===\n\
         (recorded baseline; EMPIRICALLY SUPPORTED n=4, single machine — NOT measured here)"
    );
    eprintln!(
        "  {:<20}{:>8}{:>9}{:>12}{:>16}{:>10}",
        "graph", "n", "edges", "python_ms", "rust_solver_ms", "speedup"
    );
    for (label, n, edges, py, rs, sp) in PILLAR2_REFERENCE {
        eprintln!("  {label:<20}{n:>8}{edges:>9}{py:>12.1}{rs:>16.2}{sp:>10}");
    }
    eprintln!(
        "\nCriterion below measures the *Rust* paths on this machine. Requirement 11.2: \
         `diffuse_context` p50 must not be worse than the Python Pillar-2 baseline \
         above, and improves once the CSR build is native (K2 — the `end_to_end` group \
         folds in `build_code_graph`, replacing the Python per-query `csr_ms`).\n\
         Cross-language comparison is by size class (synthetic topology is not \
         bit-identical to the numpy reference) — label results empirically-supported / \
         conjectured, never proven.\n"
    );
}

// ===========================================================================
// Benchmark groups.
// ===========================================================================

/// Group 1 — the proven forward-push kernel on a resident CSR graph
/// (≙ the §12 `rust_solver_ms`).
fn bench_kernel(c: &mut Criterion) {
    let mut group = c.benchmark_group("csar_kernel");
    for &n in SIZES {
        let g = scale_free_graph(n, BA_M, 7);
        let seed = seed_pairs(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("forward_push", n), &n, |b, _| {
            b.iter(|| approximate_ppr_push(black_box(&g), black_box(&seed), ALPHA, EPS).unwrap());
        });
    }
    group.finish();
}

/// Group 2 — `diffuse_context` compute over a resident graph (seed build + push
/// + ranking), the interactive-regime p50 once the graph is native + resident.
fn bench_diffuse_resident(c: &mut Criterion) {
    let mut group = c.benchmark_group("diffuse_context_resident");
    for &n in SIZES {
        let g = scale_free_graph(n, BA_M, 7);
        let seeds = seed_hits(&g);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("diffuse_seed_hits", n), &n, |b, _| {
            b.iter(|| {
                diffuse_seed_hits(black_box(&g), black_box(&seeds), TOPK, ALPHA, EPS).unwrap()
            });
        });
    }
    group.finish();
}

/// Group 3 — end-to-end `diffuse_context` from an indexed UCKG: native CSR build
/// (K2) + diffuse. This is the Requirement 11.2 p50 path; the native build folds
/// in the cost the Python reference paid per query as `csr_ms`.
fn bench_diffuse_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("diffuse_context_end_to_end");
    for &n in E2E_SIZES {
        let db = synthetic_db(n, 7);
        // Resolve seed ids up front (independent of the timed build).
        let seed_ids: Vec<String> = seed_indices(n).into_iter().map(node_id).collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("build_graph+diffuse", n), &n, |b, _| {
            b.iter(|| {
                let g = db.build_code_graph(None).unwrap();
                let hits: Vec<Hit> = seed_ids
                    .iter()
                    .map(|id| Hit::new(id.clone(), 1.0, "lexical", "bench seed"))
                    .collect();
                diffuse_seed_hits(black_box(&g), &[hits], TOPK, ALPHA, EPS).unwrap()
            });
        });
    }
    group.finish();
}

/// Optional Group 4 — the same two measurements on a *real* UCKG when
/// `COGNIS_DIFFUSE_DB` points to one. Skips (prints why) otherwise — never
/// fabricates a number on a missing DB.
fn bench_real_db(c: &mut Criterion) {
    let Ok(path) = std::env::var("COGNIS_DIFFUSE_DB") else {
        eprintln!(
            "SKIP real-DB diffuse bench: set COGNIS_DIFFUSE_DB to a real .cognis/uckg.db \
             for an apples-to-apples p50 vs the Python Pillar-2 baseline on the same DB."
        );
        return;
    };
    let p = PathBuf::from(&path);
    if !p.is_file() {
        eprintln!("SKIP real-DB diffuse bench: COGNIS_DIFFUSE_DB={path:?} is not a file.");
        return;
    }
    let db = match Database::open(&p) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("SKIP real-DB diffuse bench: cannot open {path:?}: {e}");
            return;
        }
    };
    let g = match db.build_code_graph(None) {
        Ok(g) if !g.is_empty() => g,
        Ok(_) => {
            eprintln!("SKIP real-DB diffuse bench: {path:?} built an empty graph.");
            return;
        }
        Err(e) => {
            eprintln!("SKIP real-DB diffuse bench: build_code_graph failed: {e}");
            return;
        }
    };
    let n = g.n();
    eprintln!(
        "real-DB diffuse bench: {path:?} → {n} nodes, {} edges (CSR nnz)",
        g.nnz()
    );

    // Seed from the first few node ids actually present in the real graph.
    let seed_ids: Vec<String> = g.node_ids.iter().take(SEED_K).cloned().collect();
    let seeds: Vec<Vec<Hit>> = vec![seed_ids
        .iter()
        .map(|id| Hit::new(id.clone(), 1.0, "lexical", "bench seed"))
        .collect()];

    let mut group = c.benchmark_group("diffuse_context_real_db");
    group.throughput(Throughput::Elements(n as u64));
    group.bench_function(BenchmarkId::new("diffuse_seed_hits", n), |b| {
        b.iter(|| diffuse_seed_hits(black_box(&g), black_box(&seeds), TOPK, ALPHA, EPS).unwrap());
    });
    group.bench_function(BenchmarkId::new("build_graph+diffuse", n), |b| {
        b.iter(|| {
            let gg = db.build_code_graph(None).unwrap();
            diffuse_seed_hits(black_box(&gg), black_box(&seeds), TOPK, ALPHA, EPS).unwrap()
        });
    });
    group.finish();
    db.close_thread_connection();
}

fn benches(c: &mut Criterion) {
    print_reference_banner();
    bench_kernel(c);
    bench_diffuse_resident(c);
    bench_diffuse_end_to_end(c);
    bench_real_db(c);
}

criterion_group!(diffuse_latency, benches);
criterion_main!(diffuse_latency);
