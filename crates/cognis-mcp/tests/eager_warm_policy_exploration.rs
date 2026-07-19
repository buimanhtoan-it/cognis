//! Bug facet #3/#4 — Ignored warm policy + eager per-process ONNX load.
//!
//! These are BUG-CONDITION EXPLORATION tests (Requirements 1.4, 1.5; expected
//! behavior 2.4, 2.5). They encode the *expected* (fixed) behavior and
//! therefore MUST FAIL on the unfixed code:
//!
//!   * facet #3 (ignored warm policy): with
//!     `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0` the engine must NOT build an
//!     embedder at `open` (lazy). On unfixed code `StoreEngine::open`
//!     unconditionally calls `cognis_embed::build_embedder(&config).ok()`, so
//!     the env var has no effect — there is no lazy path to observe.
//!
//!   * facet #4 (absent-env legacy Eager): with no warm-policy env set,
//!     `StoreEngine::open` retains the documented legacy Eager warm
//!     (Requirement 2.4: absent → Eager). Task-1 originally hypothesized
//!     "absent ⇒ lazy"; the design finalized absent → Eager for direct-launch
//!     compatibility, while the extension's generated default is the explicit
//!     value `"0"` (Lazy) covered by facet #3.
//!
//! ## Why this is observable without ONNX
//!
//! The workspace test build is offline (no `onnx` feature), so
//! `build_embedder` for the default `local` backend returns `Err` and the
//! eager `.ok()` yields `None`. That makes "was a model built at open?"
//! invisible through the public API in this build. To surface the eager-vs-lazy
//! *decision* deterministically we drive the exact same policy the fix must
//! honor through a stub backend (`embedder.backend = "stub"`), which is always
//! constructible offline. The unfixed `open` builds it eagerly regardless of
//! the env var; the fixed `open` must consult the policy and skip construction
//! when the policy is Lazy.
//!
//! We assert on `semantic_available()` as the public, side-effect-free proxy
//! for "a model/embedder was constructed and wired at open". With a populated
//! `symbol_vec` and a stub embedder, an eager `open` reports semantic available
//! immediately (no tool call has happened); a lazy `open` must report it
//! unavailable until first demand.

use std::path::PathBuf;
use std::sync::Mutex;

use cognis_mcp::engine::RetrievalEngine;
use cognis_mcp::store_engine::StoreEngine;
use cognis_store::{Database, SymbolWriter};

// These tests mutate one process-global environment variable and may otherwise
// race under Rust's parallel test runner.
static WARM_POLICY_ENV_LOCK: Mutex<()> = Mutex::new(());

/// A unique on-disk repo layout `<dir>/.cognis/uckg.db` so `StoreEngine::open`
/// can infer the repo root and load `<dir>/.cognis/config.yaml`.
fn unique_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-eager-warm-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join(".cognis")).unwrap();
    dir
}

/// Write a `.cognis/config.yaml` selecting the always-constructible `stub`
/// backend so the eager-vs-lazy decision is observable offline (no ONNX).
fn write_stub_config(repo: &std::path::Path, dim: u32) {
    std::fs::write(
        repo.join(".cognis").join("config.yaml"),
        format!("embedder:\n  backend: stub\n  dim: {dim}\n"),
    )
    .unwrap();
}

/// Seed the UCKG at `<repo>/.cognis/uckg.db` with one symbol and a populated
/// `symbol_vec` (dim `dim`) so `semantic_available()` depends solely on whether
/// an embedder was wired at open — isolating the eager/lazy signal.
fn seed_db_with_vectors(repo: &std::path::Path, dim: usize) -> PathBuf {
    use cognis_core::{Symbol, SymbolKind};

    let db_path = repo.join(".cognis").join("uckg.db");
    let mut db = Database::open(&db_path).expect("open uckg");
    let sym = Symbol {
        id: "python:src/auth.py:auth.authenticate@a1".into(),
        kind: SymbolKind::Function,
        name: "authenticate".into(),
        qualified_name: "auth.authenticate".into(),
        language: "python".into(),
        module: "auth".into(),
        file_path: "src/auth.py".into(),
        line_start: 1,
        line_end: 10,
        signature: Some("def authenticate(...)".into()),
        docstring: None,
        content_hash: "abcd1234".into(),
        body_excerpt: Some("verify the password then start a session".into()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 1_700_000_000,
    };
    db.upsert_symbols(std::slice::from_ref(&sym)).unwrap();
    db.reconcile_embedding_dim(dim).unwrap();
    let vec: Vec<f32> = {
        let mut v = vec![0.0f32; dim];
        if dim > 0 {
            v[0] = 1.0;
        }
        v
    };
    db.upsert_embeddings(&[(sym.id.clone(), vec)]).unwrap();
    assert!(
        db.vec_row_count().unwrap() > 0,
        "seed must populate symbol_vec so semantic_available depends only on the embedder"
    );
    db_path
}

/// Facet #3 — the lazy warm policy (`=0`) is ignored: a fixed engine must not
/// have a semantic-capable session wired at open when the policy is Lazy, yet
/// the unfixed `open` builds the embedder eagerly regardless of the env var.
#[test]
fn warm_policy_zero_defers_model_load_until_demand() {
    let _env_guard = WARM_POLICY_ENV_LOCK.lock().unwrap();
    let repo = unique_repo("policy0");
    let dim = 8usize;
    write_stub_config(&repo, dim as u32);
    let db_path = seed_db_with_vectors(&repo, dim);

    // Emit the extension's lazy signal. The fix must consume this and defer the
    // embedder construction until first semantic demand.
    std::env::set_var("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "0");

    let engine = StoreEngine::open(db_path.to_str().unwrap()).expect("open engine");

    // EXPECTED (fixed): lazy policy ⇒ no embedder wired at open ⇒ semantic not
    // yet available before any tool call. On unfixed code the embedder is built
    // eagerly in `open`, so with a populated symbol_vec this is `true`.
    let available_at_open = engine.semantic_available();

    std::env::remove_var("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP");
    std::fs::remove_dir_all(&repo).ok();

    assert!(
        !available_at_open,
        "with COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0 the engine must defer model \
         construction until first demand (zero ONNX/embedder session resident at \
         open); unfixed code builds the embedder eagerly in StoreEngine::open and \
         ignores the warm policy"
    );
}

/// Facet #4 / documented policy — absent env keeps legacy Eager.
///
/// Finalized precedence (Requirement 2.4, Property 5): absent → Eager for
/// legacy/direct-launch compatibility. The extension's generated default is
/// the literal `"0"` (Lazy), covered by
/// [`warm_policy_zero_defers_model_load_until_demand`]; an *absent* variable
/// is a different input and must keep the historical warm-at-open path.
///
/// Task-1 exploration originally asserted "absent ⇒ no model" under an early
/// hypothesis that the extension default was "env unset". The design finalized
/// absent → Eager; this trivial expectation alignment matches that policy so
/// the exploration suite validates the fixed system rather than the rejected
/// hypothesis.
#[test]
fn open_maps_model_when_warm_policy_env_is_absent() {
    let _env_guard = WARM_POLICY_ENV_LOCK.lock().unwrap();
    let repo = unique_repo("eager");
    let dim = 8usize;
    write_stub_config(&repo, dim as u32);
    let db_path = seed_db_with_vectors(&repo, dim);

    // No warm-policy env set at all: documented legacy / direct-launch baseline.
    // Absent → Eager, so a constructible stub backend is wired at open.
    std::env::remove_var("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP");

    let engine = StoreEngine::open(db_path.to_str().unwrap()).expect("open engine");

    // Proxy for "a model/embedder session is resident with zero tool calls".
    let mapped_at_open = engine.semantic_available();

    std::fs::remove_dir_all(&repo).ok();

    assert!(
        mapped_at_open,
        "with COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP absent, StoreEngine::open must \
         retain the legacy Eager warm (semantic-capable session at open); only \
         the explicit Lazy value (\"0\", the extension-generated default) defers \
         construction until first demand"
    );
}
