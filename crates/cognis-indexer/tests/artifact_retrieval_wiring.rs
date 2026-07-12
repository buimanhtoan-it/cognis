//! Retrieval-wiring integration tests for non-code artifact coverage (task 10.4).
//!
//! These exercise the *retrieval-layer wiring* of artifact symbols through the
//! **public** `IndexerPipeline` + `cognis_store::Database` API, asserting that
//! artifact symbols ride the identical lexical/semantic insertion path as code
//! symbols:
//!
//! * Req 10.2 — WHERE an embedder is configured, each emitted artifact symbol is
//!   embedded into the *same* semantic vector layer (`symbol_vec`) that code
//!   symbols use, and is retrievable by semantic `vec_search`.
//! * Req 10.3 — WHERE no embedder is configured, artifact symbols are indexed
//!   into the lexical layer only (`symbol_vec` stays empty), preserving the
//!   pre-feature semantic behaviour.
//! * Req 10.4 — IF embedding of an artifact symbol fails, the indexer skips that
//!   symbol, continues the batch, and leaves the set of indexed code symbols
//!   unchanged.
//!
//! The pipeline embeds one file's symbols at a time and swallows a batch embed
//! failure (`embed_and_persist`: `Ok(v) if v.len() == symbols.len()` else skip),
//! keeping the already-committed lexical/structural rows intact. Req 10.4 is a
//! per-symbol contract; the closest faithful behaviour observable through the
//! public API is a *per-file* embed failure isolated to the artifact file, which
//! leaves every code file's symbols and vectors untouched. That is what the
//! failing-embedder test drives, and what the assertions verify.
//!
//! Each pipeline gets its own on-disk temp database. In-memory (`":memory:"`)
//! handles are cached per thread by absolute path in `cognis-store`, so two
//! `":memory:"` opens on one thread would alias the same database — the tests
//! here compare two independent index runs, so they must not share state.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cognis_core::{Config, Result};
use cognis_embed::Embedder;
use cognis_indexer::IndexerPipeline;
use cognis_store::{Database, SymbolStore};

/// A tiny deterministic embedder: a 26-d bag-of-letters vector (one bucket per
/// ascii letter, L2-normalised). No model, no I/O, offline — replicated from the
/// pipeline unit tests so the index→persist→vec_search seam can be asserted
/// without the ONNX backend.
#[derive(Debug)]
struct BagOfLettersEmbedder;

impl Embedder for BagOfLettersEmbedder {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_text(t)).collect()
    }
}

/// A deterministic embedder that *fails* (returns a length-mismatched batch, the
/// same failure shape `embed_and_persist` swallows) for any file batch whose
/// text contains `sentinel`, and embeds every other batch normally. Used to
/// simulate an artifact-symbol embedding failure that is isolated to the artifact
/// file, so code files still embed cleanly (Req 10.4).
#[derive(Debug)]
struct SentinelFailingEmbedder {
    sentinel: &'static str,
}

impl Embedder for SentinelFailingEmbedder {
    fn embedding_dim(&self) -> usize {
        26
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(bag_of_letters(text))
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.iter().any(|t| t.contains(self.sentinel)) {
            // Length mismatch (0 != texts.len()): the pipeline treats this as a
            // batch embed failure and skips embeddings for this file, leaving the
            // lexical/structural rows intact.
            return Ok(Vec::new());
        }
        texts.iter().map(|t| self.embed_text(t)).collect()
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

/// A unique temp directory for this process + monotonic nonce.
fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-artifact-retrieval-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An isolated on-disk database under `dir` (avoids the per-thread `":memory:"`
/// aliasing that would make two independent index runs share state).
fn disk_db(dir: &std::path::Path) -> Database {
    Database::open(dir.join("index.db")).expect("open on-disk db")
}

/// Write a repo holding one code file and one YAML artifact file under `dir`. The
/// YAML leaf key `flux_capacitor` is a distinctive sentinel used to isolate the
/// artifact file's embed batch from the code file's.
fn write_mixed_repo(dir: &std::path::Path) {
    std::fs::write(
        dir.join("auth.py"),
        "def authenticate(token):\n    \"\"\"verify the password then start a session\"\"\"\n    return verify(token)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("service.yaml"),
        "service:\n  port: 8080\n  flux_capacitor: enabled\n",
    )
    .unwrap();
}

/// Ids of the symbols persisted from the YAML artifact file.
fn artifact_symbol_ids(db: &Database) -> BTreeSet<String> {
    db.list_symbols()
        .unwrap()
        .into_iter()
        .filter(|s| s.file_path.ends_with(".yaml"))
        .map(|s| s.id)
        .collect()
}

/// Names of the symbols persisted from the code (.py) file.
fn code_symbol_names(db: &Database) -> BTreeSet<String> {
    db.list_symbols()
        .unwrap()
        .into_iter()
        .filter(|s| s.file_path.ends_with(".py"))
        .map(|s| s.name)
        .collect()
}

/// Count of persisted code (.py) symbols.
fn code_symbol_count(db: &Database) -> usize {
    db.list_symbols()
        .unwrap()
        .into_iter()
        .filter(|s| s.file_path.ends_with(".py"))
        .count()
}

/// Req 10.2: with an embedder configured, artifact symbols are embedded into the
/// same `symbol_vec` layer as code symbols and are retrievable via `vec_search`.
#[test]
fn artifact_symbols_embed_into_the_same_vector_layer_as_code() {
    let dir = unique_dir("embed");
    write_mixed_repo(&dir);
    let db = disk_db(&dir);
    let mut pipe = IndexerPipeline::with_embedder(
        db.clone(),
        Config::default(),
        Some(Box::new(BagOfLettersEmbedder)),
    )
    .unwrap();
    let stats = pipe.index_repo(&dir, true).unwrap();

    // The artifact file contributed at least one symbol to the index.
    let artifact_ids = artifact_symbol_ids(&db);
    assert!(
        !artifact_ids.is_empty(),
        "the YAML artifact file should emit at least one symbol"
    );

    // Every indexed symbol — code and artifact alike — got a persisted vector
    // through the identical embed path, so the vector count equals the symbol
    // count (Req 10.2: artifact symbols embed into the same layer as code).
    assert_eq!(
        db.vec_row_count().unwrap(),
        stats.symbols_indexed,
        "every indexed symbol (code + artifact) should have an embedding row"
    );

    // Semantic retrieval actually returns the artifact symbols: with k == the
    // full row count, vec_search surfaces every vectorised symbol, so each
    // artifact id must appear among the semantic hits.
    let query = BagOfLettersEmbedder
        .embed_text("service port flux capacitor")
        .unwrap();
    let hits = db.vec_search(&query, stats.symbols_indexed).unwrap();
    let hit_ids: BTreeSet<String> = hits.into_iter().map(|h| h.symbol_id).collect();
    for id in &artifact_ids {
        assert!(
            hit_ids.contains(id),
            "artifact symbol {id} should be retrievable from the semantic vector layer"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Req 10.3: with no embedder, artifact symbols are indexed into the lexical
/// layer only — `symbol_vec` stays empty while the artifact symbols remain
/// present (searchable), preserving pre-feature semantic behaviour.
#[test]
fn no_embedder_degrades_artifacts_to_lexical_only() {
    let dir = unique_dir("noembed");
    write_mixed_repo(&dir);
    let db = disk_db(&dir);
    let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
    pipe.index_repo(&dir, true).unwrap();

    // No embedder → the semantic vector layer is empty for artifacts and code.
    assert_eq!(
        db.vec_row_count().unwrap(),
        0,
        "no embedder should leave the semantic vector layer empty"
    );

    // The artifact symbols are still present in the (lexical) index.
    assert!(
        !artifact_symbol_ids(&db).is_empty(),
        "artifact symbols should still be indexed lexically with no embedder"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Req 10.4: when embedding of the artifact file's symbols fails, the indexer
/// skips those symbols, the batch still completes, and the indexed code symbols
/// (and their vectors) are left unchanged.
#[test]
fn failing_artifact_embedding_is_skipped_leaving_code_symbols_intact() {
    // Control run (own isolated DB): a clean embedder over the same repo shape.
    // Records the code symbol set and code-vector count we expect preserved.
    let control_dir = unique_dir("ctrl");
    write_mixed_repo(&control_dir);
    let control_db = disk_db(&control_dir);
    let mut control_pipe = IndexerPipeline::with_embedder(
        control_db.clone(),
        Config::default(),
        Some(Box::new(BagOfLettersEmbedder)),
    )
    .unwrap();
    control_pipe.index_repo(&control_dir, true).unwrap();
    let expected_code_names = code_symbol_names(&control_db);
    let expected_code_vec_rows = code_symbol_count(&control_db);
    assert!(
        expected_code_vec_rows > 0,
        "control run should index (and embed) at least one code symbol"
    );
    std::fs::remove_dir_all(&control_dir).ok();

    // Failure run (own isolated DB): the embedder fails for the artifact file
    // batch only (the YAML leaf `flux_capacitor` is the sentinel), and embeds the
    // code file normally.
    let dir = unique_dir("fail");
    write_mixed_repo(&dir);
    let db = disk_db(&dir);
    let mut pipe = IndexerPipeline::with_embedder(
        db.clone(),
        Config::default(),
        Some(Box::new(SentinelFailingEmbedder {
            sentinel: "flux_capacitor",
        })),
    )
    .unwrap();

    // The batch still completes despite the artifact embedding failure.
    let stats = pipe.index_repo(&dir, true).unwrap();

    // Code symbols are unchanged: the same set as the clean control run, and
    // still present/searchable in the index.
    assert_eq!(
        code_symbol_names(&db),
        expected_code_names,
        "the set of indexed code symbols must be unchanged by the artifact failure"
    );

    // The artifact symbols were still written lexically (write precedes embed);
    // only their vectors were skipped.
    assert!(
        !artifact_symbol_ids(&db).is_empty(),
        "artifact symbols should remain in the lexical index despite the embed failure"
    );

    // Only the code symbols got vectors: the skipped artifact batch means the
    // vector count equals the code-symbol count, strictly below the total.
    let vec_rows = db.vec_row_count().unwrap();
    assert_eq!(
        vec_rows, expected_code_vec_rows,
        "only code symbols should be embedded when the artifact batch fails"
    );
    assert!(
        vec_rows < stats.symbols_indexed,
        "the skipped artifact symbols should be absent from the vector layer"
    );

    std::fs::remove_dir_all(&dir).ok();
}
