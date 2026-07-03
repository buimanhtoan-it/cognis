//! Indexer symbol/edge count-parity test (Task 8.3, Requirement 9.2).
//!
//! Property **P-PAR-IDX** (design `Correctness Properties`): indexing the same
//! pinned repository with the Rust pipeline reproduces the Python indexer's
//! symbol and edge counts within a recorded tolerance. The oracle counts are
//! the ones captured in `tests/e2e/baselines/requests.json` (`index_stats`),
//! produced by the Python `cognis_indexer` on `psf/requests`
//! (`v2.34.2-6-g1190afd1`).
//!
//! ## Graceful skip (never fabricate numbers)
//!
//! The test indexes the **real** `requests` checkout. When that checkout or the
//! baseline JSON is absent (a clean CI box without the benchmark repos), the
//! test prints why it skipped and returns `Ok` rather than inventing counts —
//! same discipline as the ONNX parity test. Point it at a checkout explicitly
//! with `COGNIS_REQUESTS_REPO=/path/to/requests`.
//!
//! ## Tolerance, and why it isn't zero
//!
//! Exact equality between two independent reimplementations (Python
//! tree-sitter + resolver vs. the Rust ports) is not guaranteed: minor
//! extraction differences (which AST nodes count as a symbol, fuzzy-match edge
//! tie-breaks) can shift counts by a few percent. The gate therefore asserts
//! both counts stay within [`TOLERANCE`] of the Python baseline rather than
//! demanding bit-equality.
//!
//! **Measured (psf/requests `v2.34.2-6-g1190afd1`):** the Rust pipeline
//! currently reproduces the baseline *exactly* — 736 symbols and 25 985 edges,
//! a 0 % delta on both. The 15 % band is headroom for future parser/resolver
//! tweaks; if a change moves the numbers, re-confirm the delta is explainable
//! before widening it. (The `file`-row count differs — Rust reports ~30 vs the
//! baseline's 37 — because the Rust pipeline does not yet persist `file` cache
//! rows for symbol-less files; Requirement 9.2 scopes the gate to symbol/edge
//! counts, so that difference is out of scope here.)

use std::path::{Path, PathBuf};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;

/// Fractional tolerance on each count vs. the Python baseline (±). Symbol and
/// edge counts must land within this band for the parity gate to pass.
const TOLERANCE: f64 = 0.15;

/// Workspace root = two levels up from this crate's manifest dir
/// (`crates/cognis-indexer` → repo root).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn requests_repo() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COGNIS_REQUESTS_REPO") {
        if !p.trim().is_empty() {
            let pb = PathBuf::from(p);
            return pb.is_dir().then_some(pb);
        }
    }
    let default = workspace_root()
        .join(".benchmarks")
        .join("public")
        .join("repos")
        .join("requests");
    default.is_dir().then_some(default)
}

fn baseline_counts() -> Option<(u64, u64, u64)> {
    let path = workspace_root()
        .join("tests")
        .join("e2e")
        .join("baselines")
        .join("requests.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let stats = v.get("index_stats")?;
    let symbol = stats.get("symbol")?.as_u64()?;
    let edge = stats.get("edge")?.as_u64()?;
    let file = stats.get("file")?.as_u64()?;
    Some((symbol, edge, file))
}

fn within(actual: u64, expected: u64, tol: f64) -> bool {
    let lo = (expected as f64 * (1.0 - tol)).floor();
    let hi = (expected as f64 * (1.0 + tol)).ceil();
    (actual as f64) >= lo && (actual as f64) <= hi
}

#[test]
fn rust_indexer_symbol_edge_counts_match_python_within_tolerance() {
    let Some(repo) = requests_repo() else {
        eprintln!(
            "SKIP index parity: `requests` checkout not found \
             (.benchmarks/public/repos/requests or $COGNIS_REQUESTS_REPO). \
             Clone psf/requests there to enable the P-PAR-IDX gate."
        );
        return;
    };
    let Some((py_symbols, py_edges, py_files)) = baseline_counts() else {
        eprintln!(
            "SKIP index parity: baseline tests/e2e/baselines/requests.json \
             missing or unreadable; cannot compare without the Python oracle counts."
        );
        return;
    };

    // Index the real repo with the Rust pipeline (no embedder — counts are
    // independent of vectors). An in-memory DB keeps the test self-contained.
    let db = Database::open(":memory:").expect("open in-memory uckg");
    let mut pipeline = IndexerPipeline::new(db.clone(), Config::default());
    let stats = pipeline
        .index_repo(&repo, true)
        .expect("index requests repo");

    let rs_symbols = db.count("symbol").expect("count symbol") as u64;
    let rs_edges = db.count("edge").expect("count edge") as u64;

    eprintln!(
        "index parity (requests): rust symbols={rs_symbols} edges={rs_edges} \
         files={} | python symbols={py_symbols} edges={py_edges} files={py_files} \
         | tolerance ±{:.0}%",
        stats.files_processed,
        TOLERANCE * 100.0
    );

    // Sanity: the run actually did something.
    assert!(
        stats.files_processed > 0 && rs_symbols > 0,
        "indexer produced no symbols — parity comparison would be meaningless"
    );

    assert!(
        within(rs_symbols, py_symbols, TOLERANCE),
        "symbol count out of parity: rust={rs_symbols} python={py_symbols} \
         (tolerance ±{:.0}%)",
        TOLERANCE * 100.0
    );
    assert!(
        within(rs_edges, py_edges, TOLERANCE),
        "edge count out of parity: rust={rs_edges} python={py_edges} \
         (tolerance ±{:.0}%)",
        TOLERANCE * 100.0
    );
}
