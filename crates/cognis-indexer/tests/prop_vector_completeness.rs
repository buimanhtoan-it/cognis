//! Property-based + unit tests for indexer vector completeness under lazy load.
//!
//! Feature: mcp-process-ram-duplication
//! **Property 7: Bug Condition** — Indexer lazy load with no lost vectors and
//! safe idle eviction
//!
//! **Validates: Requirements 2.6**
//!
//! _For any_ interleaving of edits, embed failures (simulating eviction races),
//! and retries, the persisted `symbol_vec` set plus the explicitly pending set
//! covers every currently indexed symbol — nothing is silently dropped, and
//! idle eviction is refused while pending / in-flight work remains.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cognis_core::{Config, Result};
use cognis_embed::{Embedder, EvictOutcome};
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Deterministic embedders
// ---------------------------------------------------------------------------

/// Always-succeeding 26-d bag-of-letters embedder.
#[derive(Debug, Default)]
struct BagOfLetters;

impl Embedder for BagOfLetters {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| bag_of_letters(t)).collect())
    }
}

/// Flips to "unavailable" after `fail_after` successful batches (0 ⇒ first
/// batch fails). Models mid-run eviction / embed failure: returns a
/// length-mismatched empty batch that the pipeline retains as pending.
#[derive(Debug)]
struct FailAfterN {
    fail_after: AtomicUsize,
    batches: AtomicUsize,
}

impl FailAfterN {
    fn new(fail_after: usize) -> Self {
        Self {
            fail_after: AtomicUsize::new(fail_after),
            batches: AtomicUsize::new(0),
        }
    }
}

impl Embedder for FailAfterN {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let n = self.batches.fetch_add(1, Ordering::SeqCst);
        if n >= self.fail_after.load(Ordering::SeqCst) {
            // Unavailable / evicted: length mismatch → NeedPending path.
            return Ok(Vec::new());
        }
        Ok(texts.iter().map(|t| bag_of_letters(t)).collect())
    }
}

/// Toggleable embedder shared with the test via `Arc<AtomicBool>`.
/// When unavailable every batch fails with a length mismatch (pending path).
/// Keeping the slot Ready (not force-evicting) matches the production
/// in-flight/unavailable contract the PBT exercises; real idle eviction of an
/// injected embedder would permanently disable demand-load (`with_embedder`
/// sets `allow_demand_load = false`).
#[derive(Debug)]
struct SharedToggle {
    available: Arc<AtomicBool>,
}

impl Embedder for SharedToggle {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if !self.available.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        Ok(texts.iter().map(|t| bag_of_letters(t)).collect())
    }
}

fn bag_of_letters(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; 26];
    for c in text.to_ascii_lowercase().chars() {
        if c.is_ascii_lowercase() {
            v[(c as u8 - b'a') as usize] += 1.0;
        }
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-vec-complete-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn disk_db(dir: &Path) -> Database {
    Database::open(dir.join("index.db")).expect("open on-disk db")
}

fn write_py(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn fn_src(fn_name: &str, word: &str) -> String {
    format!("def {fn_name}(x):\n    \"\"\"{word} documentation for {fn_name}\"\"\"\n    return x\n")
}

/// Invariant: every live symbol is either in `symbol_vec` or explicitly
/// pending. No silent drop. Never report completion for omitted vectors.
fn assert_vector_completeness(pipe: &IndexerPipeline, db: &Database) {
    let symbols = db.list_symbols().unwrap();
    let symbol_ids: std::collections::BTreeSet<String> =
        symbols.into_iter().map(|s| s.id).collect();
    let vec_ids: std::collections::BTreeSet<String> =
        db.vec_symbol_ids().unwrap().into_iter().collect();
    let pending = pipe.pending_vector_symbols();
    let pending_groups = pipe.pending_vector_groups();
    let persisted = vec_ids.len();

    // Every persisted vector must belong to a live symbol.
    for id in &vec_ids {
        assert!(
            symbol_ids.contains(id),
            "orphan vector id {id} not in symbol set"
        );
    }

    // Coverage: missing live symbols must be covered by explicit pending.
    let missing = symbol_ids.len().saturating_sub(persisted);
    assert!(
        pending >= missing || pending_groups >= 1 && missing > 0 || missing == 0,
        "silently dropped vectors: live_symbols={} persisted={} pending={} \
         pending_groups={} missing={missing}",
        symbol_ids.len(),
        persisted,
        pending,
        pending_groups
    );

    // Never report semantic completion for omitted vectors: if anything is
    // missing from symbol_vec, pending must be non-zero.
    if persisted < symbol_ids.len() {
        assert!(
            pending > 0 || pending_groups > 0,
            "vectors missing from symbol_vec without explicit pending \
             (live={} vec={persisted})",
            symbol_ids.len()
        );
    }

    // When pending is zero, every live symbol must have a vector.
    if pending == 0 && pending_groups == 0 {
        assert_eq!(
            persisted,
            symbol_ids.len(),
            "with zero pending, symbol_vec count must equal the live symbol set \
             (vec={persisted} live={})",
            symbol_ids.len()
        );
        for id in &symbol_ids {
            assert!(
                vec_ids.contains(id),
                "live symbol {id} missing from symbol_vec with zero pending"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Property 7
// ---------------------------------------------------------------------------

/// One step in a random edit / failure / retry / eviction-attempt interleaving.
///
/// "Eviction" of the *session while work is pending* is checked via
/// `TryEvictPending` (must refuse). Model unavailability is modeled by
/// `Fail`/`Recover` on a still-Ready slot so the NeedPending path is exercised
/// without permanently removing an injected embedder.
#[derive(Debug, Clone)]
enum Step {
    /// Re-index a file with a new function body (edit).
    Edit { file_idx: usize, rev: u8 },
    /// Attempt idle eviction — only meaningful when pending work exists.
    TryEvictPending,
    /// Flip the toggle embedder on (recover) and re-index / retry.
    Recover,
    /// Flip the toggle embedder off (simulate unavailable / mid-run eviction).
    Fail,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0usize..3, 0u8..5).prop_map(|(file_idx, rev)| Step::Edit { file_idx, rev }),
        Just(Step::TryEvictPending),
        Just(Step::Recover),
        Just(Step::Fail),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    // Feature: mcp-process-ram-duplication, Property 7: Bug Condition —
    // Indexer lazy load with no lost vectors and safe idle eviction
    // **Validates: Requirements 2.6**
    //
    // For random edit / unavailability / retry interleavings, the persisted
    // `symbol_vec` set equals the required set or is explicitly pending —
    // never silently dropped. Idle eviction is refused while pending work
    // remains.
    #[test]
    fn vector_completeness_under_edit_evict_retry(
        steps in proptest::collection::vec(step_strategy(), 1..12),
        start_available in any::<bool>(),
    ) {
        let dir = unique_dir("pbt");
        // Three files so edits and failures interleave across symbol groups.
        let files = ["a.py", "b.py", "c.py"];
        for (i, f) in files.iter().enumerate() {
            write_py(&dir, f, &fn_src(&format!("fn{i}"), "alpha"));
        }

        let db = disk_db(&dir);
        let available = Arc::new(AtomicBool::new(start_available));

        let mut pipe = IndexerPipeline::with_embedder(
            db.clone(),
            Config::default(),
            Some(Box::new(SharedToggle {
                available: Arc::clone(&available),
            })),
        )
        .unwrap();
        // Make idle-eviction checks attemptable without sleeping 300s.
        pipe.set_idle_evict_after(Duration::ZERO);

        // Cold index first.
        let stats = pipe.index_repo(&dir, true).unwrap();
        prop_assert!(stats.symbols_indexed >= 3);
        assert_vector_completeness(&pipe, &db);

        for step in steps {
            match step {
                Step::Edit { file_idx, rev } => {
                    let f = files[file_idx % files.len()];
                    let name = format!("fn{}_{}", file_idx % files.len(), rev);
                    write_py(&dir, f, &fn_src(&name, "beta"));
                    let path = dir.join(f);
                    let _ = pipe.index_batch(&dir, &[path]);
                }
                Step::TryEvictPending => {
                    let pending_before = pipe.pending_vector_groups();
                    if pending_before == 0 {
                        // No pending work: skip actual eviction of an injected
                        // embedder (with_embedder cannot demand-reload). The
                        // refuse-while-pending contract is what we verify.
                        continue;
                    }
                    let outcome = pipe.try_idle_evict_model();
                    prop_assert!(
                        matches!(outcome, EvictOutcome::InFlight { .. }),
                        "idle eviction must refuse with pending work, got {outcome:?}"
                    );
                    prop_assert!(
                        pipe.pending_vector_groups() > 0,
                        "pending work must be retained across eviction attempt"
                    );
                }
                Step::Recover => {
                    available.store(true, Ordering::SeqCst);
                    // Re-index all files so retry_pending_vectors runs.
                    let paths: Vec<PathBuf> =
                        files.iter().map(|f| dir.join(f)).collect();
                    let _ = pipe.index_batch(&dir, &paths);
                }
                Step::Fail => {
                    available.store(false, Ordering::SeqCst);
                    // Touch one file so embed path sees the unavailable model.
                    let path = dir.join(files[0]);
                    write_py(&dir, files[0], &fn_src("fn0_fail", "gamma"));
                    let _ = pipe.index_batch(&dir, &[path]);
                }
            }
            assert_vector_completeness(&pipe, &db);
        }

        // Final recovery: everything that is still live must land in symbol_vec
        // once the model is available again.
        available.store(true, Ordering::SeqCst);
        let paths: Vec<PathBuf> = files.iter().map(|f| dir.join(f)).collect();
        let stats = pipe.index_batch(&dir, &paths).unwrap();
        assert_vector_completeness(&pipe, &db);
        if stats.vectors_pending == 0 {
            let live = db.list_symbols().unwrap().len();
            prop_assert_eq!(
                db.vec_row_count().unwrap(),
                live,
                "after full recovery symbol_vec must equal the live set"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// Unit tests (Requirement 2.6)
// ---------------------------------------------------------------------------

#[test]
fn mid_run_failure_retains_pending_not_silent_drop() {
    let dir = unique_dir("fail-after");
    write_py(
        &dir,
        "auth.py",
        "def authenticate(token):\n    \"\"\"verify the password then start a session\"\"\"\n    return token\n",
    );
    write_py(
        &dir,
        "crypto.py",
        "def hash_password(pw):\n    \"\"\"hash a password using the configured algorithm\"\"\"\n    return pw\n",
    );

    let db = disk_db(&dir);
    // First file batch succeeds, subsequent batches fail → second file pending.
    let mut pipe = IndexerPipeline::with_embedder(
        db.clone(),
        Config::default(),
        Some(Box::new(FailAfterN::new(1))),
    )
    .unwrap();

    let stats = pipe.index_repo(&dir, true).unwrap();
    assert!(stats.symbols_indexed >= 2);

    let persisted = db.vec_row_count().unwrap();
    let pending = pipe.pending_vector_symbols();
    assert!(
        persisted + pending >= stats.symbols_indexed
            || persisted + pipe.pending_vector_groups() >= 1,
        "required vectors must be persisted or pending (persisted={persisted} \
         pending_symbols={pending} indexed={})",
        stats.symbols_indexed
    );
    // Critical: if anything is missing from symbol_vec, pending must be > 0.
    if persisted < stats.symbols_indexed {
        assert!(
            pending > 0 || pipe.pending_vector_groups() > 0,
            "omitted vectors must be explicitly pending, not silently dropped"
        );
        assert_eq!(
            stats.vectors_pending,
            pipe.pending_vector_groups(),
            "IndexStats.vectors_pending must surface the pending count"
        );
    }
    assert_vector_completeness(&pipe, &db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn idle_evict_refused_while_pending_and_allowed_when_clear() {
    let dir = unique_dir("evict");
    write_py(&dir, "a.py", &fn_src("alpha", "one"));

    let db = disk_db(&dir);
    let available = Arc::new(AtomicBool::new(false));

    let mut pipe = IndexerPipeline::with_embedder(
        db.clone(),
        Config::default(),
        Some(Box::new(SharedToggle {
            available: Arc::clone(&available),
        })),
    )
    .unwrap();
    pipe.set_idle_evict_after(Duration::ZERO);

    // Index with unavailable model → pending vectors; model stays Ready
    // (injected) but work is pending.
    let _ = pipe.index_repo(&dir, true).unwrap();
    assert!(
        pipe.pending_vector_groups() > 0,
        "unavailable embed must leave explicit pending"
    );
    match pipe.try_idle_evict_model() {
        EvictOutcome::InFlight { count } => assert!(count > 0),
        other => panic!("expected InFlight while pending, got {other:?}"),
    }

    // Recover: clear pending, then eviction is allowed.
    available.store(true, Ordering::SeqCst);
    let path = dir.join("a.py");
    let stats = pipe.index_batch(&dir, &[path]).unwrap();
    assert_eq!(stats.vectors_pending, 0);
    assert_eq!(pipe.pending_vector_groups(), 0);
    assert_vector_completeness(&pipe, &db);

    // Model was Ready (injected); with zero idle + no pending → Evicted.
    assert_eq!(pipe.try_idle_evict_model(), EvictOutcome::Evicted);
    assert!(!pipe.model_loaded());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lazy_open_has_zero_session_before_demand() {
    let dir = unique_dir("lazy-open");
    std::fs::create_dir_all(dir.join(".cognis")).unwrap();
    std::fs::write(
        dir.join(".cognis").join("config.yaml"),
        "embedder:\n  backend: stub\n  dim: 8\n",
    )
    .unwrap();
    let db_path = dir.join(".cognis").join("uckg.db");

    let mut cfg = Config::default();
    cfg.embedder.backend = "stub".into();
    cfg.embedder.dim = 8;

    let pipe =
        IndexerPipeline::open_with_policy(&db_path, cfg, cognis_core::SemanticWarmPolicy::Lazy)
            .unwrap();
    assert!(
        !pipe.model_loaded(),
        "Lazy open must keep zero ONNX/session resident before demand"
    );
    let snap = pipe.work_snapshot();
    assert!(!snap.model_loaded);
    assert_eq!(snap.pending_count, 0);
    assert_eq!(snap.inflight_count, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_success_path_persists_every_vector_exactly_once() {
    let dir = unique_dir("full-ok");
    write_py(&dir, "a.py", &fn_src("alpha", "word"));
    write_py(&dir, "b.py", &fn_src("beta", "term"));

    let db = disk_db(&dir);
    let mut pipe =
        IndexerPipeline::with_embedder(db.clone(), Config::default(), Some(Box::new(BagOfLetters)))
            .unwrap();

    let stats = pipe.index_repo(&dir, true).unwrap();
    assert!(stats.symbols_indexed >= 2);
    assert_eq!(stats.vectors_pending, 0);
    assert_eq!(db.vec_row_count().unwrap(), stats.symbols_indexed);
    assert_eq!(pipe.pending_vector_groups(), 0);
    assert_vector_completeness(&pipe, &db);

    // Re-index is idempotent: still exact coverage, no pending.
    let stats2 = pipe.index_repo(&dir, true).unwrap();
    assert_eq!(stats2.vectors_pending, 0);
    assert_eq!(db.vec_row_count().unwrap(), stats2.symbols_indexed);
    assert_vector_completeness(&pipe, &db);

    let _ = std::fs::remove_dir_all(&dir);
}
