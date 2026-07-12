//! Structural-understanding eval test (companion to `fair_harness.rs`). Drives
//! `cognis_eval::structure` to measure whether the engine recovers code
//! *structure* (who-calls-whom / dependency graph), not just keyword/semantic
//! relevance, and persists a diffable [`StructureReport`] baseline.
//!
//! ## Two modes, never fabricate (same discipline as the fair harness)
//!
//! 1. **Synthetic** (always, fully offline): a tiny in-memory UCKG with a
//!    hand-built call graph and an independent golden whose edge recall,
//!    reachability and coverage are all known by construction — proves the
//!    metric/harness *mechanics* are correct (evidence tier: proven by
//!    construction).
//! 2. **Real corpus** (when `.benchmarks/public/db/*.db` are present): build the
//!    harness over each indexed repo DB, compute descriptive structural coverage
//!    (+ edge comprehension when an independent `<repo>_edges.json` golden
//!    exists), print the tier-labelled report, and — when
//!    `COGNIS_WRITE_STRUCTURE_REPORT=1` — write the JSON artifact under
//!    `.benchmarks/structure/` plus an aggregate `RESULTS.md`. The write is
//!    opt-in so a normal `cargo test` never mutates the tree (baselines are
//!    refreshed deliberately, per `docs/development-criteria.md`).

use std::path::{Path, PathBuf};

use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
use cognis_eval::structure::{
    load_golden_edges, Provenance, StructureHarness, StructureReport, DEFAULT_DEPTH,
};
use cognis_store::{Database, SymbolWriter};

// ---------------------------------------------------------------------------
// Mode 1 — synthetic end-to-end (always runs, fully offline).
// ---------------------------------------------------------------------------

fn sym(id: &str, name: &str, file_path: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        language: "python".to_string(),
        module: "m".to_string(),
        file_path: file_path.to_string(),
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

fn edge(src: &str, dst: &str, kind: EdgeKind, missing: bool) -> Edge {
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

#[test]
fn synthetic_structure_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Database::open(tmp.path().join("structure.db")).unwrap();

    // A tiny indexed repo: a handler calls a service which calls a repo; a util
    // is isolated. One edge dangles (callee not indexed).
    let handler = "python:src/web.py:handle@h1";
    let service = "python:src/svc.py:serve@h2";
    let repo = "python:src/db.py:save@h3";
    let util = "python:src/util.py:helper@h4";
    db.upsert_symbols(&[
        sym(handler, "handle", "src/web.py"),
        sym(service, "serve", "src/svc.py"),
        sym(repo, "save", "src/db.py"),
        sym(util, "helper", "src/util.py"),
    ])
    .unwrap();
    db.upsert_edges(&[
        edge(handler, service, EdgeKind::Calls, false),
        edge(service, repo, EdgeKind::Calls, false),
        edge(handler, "python:ext:log@z9", EdgeKind::Calls, true), // dangling
    ])
    .unwrap();

    let harness = StructureHarness::new(&db).unwrap();

    // --- Coverage (descriptive, known by construction) ---
    let cov = harness.coverage();
    assert_eq!(cov.n_symbols, 4);
    assert_eq!(cov.n_edges, 3);
    assert_eq!(cov.n_edges_resolved, 2);
    assert_eq!(cov.n_edges_dangling, 1);
    assert!((cov.resolution_rate - 2.0 / 3.0).abs() < 1e-12);
    // connected = {handler, service, repo} of 4 → 0.75.
    assert!((cov.connectivity - 0.75).abs() < 1e-12);
    assert_eq!(cov.by_kind.get("calls"), Some(&2));

    // --- Edge comprehension vs an independent golden ---
    // Golden: handle→serve, serve→save are true; handle→save is a true *2-hop*
    // dependency but NOT a direct edge (so it misses single-hop recall but is
    // caught by reachability@depth≥2). One golden endpoint is a ghost → skipped.
    let golden_json = r#"{
        "repo": "synthetic", "complete": true,
        "edges": [
            {"src": ["handle", "web.py"], "dst": ["serve", "svc.py"], "kind": "calls"},
            {"src": ["serve", "svc.py"], "dst": ["save", "db.py"], "kind": "calls"},
            {"src": ["handle", "web.py"], "dst": ["save", "db.py"], "kind": "calls"},
            {"src": ["handle", "web.py"], "dst": ["ghost", "none.py"], "kind": "calls"}
        ]
    }"#;
    let golden = cognis_eval::structure::parse_golden_edges(golden_json).unwrap();
    let res = harness.evaluate_golden(&golden, DEFAULT_DEPTH);

    // 3 golden edges resolvable (all handle/serve/save combos), 1 skipped (ghost).
    assert_eq!(res.resolvable, 3);
    assert_eq!(res.skipped, 1);
    // Direct edges present: handle→serve, serve→save. handle→save is not a
    // direct edge → single-hop recall = 2/3.
    assert_eq!(res.edge.golden, 3);
    assert_eq!(res.edge.matched, 2);
    assert!((res.edge.recall - 2.0 / 3.0).abs() < 1e-12);
    // Golden is complete for its src nodes; the engine predicts exactly the two
    // real edges leaving those nodes → precision = 1.0 (no over-prediction).
    assert!((res.edge.precision - 1.0).abs() < 1e-12);
    // Reachability@3 recovers handle→save via the 2-hop path → recall = 1.0.
    assert!(
        (res.reach.recall - 1.0).abs() < 1e-12,
        "reachability@{} should surface the 2-hop dependency, got {}",
        res.reach.depth,
        res.reach.recall
    );

    // --- Report serializes with all invariants in range ---
    let report = harness.report("synthetic", DEFAULT_DEPTH, Some(&golden), None);
    assert_eq!(report.mode, "golden");
    let json = report.to_json().unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["coverage"]["n_symbols"], 4);
    assert_no_fabricated_rates(&report);

    db.close_thread_connection();
}

/// Every rate in the report must be a real fraction in `[0, 1]`.
fn assert_no_fabricated_rates(report: &StructureReport) {
    let c = &report.coverage;
    for v in [c.resolution_rate, c.connectivity] {
        assert!((0.0..=1.0).contains(&v), "coverage rate out of range: {v}");
    }
    if let Some(r) = &report.edge_vs_golden {
        for v in [r.edge.precision, r.edge.recall, r.edge.f1, r.reach.recall] {
            assert!((0.0..=1.0).contains(&v), "golden rate out of range: {v}");
        }
    }
}

// ---------------------------------------------------------------------------
// Mode 2 — real corpus (when the benchmark DBs are present).
// ---------------------------------------------------------------------------

/// Workspace root = two levels up from this crate's manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `(repo tag, db file, repo dir for provenance)` for each public corpus repo.
/// The optional golden edges file is `golden/<tag>_edges.json` when present.
const REPOS: &[(&str, &str, &str)] = &[
    ("requests", "db/requests.db", "repos/requests"),
    ("fastapi", "db/fastapi.db", "repos/fastapi"),
    ("eshop", "db/eshop.db", "repos/eShop"),
    (
        "petclinic",
        "db/petclinic.db",
        "repos/spring-petclinic-microservices",
    ),
    ("jsoup", "db/jsoup.db", "repos/jsoup"),
];

#[test]
fn real_corpus_structure_report() {
    let base = workspace_root().join(".benchmarks").join("public");
    if !base.join("db").join("requests.db").is_file() {
        eprintln!(
            "SKIP structure eval: benchmark corpus not found under {:?}; \
             build it with the `.benchmarks/public` index scripts.",
            base
        );
        return;
    }

    let write = std::env::var("COGNIS_WRITE_STRUCTURE_REPORT").is_ok();
    let out_dir = workspace_root().join(".benchmarks").join("structure");
    let mut markdown = String::from("# Structural understanding — benchmark report\n\n");
    markdown.push_str(
        "Descriptive structural coverage (how much of the call/dependency graph \
         the indexer recovered) per public repo, plus edge comprehension vs an \
         independent golden where one exists. Coverage is a benchmark *trend*, \
         not a retrieval-quality claim (Pillar-1 quality is the PR-derived \
         harness). Regenerate with `COGNIS_WRITE_STRUCTURE_REPORT=1 cargo test \
         -p cognis-eval --test structure_eval`.\n\n",
    );
    let mut reports = 0usize;

    for (tag, db_rel, repo_rel) in REPOS {
        let db_path = base.join(db_rel);
        if !db_path.is_file() {
            eprintln!("  [skip] {tag}: missing db {db_path:?}");
            continue;
        }

        // Copy the DB so the test never writes WAL sidecars next to the fixture.
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join(format!("{tag}.db"));
        std::fs::copy(&db_path, &dst).expect("copy bench db");
        let db = Database::open(&dst).expect("open bench db");

        let harness = StructureHarness::new(&db).expect("build structure harness");

        // Optional independent golden edge set.
        let golden_path = base.join(format!("golden/{tag}_edges.json"));
        let golden = if golden_path.is_file() {
            match load_golden_edges(&golden_path) {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("  [skip golden] {tag}: {e}");
                    None
                }
            }
        } else {
            None
        };

        let provenance = Provenance::from_git(&base.join(repo_rel));
        let report = harness.report(*tag, DEFAULT_DEPTH, golden.as_ref(), provenance);

        // Honest invariants — always.
        let c = &report.coverage;
        assert!(c.n_symbols > 0, "{tag}: indexed DB has no symbols");
        assert!(c.n_edges > 0, "{tag}: indexed DB has no edges");
        for v in [c.resolution_rate, c.connectivity] {
            assert!((0.0..=1.0).contains(&v), "{tag}: rate out of range: {v}");
        }
        if let Some(r) = &report.edge_vs_golden {
            for v in [r.edge.recall, r.edge.precision, r.reach.recall] {
                assert!((0.0..=1.0).contains(&v), "{tag}: golden rate out of range");
            }
        }

        eprintln!(
            "\n=== {tag} — {} symbols, {} edges ({} resolved, {:.1}% resolution, \
             {:.1}% connectivity) ===",
            c.n_symbols,
            c.n_edges,
            c.n_edges_resolved,
            c.resolution_rate * 100.0,
            c.connectivity * 100.0
        );
        if let Some(r) = &report.edge_vs_golden {
            eprintln!(
                "  edge recall {:.1}% (n={} resolvable), reachability@{} {:.1}%",
                r.edge.recall * 100.0,
                r.resolvable,
                r.reach.depth,
                r.reach.recall * 100.0
            );
        } else {
            eprintln!("  [no golden] add golden/{tag}_edges.json to score comprehension");
        }
        eprintln!("  [tier] {}", report.evidence_tier);

        markdown.push_str(&report.to_markdown());

        if write {
            std::fs::create_dir_all(&out_dir).expect("create structure out dir");
            let json = report.to_json().expect("serialize report");
            std::fs::write(out_dir.join(format!("{tag}.json")), json).expect("write report json");
        }

        reports += 1;
        db.close_thread_connection();
    }

    assert!(reports > 0, "corpus present but no repo produced a report");

    if write {
        std::fs::create_dir_all(&out_dir).expect("create structure out dir");
        std::fs::write(out_dir.join("RESULTS.md"), &markdown).expect("write RESULTS.md");
        eprintln!("\nWrote {reports} structure report(s) + RESULTS.md to {out_dir:?}");
    } else {
        eprintln!(
            "\n{reports} report(s) computed (not written). Set \
             COGNIS_WRITE_STRUCTURE_REPORT=1 to persist the baseline artifact."
        );
    }
}

/// Precision (over-linking) probe on petclinic — the improvement lever recall
/// cannot see. Recall answers "did the engine find the true edges"; precision
/// answers "does it *also* invent edges that are not there". We use a tiny
/// `complete = true` golden over two methods whose full bodies are known from
/// source, so **every** true call to an indexed symbol is enumerated:
///
/// * `Owner.addPet` → `getPetsInternal` (Owner) + `setOwner` (Pet) — the only
///   two calls whose targets are indexed (`.add(..)` is `java.util.Set`).
/// * `Owner.getPets` → `getPetsInternal` (Owner) — the only indexed-target call
///   (`ArrayList::new`, `PropertyComparator.sort`, `Collections.unmodifiableList`
///   are library).
///
/// With the golden complete for these nodes, `precision < 1.0` means the engine
/// linked an edge that source does not support (over-linking) — a concrete
/// place to improve. This is a *reported* diagnostic, not a hard gate.
#[test]
fn petclinic_precision_probe() {
    let base = workspace_root().join(".benchmarks").join("public");
    let db_path = base.join("db/petclinic.db");
    if !db_path.is_file() {
        eprintln!("SKIP precision probe: petclinic.db not present");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("petclinic.db");
    std::fs::copy(&db_path, &dst).unwrap();
    let db = Database::open(&dst).unwrap();
    let harness = StructureHarness::new(&db).unwrap();

    // Source-exhaustive golden for two bounded methods (see doc comment).
    let golden_json = r#"{
        "repo": "petclinic-precision-probe", "complete": true,
        "edges": [
            {"src": ["addPet", "Owner.java"], "dst": ["getPetsInternal", "Owner.java"], "kind": "calls"},
            {"src": ["addPet", "Owner.java"], "dst": ["setOwner", "Pet.java"], "kind": "calls"},
            {"src": ["getPets", "Owner.java"], "dst": ["getPetsInternal", "Owner.java"], "kind": "calls"}
        ]
    }"#;
    let golden = cognis_eval::structure::parse_golden_edges(golden_json).unwrap();
    let res = harness.evaluate_golden(&golden, DEFAULT_DEPTH);

    eprintln!(
        "\n=== petclinic precision probe (complete golden, addPet/getPets) ===\n  \
         resolvable {} (skipped {}) · edge recall {:.1}% · precision {:.1}% \
         (predicted {} edges from these nodes, matched {})",
        res.resolvable,
        res.skipped,
        res.edge.recall * 100.0,
        res.edge.precision * 100.0,
        res.edge.predicted,
        res.edge.matched
    );
    if res.edge.precision < 1.0 {
        eprintln!(
            "  [signal] precision < 100%: the engine links {} edge(s) from these \
             nodes beyond the {} source supports → over-linking to improve.",
            res.edge.predicted - res.edge.matched,
            res.edge.golden
        );
    } else {
        eprintln!("  [signal] precision 100%: no over-linking on these nodes.");
    }

    // Honest invariants only — this is a measurement, not a gate.
    assert_eq!(
        res.skipped, 0,
        "probe golden must fully resolve on petclinic"
    );
    for v in [res.edge.recall, res.edge.precision, res.edge.f1] {
        assert!((0.0..=1.0).contains(&v), "probe rate out of range: {v}");
    }
    db.close_thread_connection();
}
