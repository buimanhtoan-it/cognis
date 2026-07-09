//! Property-based fix + consistency tests for `indexd-vec0-legacy-crash`
//! (Task 4.2, design.md → Testing Strategy → Property-Based Tests / Consistency).
//!
//! These tests exercise the FIXED behaviour (Property 1) across many generated
//! DB shapes and pin the cross-surface consistency guarantee (Req 2.3). They are
//! new coverage — the Task 1/2 tests (`vec0_legacy_crash.rs`,
//! `vec0_legacy_preservation.rs`) are left untouched.
//!
//! Property 1 (fix): for a DB with a random symbol/edge/FTS population plus a
//! synthetic legacy `vec0` `symbol_vec`, the shared open path
//! (`Database::open`) and `IndexerPipeline::open` both succeed, all indexed
//! UCKG symbol data is preserved, `symbol_vec` is healed to a plain-BLOB table,
//! and a follow-up `upsert_embeddings` + `vec_search` round-trips.
//!
//! Consistency (Req 2.3): after healing a legacy DB once, opening the *same* DB
//! through the `health`, `doctor`, and `indexd` entry points yields identical
//! post-heal state (all see a plain-BLOB `symbol_vec`, none crash).
//!
//! The buggy fixture is crafted **module-free** (no sqlite-vec toolchain): a
//! fresh DB is populated via the public writer surface, then its plain-BLOB
//! `symbol_vec` is rewritten into a `CREATE VIRTUAL TABLE ... USING vec0(...)`
//! `sqlite_master` row (+ `symbol_vec_*` shadow tables) via
//! `PRAGMA writable_schema=ON`. This mirrors the pattern in `vec0_legacy_crash.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cognis_cli::{build_health_report, HealthStatus};
use cognis_core::{Config, Edge, EdgeKind, Symbol, SymbolKind};
use cognis_indexer::IndexerPipeline;
use cognis_store::{Database, SymbolStore, SymbolWriter};
use proptest::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Module-free legacy-vec0 crafting (self-contained; mirrors vec0_legacy_crash.rs)
// ---------------------------------------------------------------------------

/// SQL that rewrites a DB's plain-BLOB `symbol_vec` into a legacy `vec0`
/// virtual table plus `symbol_vec_*` shadow tables — module-free.
///
/// Dropping the plain table first removes its `sqlite_autoindex_symbol_vec_1`
/// (otherwise the rewritten master row orphans that index and the DB reopens
/// "malformed"). The virtual-table row is inserted with `rootpage = 0`, which
/// is how SQLite records a virtual table on disk.
const CRAFT_LEGACY_VEC0_SQL: &str = "\
    DROP TABLE IF EXISTS symbol_vec;\n\
    CREATE TABLE IF NOT EXISTS symbol_vec_chunks(chunk_id INTEGER PRIMARY KEY, data BLOB);\n\
    CREATE TABLE IF NOT EXISTS symbol_vec_rowids(rowid INTEGER PRIMARY KEY, symbol_id TEXT);\n\
    INSERT INTO symbol_vec_chunks(chunk_id, data) VALUES (1, x'00');\n\
    INSERT INTO symbol_vec_rowids(rowid, symbol_id) VALUES (1, 's:legacy');\n\
    PRAGMA writable_schema=ON;\n\
    INSERT INTO sqlite_master(type,name,tbl_name,rootpage,sql) \
        VALUES('table','symbol_vec','symbol_vec',0,\
        'CREATE VIRTUAL TABLE symbol_vec USING vec0(symbol_id TEXT PRIMARY KEY, embedding FLOAT[384])');\n\
    PRAGMA writable_schema=OFF;\n";

/// Rewrite the `symbol_vec` of the DB at `db_path` into a legacy `vec0` virtual
/// table, leaving all other tables (symbol/edge/symbol_fts/...) intact.
fn craft_legacy_vec0(db_path: &Path) {
    let db = Database::open(db_path).expect("open for crafting");
    {
        let conn = db.connect().expect("connect for crafting");
        conn.execute_batch(CRAFT_LEGACY_VEC0_SQL)
            .expect("craft legacy vec0 schema");
    }
    // Drop the crafting connection so the next open re-reads the schema cookie.
    db.close_thread_connection();
}

/// Config that selects the deterministic `stub` embedder (384-d, no model, no
/// I/O) so `IndexerPipeline::open` builds an embedder and reaches
/// `reconcile_embedding_dim` — the code path that would hit the `vec0` `DROP`.
fn stub_embedder_config() -> Config {
    let mut cfg = Config::default();
    cfg.embedder.backend = "stub".into();
    cfg.embedder.dim = 384;
    cfg
}

/// Minimal valid [`Symbol`] for a generated id (satisfies the NOT NULL columns
/// and `Symbol::validate`).
fn make_symbol(id: &str) -> Symbol {
    Symbol {
        id: id.to_string(),
        kind: SymbolKind::Function,
        name: id.to_string(),
        qualified_name: format!("m::{id}"),
        language: "rust".to_string(),
        module: "m".to_string(),
        file_path: "src/m.rs".to_string(),
        line_start: 1,
        line_end: 2,
        signature: None,
        docstring: None,
        content_hash: "hash".to_string(),
        body_excerpt: None,
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 0,
    }
}

/// A `Calls` edge between two existing symbol ids.
fn make_edge(src: &str, dst: &str) -> Edge {
    Edge {
        src_id: src.to_string(),
        dst_id: dst.to_string(),
        kind: EdgeKind::Calls,
        confidence: 1.0,
        meta: serde_json::Value::Null,
    }
}

/// The `CREATE` SQL recorded for `symbol_vec` in `sqlite_master`, or `None` when
/// the table is absent.
fn symbol_vec_sql(db: &Database) -> Option<String> {
    let conn = db.connect().expect("connect");
    conn.query_row(
        "SELECT sql FROM sqlite_master \
         WHERE type IN ('table','view') AND name = 'symbol_vec'",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// True when `symbol_vec` is the healed plain-BLOB fallback (not a `vec0`
/// virtual table).
fn symbol_vec_is_plain_blob(db: &Database) -> bool {
    match symbol_vec_sql(db) {
        Some(sql) => {
            let upper = sql.to_ascii_uppercase();
            upper.contains("BLOB") && !upper.contains("USING VEC0")
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Property 1 (fix) — random population + synthetic legacy vec0 heals on open
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    /// For a DB with a random symbol/edge/FTS population and a synthetic legacy
    /// `vec0` `symbol_vec`: `Database::open` and `IndexerPipeline::open` both
    /// succeed (no `no such module: vec0` crash), symbol/edge/FTS data is
    /// preserved, `symbol_vec` is healed to a plain-BLOB table with no rows,
    /// and a follow-up `upsert_embeddings` + `vec_search` round-trips.
    ///
    /// **Validates: Requirements 2.1, 2.2, 2.4, 2.5**
    #[test]
    fn legacy_vec0_heals_and_supports_upsert_and_search(
        n_symbols in 1usize..=6usize,
        edge_idx in prop::collection::vec((0usize..6, 0usize..6), 0..10),
        embed_dim in 2usize..=8usize,
        n_embed in 0usize..=6usize,
    ) {
        // --- Build the generated population. ---
        let ids: Vec<String> = (0..n_symbols).map(|i| format!("s:{i}")).collect();
        let symbols: Vec<Symbol> = ids.iter().map(|id| make_symbol(id)).collect();

        // Distinct, non-self edges between existing symbols (dedupe by src,dst).
        let mut edge_pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (a, b) in edge_idx {
            if a < n_symbols && b < n_symbols && a != b {
                edge_pairs.insert((a, b));
            }
        }
        let edges: Vec<Edge> = edge_pairs
            .iter()
            .map(|(a, b)| make_edge(&ids[*a], &ids[*b]))
            .collect();
        let expected_symbols = n_symbols as i64;
        let expected_edges = edges.len() as i64;

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("uckg.db");

        // --- Populate a fresh DB, then craft the legacy vec0 fixture on top. ---
        {
            let mut db = Database::open(&db_path).expect("open fresh DB");
            db.upsert_symbols(&symbols).expect("upsert symbols");
            db.upsert_edges(&edges).expect("upsert edges");
            db.close_thread_connection();
        }
        craft_legacy_vec0(&db_path);

        // --- The shared open path heals the legacy vec0 DB (no crash). ---
        let opened = Database::open(&db_path);
        prop_assert!(
            opened.is_ok(),
            "Database::open must self-heal a legacy vec0 DB: {:?}",
            opened.err()
        );
        opened.unwrap().close_thread_connection();

        // --- indexd's open path also succeeds (builds embedder, reconciles). ---
        let pipeline = IndexerPipeline::open(&db_path, stub_embedder_config());
        prop_assert!(
            pipeline.is_ok(),
            "IndexerPipeline::open must self-heal a legacy vec0 DB: {:?}",
            pipeline.err()
        );
        drop(pipeline);

        // --- Data preserved + symbol_vec healed to a plain-BLOB empty table. ---
        {
            let db = Database::open(&db_path).expect("reopen healed DB");
            prop_assert_eq!(db.count("symbol").expect("count symbol"), expected_symbols);
            prop_assert_eq!(db.count("edge").expect("count edge"), expected_edges);
            prop_assert_eq!(db.count("symbol_fts").expect("count fts"), expected_symbols);
            prop_assert!(
                db.vec_symbol_ids().is_ok(),
                "symbol_vec should be a queryable plain-BLOB table after heal"
            );
            prop_assert!(
                symbol_vec_is_plain_blob(&db),
                "symbol_vec must be healed to a plain-BLOB table, not a vec0 vtable"
            );
            prop_assert_eq!(
                db.vec_row_count().expect("vec_row_count"),
                0,
                "legacy vectors cleared on heal (repopulated on next index)"
            );
            db.close_thread_connection();
        }

        // --- Follow-up upsert_embeddings + vec_search round-trips on the heal. ---
        // One-hot vectors over `embed_dim` keep each embedded symbol on its own
        // orthogonal axis, so the self-match query is the unique nearest hit.
        let m = n_embed.min(n_symbols).min(embed_dim);
        if m > 0 {
            let mut db = Database::open(&db_path).expect("reopen for embeddings");
            db.reconcile_embedding_dim(embed_dim).expect("reconcile dim");

            let rows: Vec<(String, Vec<f32>)> = (0..m)
                .map(|j| {
                    let mut v = vec![0.0f32; embed_dim];
                    v[j] = 1.0;
                    (ids[j].clone(), v)
                })
                .collect();
            db.upsert_embeddings(&rows).expect("upsert embeddings");

            prop_assert_eq!(
                db.vec_row_count().expect("vec_row_count after upsert"),
                m,
                "every embedded symbol is stored"
            );

            // Query along the first embedded symbol's axis: it is the unique
            // nearest hit, and the full result set is exactly the embedded ids.
            let mut query = vec![0.0f32; embed_dim];
            query[0] = 1.0;
            let hits = db.vec_search(&query, m).expect("vec_search");
            prop_assert_eq!(hits.len(), m, "vec_search returns all embedded rows");
            prop_assert_eq!(
                &hits[0].symbol_id,
                &ids[0],
                "self-match is the nearest hit"
            );
            let got: BTreeSet<String> = hits.iter().map(|h| h.symbol_id.clone()).collect();
            let want: BTreeSet<String> = (0..m).map(|j| ids[j].clone()).collect();
            prop_assert_eq!(got, want, "vec_search returns exactly the embedded ids");

            db.close_thread_connection();
        }
    }
}

// ---------------------------------------------------------------------------
// Consistency (Req 2.3) — health / doctor / indexd reach identical post-heal state
// ---------------------------------------------------------------------------

/// A post-heal snapshot of the observable DB state the three surfaces must
/// agree on: preserved row counts, an empty plain-BLOB `symbol_vec`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PostHealState {
    symbols: i64,
    edges: i64,
    fts: i64,
    vec_rows: usize,
    plain_blob: bool,
}

/// Read the observable post-heal state through a fresh `Database` handle.
fn snapshot(db_path: &Path) -> PostHealState {
    let db = Database::open(db_path).expect("open for snapshot");
    let state = PostHealState {
        symbols: db.count("symbol").expect("count symbol"),
        edges: db.count("edge").expect("count edge"),
        fts: db.count("symbol_fts").expect("count fts"),
        vec_rows: db.vec_row_count().expect("vec_row_count"),
        plain_blob: symbol_vec_is_plain_blob(&db) && db.vec_symbol_ids().is_ok(),
    };
    db.close_thread_connection();
    state
}

/// A legacy-`vec0` fixture at `<dir>/.cognis/uckg.db` (the workspace layout the
/// CLI/indexd resolve), populated with a small known symbol/edge/FTS set.
struct LegacyRepo {
    _dir: TempDir,
    repo_root: PathBuf,
    db_path: PathBuf,
}

fn legacy_repo() -> LegacyRepo {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().to_path_buf();
    let cognis_dir = repo_root.join(".cognis");
    fs::create_dir_all(&cognis_dir).expect("mkdir .cognis");
    let db_path = cognis_dir.join("uckg.db");

    // Populate a small known graph, then rewrite symbol_vec into a legacy vtable.
    {
        let mut db = Database::open(&db_path).expect("open fresh DB");
        let symbols = [make_symbol("s:a"), make_symbol("s:b"), make_symbol("s:c")];
        db.upsert_symbols(&symbols).expect("upsert symbols");
        db.upsert_edges(&[make_edge("s:a", "s:b"), make_edge("s:b", "s:c")])
            .expect("upsert edges");
        db.close_thread_connection();
    }
    craft_legacy_vec0(&db_path);

    LegacyRepo {
        _dir: dir,
        repo_root,
        db_path,
    }
}

/// After healing a legacy DB once, opening the same DB through the `health`,
/// `doctor`, and `indexd` entry points yields identical post-heal state — all
/// see a plain-BLOB `symbol_vec`, none crash. (design.md Consistency, Req 2.3)
///
/// **Validates: Requirements 2.1, 2.3**
#[test]
fn health_doctor_indexd_reach_identical_post_heal_state() {
    // These surfaces resolve the DB via COGNIS_DB_PATH first; clear any
    // inherited override so we exercise the fixture at `<repo>/.cognis/uckg.db`.
    std::env::remove_var("COGNIS_DB_PATH");
    let fx = legacy_repo();

    // Heal the DB once via the shared open path, then capture the baseline.
    Database::open(&fx.db_path)
        .expect("initial heal open")
        .close_thread_connection();
    let baseline = snapshot(&fx.db_path);

    // The baseline itself is the expected post-heal shape: 3 symbols / 2 edges /
    // 3 FTS rows, an empty plain-BLOB symbol_vec.
    assert_eq!(baseline.symbols, 3, "symbol data preserved by heal");
    assert_eq!(baseline.edges, 2, "edge data preserved by heal");
    assert_eq!(baseline.fts, 3, "FTS data preserved by heal");
    assert_eq!(baseline.vec_rows, 0, "legacy vectors cleared on heal");
    assert!(
        baseline.plain_blob,
        "symbol_vec healed to a plain-BLOB table"
    );

    // --- health entry point: no crash, vector check degrades to warn. ---
    let report = build_health_report(&fx.repo_root);
    let vector = report
        .checks
        .iter()
        .find(|(name, _)| name == "vector")
        .map(|(_, check)| check.status);
    assert_eq!(
        vector,
        Some(HealthStatus::Warn),
        "vector check should degrade to warn (empty BLOB), not crash"
    );
    let health_state = snapshot(&fx.db_path);

    // --- doctor entry point: spawn the CLI binary, assert exit 0. ---
    let exe = env!("CARGO_BIN_EXE_cognis-cli");
    let out = std::process::Command::new(exe)
        .arg("--repo-root")
        .arg(&fx.repo_root)
        .arg("doctor")
        .env_remove("COGNIS_DB_PATH")
        .output()
        .expect("run cognis-cli doctor");
    assert!(
        out.status.success(),
        "doctor must exit 0 on a healed legacy DB; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doctor_state = snapshot(&fx.db_path);

    // --- indexd entry point: IndexerPipeline::open succeeds (no exit 1). ---
    let pipeline = IndexerPipeline::open(&fx.db_path, stub_embedder_config());
    assert!(
        pipeline.is_ok(),
        "indexd open must succeed on a healed legacy DB: {:?}",
        pipeline.err()
    );
    drop(pipeline);
    let indexd_state = snapshot(&fx.db_path);

    // All three surfaces observe the identical post-heal state, and none crash.
    assert_eq!(health_state, baseline, "health path diverged from baseline");
    assert_eq!(doctor_state, baseline, "doctor path diverged from baseline");
    assert_eq!(indexd_state, baseline, "indexd path diverged from baseline");
}
