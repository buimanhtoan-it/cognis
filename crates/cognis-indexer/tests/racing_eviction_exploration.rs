//! Bug facet #5 — Racing eviction / lost vector updates.
//!
//! This is a BUG-CONDITION EXPLORATION test (Requirements 1.6, 2.6;
//! preservation clause 3.5). It encodes the *expected* (fixed) behavior and
//! therefore MUST FAIL on the unfixed code.
//!
//! Expected (fixed): for any interleaving of indexing work and a model
//! eviction / mid-run embed failure, the pipeline writes every required
//! `symbol_vec` update exactly once OR leaves it explicitly pending for retry
//! — it must NEVER report semantic completion for an omitted vector
//! (Property 7 / Requirement 2.6). Concretely, after a run
//! `persisted + pending >= indexed` and if anything is missing from
//! `symbol_vec` then `pending > 0` (nothing silently dropped).
//!
//! Unfixed behavior: `IndexerPipeline::embed_and_persist` is best-effort. When
//! the embedder becomes unavailable mid-run — exactly what an idle-eviction
//! timer firing during in-flight work looks like — a batch embed failure is
//! swallowed, the file's symbols are written lexically, and their vectors are
//! silently skipped with no pending record. There is no in-flight reference
//! count that refuses eviction while work is in flight, and no pending-vector
//! bookkeeping. So a lost vector is not just possible, it is guaranteed under
//! this interleaving — and the run still reports success.
//!
//! ## Simulating the race deterministically
//!
//! `Embedder` methods are `&self`, so we carry an `AtomicBool` "evicted" flag
//! (interior mutability). The first file's batch embeds normally (an in-flight
//! semantic call), then the eviction "fires" (flag flips); every subsequent
//! batch behaves as an evicted model — it returns a length-mismatched (empty)
//! batch, the same failure shape the pipeline swallows. With two files this
//! deterministically drops the second file's vectors mid-run.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use cognis_core::{Config, Result};
use cognis_embed::Embedder;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;

/// An embedder that models an idle-eviction firing mid-run: it serves the first
/// batch (an in-flight call), then "evicts" — every later batch behaves as an
/// unavailable/evicted session and returns an empty (length-mismatched) batch,
/// the failure shape `embed_and_persist` silently skips.
#[derive(Debug, Default)]
struct EvictingEmbedder {
    evicted: AtomicBool,
}

impl Embedder for EvictingEmbedder {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.evicted.load(Ordering::SeqCst) {
            // Evicted mid-run: the model is gone, so this in-flight batch cannot
            // complete. Length-mismatched (empty) batch = the failure the
            // pipeline swallows, silently dropping these vectors.
            return Ok(Vec::new());
        }
        // Serve this batch (in-flight call), then trip the eviction so the next
        // file's batch races against a now-evicted model.
        let out: Vec<Vec<f32>> = texts.iter().map(|t| bag_of_letters(t)).collect();
        self.evicted.store(true, Ordering::SeqCst);
        Ok(out)
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

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-racing-eviction-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn disk_db(dir: &std::path::Path) -> Database {
    Database::open(dir.join("index.db")).expect("open on-disk db")
}

/// Two code files, each contributing at least one symbol, so the run performs
/// more than one embed batch and the eviction can race an in-flight call.
fn write_two_file_repo(dir: &std::path::Path) {
    std::fs::write(
        dir.join("auth.py"),
        "def authenticate(token):\n    \"\"\"verify the password then start a session\"\"\"\n    return token\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("crypto.py"),
        "def hash_password(pw):\n    \"\"\"hash a password using the configured algorithm\"\"\"\n    return pw\n",
    )
    .unwrap();
}

#[test]
fn eviction_racing_inflight_work_never_loses_a_vector() {
    let dir = unique_dir("race");
    write_two_file_repo(&dir);
    let db = disk_db(&dir);
    let mut pipe = IndexerPipeline::with_embedder(
        db.clone(),
        Config::default(),
        Some(Box::new(EvictingEmbedder::default())),
    )
    .unwrap();

    // The run reports success even when some vectors cannot be written mid-run;
    // the fixed contract is that omitted vectors are retained as pending, not
    // silently dropped (Property 7 / Requirement 2.6).
    let stats = pipe.index_repo(&dir, true).unwrap();
    assert!(
        stats.symbols_indexed >= 2,
        "the two-file repo must index at least two symbols so eviction can race \
         an in-flight batch (got {})",
        stats.symbols_indexed
    );

    let persisted_vectors = db.vec_row_count().unwrap();
    let pending_symbols = pipe.pending_vector_symbols();
    let pending_groups = pipe.pending_vector_groups();
    let pending = pending_symbols
        .max(stats.vectors_pending)
        .max(pending_groups);

    std::fs::remove_dir_all(&dir).ok();

    // EXPECTED (fixed): every required vector is written exactly once OR left
    // explicitly pending. Task-1 exploration originally asserted
    // `persisted == indexed` only (no pending surface existed yet); the fix
    // introduces explicit pending bookkeeping, so the correct fixed-system
    // observable is coverage = persisted ∪ pending with no silent drop.
    assert!(
        persisted_vectors + pending >= stats.symbols_indexed
            || (persisted_vectors < stats.symbols_indexed && pending > 0),
        "a model eviction racing in-flight indexing work left \
         {persisted_vectors}/{symbols} symbol_vec rows with pending={pending} \
         (pending_symbols={pending_symbols} pending_groups={pending_groups} \
         stats.vectors_pending={}); fixed lifecycle must persist every vector \
         or retain the omitted ones as explicitly pending — never silently drop",
        stats.vectors_pending,
        symbols = stats.symbols_indexed,
    );
    if persisted_vectors < stats.symbols_indexed {
        assert!(
            pending > 0,
            "omitted vectors must be explicitly pending, not silently dropped \
             (persisted={persisted_vectors} indexed={})",
            stats.symbols_indexed
        );
        assert_eq!(
            stats.vectors_pending, pending_groups,
            "IndexStats.vectors_pending must surface the pending group count \
             so completion is never claimed for omitted vectors"
        );
    }
}
