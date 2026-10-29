//! `Index_Publisher` — atomic, generation-addressed publication.
//!
//! Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 7.1, 8.3, 11.1,
//! 14.1). This module declares the value types one change batch is expressed in
//! and the trait that applies it.
//!
//! The shape here is load-bearing for Requirement 7.1: a [`PublishBatch`] is
//! *the whole batch*, so [`IndexPublisher::publish`] can wrap every mutation
//! category in exactly one `BEGIN IMMEDIATE` transaction and advance
//! `source_generation` inside it. Splitting the batch across several calls —
//! which is what `write_file_diff` → `delete_symbols` → `embed_and_persist` does
//! today — is what makes a reader able to observe new symbols with old edges.
//!
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use cognis_core::ParseStatus;
use cognis_core::{CognisError, Edge, Result, Symbol};
use rusqlite::Connection;

use crate::{
    edge_kind_to_db, edge_meta_to_db, enum_to_db_str, floats_to_le_bytes, recreate_vec_table,
    store_err, symbol_kind_to_db, try_load_vec_extension, vec0_table_present, vec_table_dim,
    with_write_txn, Database, SYMBOL_COLUMNS,
};

/// A row of the `file` table — the `File_Cache` entry a committed batch writes
/// for every file it covered (Requirement 8.3).
///
/// Mirrors migration `001_initial.sql`'s `file` table column-for-column:
/// `path, language, size_bytes, content_hash, parsed_at, parse_status`.
///
/// `content_hash` is the **byte-exact** `sha256` of the file's contents, not
/// `parser::normalize::content_hash` — the cache must notice a cosmetic edit
/// even when symbol identity is unchanged (design §Change_Planner step 2).
///
/// `size_bytes` is `i64` rather than `u64` because SQLite's INTEGER is signed
/// and this row is read straight back out of the column; `cognis_core::FileRecord`
/// keeps its `u64` for the model-level view.
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheRow {
    /// Repository-relative path, the `file` table's primary key.
    pub path: String,
    pub language: String,
    pub size_bytes: i64,
    /// `sha256` over the file's exact bytes, hex-encoded.
    pub content_hash: String,
    /// Unix seconds at which the parse happened.
    pub parsed_at: i64,
    /// Recorded so the next reconciliation pass retries a failed parse
    /// (Requirement 8.7).
    pub parse_status: ParseStatus,
}

/// One row of `embedding_queue` — a symbol whose vector is required but not yet
/// persisted (Requirement 11.1).
///
/// `attempts`, `next_attempt_at`, and `last_error` are owned by the drainer
/// (task 8.1) and defaulted by the schema, so an *enqueue* only needs to name
/// the symbol and its file.
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSymbol {
    pub symbol_id: String,
    pub file_path: String,
}

/// Why a batch is being published. Persisted in `publish_batch.reason` so a
/// committed generation records how it came to be (Requirement 7.5).
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishReason {
    /// First index of a workspace whose database was absent or empty.
    Initial,
    /// A watcher batch: exactly the touched paths (plus their dependents).
    Incremental,
    /// Turn-on with an existing database: the tree is diffed against the cache.
    Reconcile,
    /// Every admitted path reparsed, ignoring hash equality.
    Full,
    /// Vectors rewritten under a new `Vector_Space_Fingerprint`.
    ReEmbed,
}

impl PublishReason {
    /// The label stored in `publish_batch.reason`.
    ///
    /// These are the exact strings migration `002_generation_and_cache.sql`
    /// documents: `initial | incremental | reconcile | full | reembed`.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            PublishReason::Initial => "initial",
            PublishReason::Incremental => "incremental",
            PublishReason::Reconcile => "reconcile",
            PublishReason::Full => "full",
            PublishReason::ReEmbed => "reembed",
        }
    }

    /// Inverse of [`as_db_str`](PublishReason::as_db_str). `None` for a label
    /// this build does not know, so a reader can report the unknown value
    /// rather than silently coerce it.
    pub fn parse_db_str(s: &str) -> Option<PublishReason> {
        match s {
            "initial" => Some(PublishReason::Initial),
            "incremental" => Some(PublishReason::Incremental),
            "reconcile" => Some(PublishReason::Reconcile),
            "full" => Some(PublishReason::Full),
            "reembed" => Some(PublishReason::ReEmbed),
            _ => None,
        }
    }
}

/// The persisted identity of the embedding space that produced the stored
/// vectors (Requirement 14.1).
///
/// One row of `vector_space`. `digest` is `ModelFingerprint::derive(..)` — which
/// already folds backend, model identity, asset checksums and embedding
/// dimension, and gains pooling + normalization mode in task 10.3. `dim` and
/// `model_id` are carried alongside so a mismatch can be *explained*, not merely
/// detected; the comparison the `Vector_Space_Guard` performs is over `digest`.
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorSpaceFingerprint {
    /// The value compared by the guard. Equal digests ⇒ same embedding space.
    pub digest: String,
    /// Embedding dimension. Informational: a dimension-only check is exactly the
    /// Requirement 14.4 trap.
    pub dim: usize,
    /// Model identity, e.g. `bge-small-en-v1.5`.
    pub model_id: String,
}

/// Everything one change batch mutates.
///
/// Built by the `Change_Planner` (task 7.1), applied by the [`IndexPublisher`]
/// in exactly one durable transaction (task 4.1).
///
/// The categories are deliberately explicit rather than a list of opaque
/// statements: Requirement 7.1 enumerates them, and property P1 asserts that an
/// interruption at *any* point inside the sequence leaves generation `g` or
/// `g+1` and nothing between.
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct PublishBatch {
    /// Symbols to insert-or-replace; `symbol_fts` is synced for each in the
    /// same transaction.
    pub symbol_upserts: Vec<Symbol>,
    /// Symbol ids to delete, cascading `symbol_attribute`, `symbol_vec`, the
    /// FTS row, and the queue row.
    pub symbol_deletes: Vec<String>,
    pub edge_upserts: Vec<Edge>,
    /// `(src_id, dst_id, kind)` — the composite `edge` key.
    pub edge_deletes: Vec<(String, String, String)>,
    /// `(symbol_id, vector)` pairs written to `symbol_vec`.
    pub vector_upserts: Vec<(String, Vec<f32>)>,
    pub vector_deletes: Vec<String>,
    /// `file` rows upserted for every path this batch covered.
    pub file_rows: Vec<FileCacheRow>,
    /// Paths removed from the tree: their `file` row and derived rows go.
    pub file_deletes: Vec<String>,
    /// Symbols whose vectors are still required (Requirement 7.7, 11.1).
    pub queue_enqueue: Vec<QueuedSymbol>,
    /// Symbol ids whose vectors are now persisted, or whose symbol is gone.
    pub queue_remove: Vec<String>,
    /// Set on the first vector write and on a re-embed (Requirement 14.1, 14.6).
    pub fingerprint: Option<VectorSpaceFingerprint>,
    pub reason: PublishReason,
}

impl PublishBatch {
    /// A batch that mutates no row.
    ///
    /// An empty batch is not a no-op: publishing it still advances
    /// `source_generation`, which is what lets a reconcile that found nothing
    /// record that it ran.
    pub fn empty(reason: PublishReason) -> Self {
        PublishBatch {
            symbol_upserts: Vec::new(),
            symbol_deletes: Vec::new(),
            edge_upserts: Vec::new(),
            edge_deletes: Vec::new(),
            vector_upserts: Vec::new(),
            vector_deletes: Vec::new(),
            file_rows: Vec::new(),
            file_deletes: Vec::new(),
            queue_enqueue: Vec::new(),
            queue_remove: Vec::new(),
            fingerprint: None,
            reason,
        }
    }

    /// Whether this batch mutates no row at all.
    pub fn is_empty(&self) -> bool {
        self.symbol_upserts.is_empty()
            && self.symbol_deletes.is_empty()
            && self.edge_upserts.is_empty()
            && self.edge_deletes.is_empty()
            && self.vector_upserts.is_empty()
            && self.vector_deletes.is_empty()
            && self.file_rows.is_empty()
            && self.file_deletes.is_empty()
            && self.queue_enqueue.is_empty()
            && self.queue_remove.is_empty()
    }

    /// The distinct, sorted paths this batch covers — exactly what
    /// `publish_batch_file` records for the committed generation
    /// (Requirement 7.5).
    pub fn covered_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .file_rows
            .iter()
            .map(|r| r.path.clone())
            .chain(self.file_deletes.iter().cloned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// What one committed publish produced.
///
/// Every field is read back **inside the publish transaction**, so a receipt
/// describes a state that actually existed at the moment of commit rather than
/// a state the caller inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReceipt {
    /// The generation this batch committed as (Requirement 7.2).
    pub source_generation: i64,
    /// Equal to `source_generation` iff `embedding_queue` was empty at commit
    /// (Requirement 7.6, 7.7).
    pub semantic_generation: i64,
    /// `SELECT COUNT(*) FROM embedding_queue`, evaluated in the transaction
    /// (Requirement 11.6).
    pub pending_vector_count: i64,
    /// The paths this generation covered (Requirement 7.5).
    pub files: Vec<String>,
}

/// Apply one [`PublishBatch`] atomically and advance `Source_Generation`.
pub trait IndexPublisher {
    /// Apply `batch` and advance `Source_Generation` inside ONE transaction.
    ///
    /// Contract (Requirement 7):
    ///
    /// 1. One `BEGIN IMMEDIATE` wraps every mutation category plus the
    ///    generation bump, so a reader observes exactly `g` or exactly `g+1`.
    /// 2. `source_generation` is incremented **after** all row mutations and
    ///    **inside** the same transaction.
    /// 3. `semantic_generation` advances to the new `source_generation` iff
    ///    `SELECT COUNT(*) FROM embedding_queue` is `0`, evaluated inside the
    ///    transaction rather than by the caller.
    /// 4. `publish_batch` / `publish_batch_file` record the covered paths and
    ///    the [`PublishReason`].
    /// 5. Any error rolls the whole transaction back; the database retains
    ///    generation `g` exactly.
    fn publish(&mut self, batch: &PublishBatch) -> Result<PublishReceipt>;
}

impl IndexPublisher for Database {
    fn publish(&mut self, batch: &PublishBatch) -> Result<PublishReceipt> {
        publish_with_checkpoints(self, batch, env_interrupt_checkpoint)
    }
}

/// E2E crash-during-publish hook (Requirement 24.3 / 24.4).
///
/// When `COGNIS_PUBLISH_INTERRUPT_STAGE` matches this checkpoint ordinal, write
/// `COGNIS_PUBLISH_INTERRUPT_READY` (if set) and park so the harness can kill
/// the process while the publish transaction is still open. Unset in production
/// → no-op, same as the previous unconditional `|_, _| Ok(())` callback.
fn env_interrupt_checkpoint(_conn: &Connection, ordinal: usize) -> Result<()> {
    let Ok(raw) = std::env::var("COGNIS_PUBLISH_INTERRUPT_STAGE") else {
        return Ok(());
    };
    let Ok(target) = raw.trim().parse::<usize>() else {
        return Ok(());
    };
    if ordinal != target {
        return Ok(());
    }
    if let Ok(path) = std::env::var("COGNIS_PUBLISH_INTERRUPT_READY") {
        let path = path.trim();
        if !path.is_empty() {
            let _ = std::fs::write(path, format!("{ordinal}\n"));
        }
    }
    std::thread::park();
    Ok(())
}

/// The publish sequence exposes 17 ordered boundaries to its in-module
/// atomicity test, matching `cognis_testgen::PublishStage::ALL` by ordinal.
#[cfg(test)]
const PUBLISH_CHECKPOINT_COUNT: usize = 17;

/// Test-only entry point for deterministic error, rollback, and process-death
/// injection. Production publication uses the same body with a no-op callback.
#[cfg(test)]
fn publish_with_test_checkpoint(
    db: &mut Database,
    batch: &PublishBatch,
    checkpoint: impl FnMut(&Connection, usize) -> Result<()>,
) -> Result<PublishReceipt> {
    publish_with_checkpoints(db, batch, checkpoint)
}

fn publish_with_checkpoints(
    db: &mut Database,
    batch: &PublishBatch,
    mut checkpoint: impl FnMut(&Connection, usize) -> Result<()>,
) -> Result<PublishReceipt> {
    let conn = db.connect()?;
    let files = batch.covered_paths();
    let now = unix_timestamp()?;

    let receipt = with_write_txn(&conn, |conn| {
        checkpoint(conn, 0)?;
        let file_changes = classify_file_changes(conn, batch, &files)?;

        upsert_symbols(conn, &batch.symbol_upserts)?;
        checkpoint(conn, 1)?;
        upsert_edges(conn, &batch.edge_upserts)?;
        checkpoint(conn, 2)?;
        upsert_vector_space_and_vectors(conn, batch, now)?;
        checkpoint(conn, 3)?;
        upsert_file_rows(conn, &batch.file_rows)?;
        checkpoint(conn, 4)?;
        enqueue_symbols(conn, &batch.queue_enqueue, now)?;
        checkpoint(conn, 5)?;

        delete_edges(conn, &batch.edge_deletes)?;
        checkpoint(conn, 6)?;
        delete_vectors(conn, &batch.vector_deletes)?;
        checkpoint(conn, 7)?;
        remove_queued_symbols(conn, &batch.queue_remove)?;
        checkpoint(conn, 8)?;

        let mut symbol_deletes: BTreeSet<String> = batch.symbol_deletes.iter().cloned().collect();
        collect_file_symbol_ids(conn, &batch.file_deletes, &mut symbol_deletes)?;
        checkpoint(conn, 9)?;
        delete_symbols(conn, symbol_deletes.iter().map(String::as_str))?;
        checkpoint(conn, 10)?;
        delete_file_queue_rows(conn, &batch.file_deletes)?;
        checkpoint(conn, 11)?;
        delete_file_rows(conn, &batch.file_deletes)?;
        checkpoint(conn, 12)?;

        let pending_vector_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| row.get(0))
            .map_err(store_err)?;
        checkpoint(conn, 13)?;
        let updated = conn
            .execute(
                "UPDATE generation \
                    SET source_generation = source_generation + 1, \
                        semantic_generation = CASE \
                            WHEN ?1 = 0 THEN source_generation + 1 \
                            ELSE semantic_generation \
                        END, \
                        updated_at = ?2 \
                  WHERE id = 1",
                rusqlite::params![pending_vector_count, now],
            )
            .map_err(store_err)?;
        if updated != 1 {
            return Err(CognisError::Store(
                "generation singleton row is missing".to_string(),
            ));
        }

        let (source_generation, semantic_generation): (i64, i64) = conn
            .query_row(
                "SELECT source_generation, semantic_generation \
                   FROM generation WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(store_err)?;
        checkpoint(conn, 14)?;

        let file_count = i64::try_from(files.len()).map_err(store_err)?;
        conn.execute(
            "INSERT INTO publish_batch( \
                 source_generation, committed_at, file_count, reason \
             ) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![source_generation, now, file_count, batch.reason.as_db_str(),],
        )
        .map_err(store_err)?;

        let mut insert_file = conn
            .prepare(
                "INSERT INTO publish_batch_file(source_generation, path, change) \
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(store_err)?;
        for path in &files {
            insert_file
                .execute(rusqlite::params![
                    source_generation,
                    path,
                    file_changes[path],
                ])
                .map_err(store_err)?;
        }
        checkpoint(conn, 15)?;

        Ok(PublishReceipt {
            source_generation,
            semantic_generation,
            pending_vector_count,
            files: files.clone(),
        })
    })?;

    // This boundary is intentionally after COMMIT: process death here may
    // expose g+1, while every earlier boundary is still inside the transaction.
    checkpoint(&conn, 16)?;
    Ok(receipt)
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(store_err)?
        .as_secs();
    i64::try_from(seconds).map_err(store_err)
}

fn classify_file_changes(
    conn: &Connection,
    batch: &PublishBatch,
    files: &[String],
) -> Result<BTreeMap<String, &'static str>> {
    let removed: HashSet<&str> = batch.file_deletes.iter().map(String::as_str).collect();
    let mut exists = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM file WHERE path = ?1)")
        .map_err(store_err)?;
    let mut changes = BTreeMap::new();
    for path in files {
        let change = if removed.contains(path.as_str()) {
            "removed"
        } else if exists
            .query_row(rusqlite::params![path], |row| row.get::<_, bool>(0))
            .map_err(store_err)?
        {
            "changed"
        } else {
            "added"
        };
        changes.insert(path.clone(), change);
    }
    Ok(changes)
}

fn upsert_symbols(conn: &Connection, symbols: &[Symbol]) -> Result<()> {
    if symbols.is_empty() {
        return Ok(());
    }

    let placeholders = vec!["?"; SYMBOL_COLUMNS.len()].join(", ");
    let update_clause = SYMBOL_COLUMNS
        .iter()
        .filter(|col| **col != "id")
        .map(|col| format!("{col} = excluded.{col}"))
        .collect::<Vec<_>>()
        .join(", ");
    let upsert_sql = format!(
        "INSERT INTO symbol ({}) VALUES ({placeholders}) \
         ON CONFLICT(id) DO UPDATE SET {update_clause}",
        SYMBOL_COLUMNS.join(", "),
    );

    let mut upsert = conn.prepare(&upsert_sql).map_err(store_err)?;
    let mut fts_delete = conn
        .prepare("DELETE FROM symbol_fts WHERE id = ?1")
        .map_err(store_err)?;
    let mut fts_insert = conn
        .prepare(
            "INSERT INTO symbol_fts (id, name, qualified_name, signature, \
             docstring, body_excerpt) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(store_err)?;

    for symbol in symbols {
        let kind = symbol_kind_to_db(symbol.kind)?;
        let untrusted_flags = if symbol.untrusted_flags.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&symbol.untrusted_flags).map_err(store_err)?)
        };
        upsert
            .execute(rusqlite::params![
                symbol.id,
                kind,
                symbol.name,
                symbol.qualified_name,
                symbol.language,
                symbol.module,
                symbol.file_path,
                symbol.line_start,
                symbol.line_end,
                symbol.signature,
                symbol.docstring,
                symbol.content_hash,
                symbol.body_excerpt,
                symbol.semantic_summary,
                symbol.risk_score,
                i64::from(symbol.ambiguous),
                untrusted_flags,
                symbol.updated_at,
            ])
            .map_err(store_err)?;
        fts_delete
            .execute(rusqlite::params![symbol.id])
            .map_err(store_err)?;
        fts_insert
            .execute(rusqlite::params![
                symbol.id,
                symbol.name,
                symbol.qualified_name,
                symbol.signature.as_deref().unwrap_or(""),
                symbol.docstring.as_deref().unwrap_or(""),
                symbol.body_excerpt.as_deref().unwrap_or(""),
            ])
            .map_err(store_err)?;
    }
    Ok(())
}

fn upsert_edges(conn: &Connection, edges: &[Edge]) -> Result<()> {
    let mut upsert = conn
        .prepare(
            "INSERT INTO edge (src_id, dst_id, kind, confidence, meta) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(src_id, dst_id, kind) DO UPDATE SET \
             confidence = excluded.confidence, meta = excluded.meta",
        )
        .map_err(store_err)?;
    for edge in edges {
        let kind = edge_kind_to_db(edge.kind)?;
        let meta = edge_meta_to_db(&edge.meta)?;
        upsert
            .execute(rusqlite::params![
                edge.src_id,
                edge.dst_id,
                kind,
                edge.confidence,
                meta,
            ])
            .map_err(store_err)?;
    }
    Ok(())
}

fn upsert_vector_space_and_vectors(
    conn: &Connection,
    batch: &PublishBatch,
    now: i64,
) -> Result<()> {
    let mut symbol_exists = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM symbol WHERE id = ?1)")
        .map_err(store_err)?;
    let mut rows = Vec::new();
    for (symbol_id, vector) in &batch.vector_upserts {
        if symbol_exists
            .query_row(rusqlite::params![symbol_id], |row| row.get::<_, bool>(0))
            .map_err(store_err)?
        {
            rows.push((symbol_id, vector));
        }
    }
    if rows.is_empty() {
        return Ok(());
    }

    let persisted = conn
        .query_row(
            "SELECT fingerprint, embedding_dim, model_id FROM vector_space WHERE id = 1",
            [],
            |row| {
                let dim = row.get::<_, i64>(1)?;
                Ok((row.get::<_, String>(0)?, dim, row.get::<_, String>(2)?))
            },
        )
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok((String::new(), -1, String::new())),
            other => Err(other),
        })
        .map_err(store_err)?;
    let active_dim = if let Some(fingerprint) = &batch.fingerprint {
        fingerprint.dim
    } else if persisted.1 >= 0 {
        usize::try_from(persisted.1).map_err(store_err)?
    } else {
        return Err(CognisError::Store(
            "first vector write requires a vector-space fingerprint".to_string(),
        ));
    };

    for (symbol_id, vector) in &rows {
        if vector.len() != active_dim {
            return Err(CognisError::Store(format!(
                "vector for {symbol_id:?} has dimension {}, expected {active_dim}",
                vector.len()
            )));
        }
    }

    if let Some(fingerprint) = &batch.fingerprint {
        let dim = i64::try_from(fingerprint.dim).map_err(store_err)?;
        let space_changed = !persisted.0.is_empty() && persisted.0 != fingerprint.digest;
        let want_vec0 = try_load_vec_extension(conn);
        let is_vec0 = vec0_table_present(conn)?;
        let upgrade_or_heal = want_vec0 != is_vec0;
        let dim_changed = vec_table_dim(conn)?.is_some_and(|dim| dim != fingerprint.dim);
        if upgrade_or_heal || dim_changed {
            let preserved = if !space_changed && !dim_changed && !is_vec0 && want_vec0 {
                read_compatible_blob_vectors(conn, fingerprint.dim)?
            } else {
                Vec::new()
            };
            recreate_vec_table(conn, want_vec0, fingerprint.dim)?;
            if !preserved.is_empty() {
                let mut ins = conn
                    .prepare("INSERT INTO symbol_vec(symbol_id, embedding) VALUES (?1, ?2)")
                    .map_err(store_err)?;
                for (sym_id, blob) in preserved {
                    ins.execute(rusqlite::params![sym_id, blob])
                        .map_err(store_err)?;
                }
            }
        } else if space_changed {
            // No vector from the old space may survive under the new identity.
            conn.execute("DELETE FROM symbol_vec", [])
                .map_err(store_err)?;
        }
        conn.execute(
            "INSERT INTO vector_space( \
                 id, fingerprint, embedding_dim, model_id, established_at \
             ) VALUES (1, ?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET \
                 fingerprint = excluded.fingerprint, \
                 embedding_dim = excluded.embedding_dim, \
                 model_id = excluded.model_id, \
                 established_at = excluded.established_at",
            rusqlite::params![fingerprint.digest, dim, fingerprint.model_id, now],
        )
        .map_err(store_err)?;
    }

    let mut delete = conn
        .prepare("DELETE FROM symbol_vec WHERE symbol_id = ?1")
        .map_err(store_err)?;
    let mut insert = conn
        .prepare("INSERT INTO symbol_vec(symbol_id, embedding) VALUES (?1, ?2)")
        .map_err(store_err)?;
    for (symbol_id, vector) in rows {
        delete
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        insert
            .execute(rusqlite::params![symbol_id, floats_to_le_bytes(vector)])
            .map_err(store_err)?;
    }
    Ok(())
}

fn read_compatible_blob_vectors(conn: &Connection, dim: usize) -> Result<Vec<(String, Vec<u8>)>> {
    let expected_bytes = dim.saturating_mul(4);
    let mut stmt = conn
        .prepare("SELECT symbol_id, embedding FROM symbol_vec ORDER BY symbol_id")
        .map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })
        .map_err(store_err)?;
    let mut kept = Vec::new();
    for row in rows {
        let (id, blob) = row.map_err(store_err)?;
        if blob.len() == expected_bytes {
            kept.push((id, blob));
        }
    }
    Ok(kept)
}

fn upsert_file_rows(conn: &Connection, rows: &[FileCacheRow]) -> Result<()> {
    let mut upsert = conn
        .prepare(
            "INSERT INTO file( \
                 path, language, size_bytes, content_hash, parsed_at, parse_status \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(path) DO UPDATE SET \
                 language = excluded.language, \
                 size_bytes = excluded.size_bytes, \
                 content_hash = excluded.content_hash, \
                 parsed_at = excluded.parsed_at, \
                 parse_status = excluded.parse_status",
        )
        .map_err(store_err)?;
    for row in rows {
        let parse_status = enum_to_db_str(serde_json::to_value(row.parse_status))?;
        upsert
            .execute(rusqlite::params![
                row.path,
                row.language,
                row.size_bytes,
                row.content_hash,
                row.parsed_at,
                parse_status,
            ])
            .map_err(store_err)?;
    }
    Ok(())
}

fn enqueue_symbols(conn: &Connection, symbols: &[QueuedSymbol], now: i64) -> Result<()> {
    let mut enqueue = conn
        .prepare(
            "INSERT INTO embedding_queue( \
                 symbol_id, file_path, enqueued_at, attempts, next_attempt_at, last_error \
             ) VALUES (?1, ?2, ?3, 0, 0, NULL) \
             ON CONFLICT(symbol_id) DO UPDATE SET \
                 file_path = excluded.file_path, \
                 enqueued_at = excluded.enqueued_at, \
                 attempts = 0, \
                 next_attempt_at = 0, \
                 last_error = NULL",
        )
        .map_err(store_err)?;
    for symbol in symbols {
        enqueue
            .execute(rusqlite::params![symbol.symbol_id, symbol.file_path, now])
            .map_err(store_err)?;
    }
    Ok(())
}

fn delete_edges(conn: &Connection, edges: &[(String, String, String)]) -> Result<()> {
    let mut delete = conn
        .prepare("DELETE FROM edge WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3")
        .map_err(store_err)?;
    for (src_id, dst_id, kind) in edges {
        delete
            .execute(rusqlite::params![src_id, dst_id, kind])
            .map_err(store_err)?;
    }
    Ok(())
}

fn delete_vectors(conn: &Connection, symbol_ids: &[String]) -> Result<()> {
    let mut delete = conn
        .prepare("DELETE FROM symbol_vec WHERE symbol_id = ?1")
        .map_err(store_err)?;
    for symbol_id in symbol_ids {
        delete
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
    }
    Ok(())
}

fn remove_queued_symbols(conn: &Connection, symbol_ids: &[String]) -> Result<()> {
    let mut delete = conn
        .prepare("DELETE FROM embedding_queue WHERE symbol_id = ?1")
        .map_err(store_err)?;
    for symbol_id in symbol_ids {
        delete
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
    }
    Ok(())
}

fn collect_file_symbol_ids(
    conn: &Connection,
    paths: &[String],
    symbol_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let mut select = conn
        .prepare("SELECT id FROM symbol WHERE file_path = ?1")
        .map_err(store_err)?;
    for path in paths {
        let ids = select
            .query_map(rusqlite::params![path], |row| row.get::<_, String>(0))
            .map_err(store_err)?;
        for symbol_id in ids {
            symbol_ids.insert(symbol_id.map_err(store_err)?);
        }
    }
    Ok(())
}

fn delete_symbols<'a>(
    conn: &Connection,
    symbol_ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut delete_symbol = conn
        .prepare("DELETE FROM symbol WHERE id = ?1")
        .map_err(store_err)?;
    let mut delete_fts = conn
        .prepare("DELETE FROM symbol_fts WHERE id = ?1")
        .map_err(store_err)?;
    let mut delete_vector = conn
        .prepare("DELETE FROM symbol_vec WHERE symbol_id = ?1")
        .map_err(store_err)?;
    let mut delete_queue = conn
        .prepare("DELETE FROM embedding_queue WHERE symbol_id = ?1")
        .map_err(store_err)?;
    let mut delete_outbound = conn
        .prepare("DELETE FROM edge WHERE src_id = ?1")
        .map_err(store_err)?;
    let mut flag_inbound = conn
        .prepare(
            "UPDATE edge \
                SET meta = json_patch( \
                        COALESCE(meta, '{}'), \
                        json_object('dst_missing', json('true')) \
                    ) \
              WHERE dst_id = ?1",
        )
        .map_err(store_err)?;

    for symbol_id in symbol_ids {
        delete_symbol
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        delete_fts
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        delete_vector
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        delete_queue
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        delete_outbound
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
        flag_inbound
            .execute(rusqlite::params![symbol_id])
            .map_err(store_err)?;
    }
    Ok(())
}

fn delete_file_queue_rows(conn: &Connection, paths: &[String]) -> Result<()> {
    let mut delete = conn
        .prepare("DELETE FROM embedding_queue WHERE file_path = ?1")
        .map_err(store_err)?;
    for path in paths {
        delete.execute(rusqlite::params![path]).map_err(store_err)?;
    }
    Ok(())
}

fn delete_file_rows(conn: &Connection, paths: &[String]) -> Result<()> {
    let mut delete = conn
        .prepare("DELETE FROM file WHERE path = ?1")
        .map_err(store_err)?;
    for path in paths {
        delete.execute(rusqlite::params![path]).map_err(store_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use cognis_core::{EdgeKind, SymbolKind};
    use cognis_testgen::{
        arb_fingerprint_pair, arb_interruption_point, arb_publish_batch, FingerprintPair,
        FingerprintSpec, InterruptionMode, InterruptionPoint, PublishBatchSpec, PublishReasonSpec,
        PublishStage,
    };
    use proptest::prelude::*;
    use rusqlite::types::ValueRef;

    const P1_CHILD_ENV: &str = "COGNIS_P1_ATOMICITY_CHILD";
    const P1_CHILD_READY: &str = "COGNIS_P1_CHECKPOINT_READY";

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DurableState {
        generation: i64,
        rows: Vec<String>,
    }

    impl DurableState {
        fn read(path: &Path) -> Self {
            let db = Database::open(path).expect("reopen database after interruption");
            let conn = db.connect().expect("connect reopened database");
            let generation = conn
                .query_row(
                    "SELECT source_generation FROM generation WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .expect("read source generation");
            let rows = durable_rows(&conn);
            drop(conn);
            db.close_thread_connection();
            DurableState { generation, rows }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct VectorIdentityState {
        fingerprint: Option<(String, i64, String)>,
        vectors: Vec<(String, Vec<u8>)>,
    }

    impl VectorIdentityState {
        fn read(path: &Path) -> Self {
            let db = Database::open(path).expect("reopen vector identity database");
            let conn = db.connect().expect("connect vector identity database");
            let fingerprint = match conn.query_row(
                "SELECT fingerprint, embedding_dim, model_id FROM vector_space WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ) {
                Ok(row) => Some(row),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(error) => panic!("read vector-space fingerprint: {error}"),
            };
            let vectors = conn
                .prepare("SELECT symbol_id, embedding FROM symbol_vec ORDER BY symbol_id")
                .expect("prepare vector identity query")
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("read vector identity rows")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("decode vector identity rows");
            drop(conn);
            db.close_thread_connection();
            VectorIdentityState {
                fingerprint,
                vectors,
            }
        }
    }

    fn durable_rows(conn: &Connection) -> Vec<String> {
        // Wall-clock columns are intentionally excluded: they are not generation
        // content and an expected commit necessarily runs at a different instant.
        let tables = [
            ("generation", "SELECT id, source_generation, semantic_generation FROM generation"),
            ("publish_batch", "SELECT source_generation, file_count, reason FROM publish_batch"),
            ("publish_batch_file", "SELECT source_generation, path, change FROM publish_batch_file"),
            ("embedding_queue", "SELECT symbol_id, file_path, attempts, next_attempt_at, last_error FROM embedding_queue"),
            ("vector_space", "SELECT id, fingerprint, embedding_dim, model_id FROM vector_space"),
            ("symbol", "SELECT id, kind, name, qualified_name, language, module, file_path, line_start, line_end, signature, docstring, content_hash, body_excerpt, semantic_summary, risk_score, ambiguous, untrusted_flags FROM symbol"),
            ("edge", "SELECT src_id, dst_id, kind, confidence, meta FROM edge"),
            ("symbol_fts", "SELECT id, name, qualified_name, signature, docstring, body_excerpt FROM symbol_fts"),
            ("symbol_vec", "SELECT symbol_id, embedding FROM symbol_vec"),
            ("file", "SELECT path, language, size_bytes, content_hash, parse_status FROM file"),
        ];
        let mut rows = Vec::new();
        for (table, sql) in tables {
            let mut stmt = conn.prepare(sql).expect("prepare durable-state query");
            let column_count = stmt.column_count();
            let mapped = stmt
                .query_map([], |row| {
                    let mut values = Vec::with_capacity(column_count);
                    for column in 0..column_count {
                        values.push(encode_value(row.get_ref(column)?));
                    }
                    Ok(format!("{table}|{}", values.join("|")))
                })
                .expect("read durable-state rows");
            for row in mapped {
                rows.push(row.expect("decode durable-state row"));
            }
        }
        rows.sort();
        rows
    }

    fn encode_value(value: ValueRef<'_>) -> String {
        match value {
            ValueRef::Null => "n".to_string(),
            ValueRef::Integer(value) => format!("i{value}"),
            ValueRef::Real(value) => format!("r{:016x}", value.to_bits()),
            ValueRef::Text(value) => format!("t{}:{value:?}", value.len()),
            ValueRef::Blob(value) => {
                let mut hex = String::with_capacity(value.len() * 2);
                for byte in value {
                    use std::fmt::Write as _;
                    write!(hex, "{byte:02x}").expect("write to string");
                }
                format!("b{}:{hex}", value.len())
            }
        }
    }

    fn local_batch(spec: &PublishBatchSpec) -> PublishBatch {
        let mut batch = PublishBatch {
            symbol_upserts: spec.symbol_upserts.clone(),
            symbol_deletes: spec.symbol_deletes.clone(),
            edge_upserts: spec.edge_upserts.clone(),
            edge_deletes: spec.edge_deletes.clone(),
            vector_upserts: spec.vector_upserts.clone(),
            vector_deletes: spec.vector_deletes.clone(),
            file_rows: spec
                .file_rows
                .iter()
                .map(|row| FileCacheRow {
                    path: row.path.clone(),
                    language: row.language.clone(),
                    size_bytes: row.size_bytes,
                    content_hash: row.content_hash.clone(),
                    parsed_at: row.parsed_at,
                    parse_status: row.parse_status,
                })
                .collect(),
            file_deletes: spec.file_deletes.clone(),
            queue_enqueue: spec
                .queue_enqueue
                .iter()
                .map(|row| QueuedSymbol {
                    symbol_id: row.symbol_id.clone(),
                    file_path: row.file_path.clone(),
                })
                .collect(),
            queue_remove: spec.queue_remove.clone(),
            fingerprint: spec.fingerprint.as_ref().map(|fingerprint| {
                let converted = fingerprint.to_fingerprint();
                VectorSpaceFingerprint {
                    digest: converted.digest,
                    dim: converted.dim,
                    model_id: converted.model_id,
                }
            }),
            reason: match spec.reason {
                PublishReasonSpec::Initial => PublishReason::Initial,
                PublishReasonSpec::Incremental => PublishReason::Incremental,
                PublishReasonSpec::Reconcile => PublishReason::Reconcile,
                PublishReasonSpec::Full => PublishReason::Full,
                PublishReasonSpec::ReEmbed => PublishReason::ReEmbed,
            },
        };
        let upserted: HashSet<&str> = batch
            .symbol_upserts
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect();
        if batch
            .vector_upserts
            .iter()
            .any(|(symbol_id, _)| upserted.contains(symbol_id.as_str()))
        {
            for (_, vector) in &mut batch.vector_upserts {
                vector.resize(4, 0.0);
                vector.truncate(4);
            }
            let fingerprint = batch
                .fingerprint
                .get_or_insert_with(|| VectorSpaceFingerprint {
                    digest: "property-1-space".to_string(),
                    dim: 4,
                    model_id: "property-1-model".to_string(),
                });
            fingerprint.dim = 4;
        }
        batch
    }

    fn state_after_commit(path: &Path, batch: &PublishBatch) -> DurableState {
        fs::copy(path, path.with_extension("expected.db")).expect("copy generation g database");
        let expected_path = path.with_extension("expected.db");
        let mut expected =
            Database::open(&expected_path).expect("open expected generation database");
        expected.publish(batch).expect("generated batch commits");
        expected.close_thread_connection();
        DurableState::read(&expected_path)
    }

    fn inject_in_process(
        path: &Path,
        batch: &PublishBatch,
        point: InterruptionPoint,
        old: &DurableState,
        new: &DurableState,
    ) {
        let mut db = Database::open(path).expect("open in-process interruption database");
        let result = publish_with_test_checkpoint(&mut db, batch, |conn, ordinal| {
            if ordinal != point.ordinal() || point.stage == PublishStage::Commit {
                return Ok(());
            }
            match point.mode {
                InterruptionMode::ErrorReturn => Err(CognisError::Store(format!(
                    "injected publish failure at checkpoint {ordinal}"
                ))),
                InterruptionMode::Rollback => {
                    conn.execute_batch("ROLLBACK").map_err(store_err)?;
                    Err(CognisError::Store(format!(
                        "injected publish rollback at checkpoint {ordinal}"
                    )))
                }
                InterruptionMode::ProcessTermination => unreachable!("handled by child process"),
            }
        });
        if point.stage == PublishStage::Commit {
            result.expect("commit checkpoint fires only after a successful commit");
        } else {
            result.expect_err("pre-commit interruption must abort publication");
        }
        db.close_thread_connection();
        assert_old_or_new(path, old, new, point);
    }

    fn inject_process_termination(
        path: &Path,
        batch: &PublishBatch,
        point: InterruptionPoint,
        old: &DurableState,
        new: &DurableState,
    ) {
        let batch_path = path.with_extension("batch.json");
        fs::write(
            &batch_path,
            serde_json::to_vec(batch).expect("serialize PublishBatch for child"),
        )
        .expect("write child PublishBatch");

        let mut child = Command::new(env::current_exe().expect("resolve test executable"))
            .arg("--exact")
            .arg("publish::tests::publish_atomicity_property_1")
            .arg("--nocapture")
            .env(P1_CHILD_ENV, "1")
            .env("COGNIS_P1_DB_PATH", path)
            .env("COGNIS_P1_BATCH_PATH", &batch_path)
            .env("COGNIS_P1_STAGE", point.ordinal().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn publish child");
        let stdout = child.stdout.take().expect("capture child stdout");
        let lines = BufReader::new(stdout).lines();
        let mut reached = false;
        for line in lines {
            let line = line.expect("read child checkpoint");
            if line.contains(P1_CHILD_READY) {
                reached = true;
                break;
            }
        }
        assert!(reached, "child did not reach checkpoint {point:?}");
        child.kill().expect("terminate publish child");
        let _ = child.wait().expect("reap publish child");
        assert_old_or_new(path, old, new, point);
    }

    fn assert_old_or_new(
        path: &Path,
        old: &DurableState,
        new: &DurableState,
        point: InterruptionPoint,
    ) {
        let reopened = DurableState::read(path);
        assert!(
            reopened == *old || reopened == *new,
            "mixed durable state after {point:?}: reopened generation {}, expected {} or {}",
            reopened.generation,
            old.generation,
            new.generation
        );
    }

    fn local_fingerprint(spec: &FingerprintSpec) -> VectorSpaceFingerprint {
        VectorSpaceFingerprint {
            digest: spec.digest(),
            dim: spec.dim,
            model_id: spec.model_id.clone(),
        }
    }

    fn vector_identity_batches(pair: &FingerprintPair) -> (PublishBatch, PublishBatch) {
        // cognis-testgen depends on the production cognis-store library, while
        // this module tests cognis-store itself. Build the local crate's type
        // explicitly so Rust does not mix the two crate instances in lib tests.
        let old_fingerprint = local_fingerprint(&pair.persisted);
        let new_fingerprint = local_fingerprint(&pair.query);
        let old_values: Vec<f32> = (0..old_fingerprint.dim)
            .map(|index| index as f32 + 0.25)
            .collect();
        let new_values: Vec<f32> = (0..new_fingerprint.dim)
            .map(|index| -(index as f32) - 0.75)
            .collect();

        let mut first = PublishBatch::empty(PublishReason::Initial);
        first.symbol_upserts = vec![symbol("vector-space-symbol", "embedded", "src/embedded.rs")];
        first.vector_upserts = vec![("vector-space-symbol".into(), old_values)];
        first.fingerprint = Some(old_fingerprint);

        let mut reembed = PublishBatch::empty(PublishReason::ReEmbed);
        reembed.vector_upserts = vec![("vector-space-symbol".into(), new_values)];
        reembed.fingerprint = Some(new_fingerprint);
        (first, reembed)
    }

    fn assert_vector_identity_old_or_new(
        path: &Path,
        old: &VectorIdentityState,
        new: &VectorIdentityState,
        point: InterruptionPoint,
    ) {
        let reopened = VectorIdentityState::read(path);
        assert!(
            reopened == *old || reopened == *new,
            "fingerprint/vector mismatch after {point:?}: reopened {reopened:?}, expected old {old:?} or new {new:?}"
        );
    }

    fn run_publish_child() {
        let path = PathBuf::from(env::var_os("COGNIS_P1_DB_PATH").expect("child database path"));
        let batch_path =
            PathBuf::from(env::var_os("COGNIS_P1_BATCH_PATH").expect("child batch path"));
        let target = env::var("COGNIS_P1_STAGE")
            .expect("child stage")
            .parse::<usize>()
            .expect("numeric child stage");
        let batch: PublishBatch =
            serde_json::from_slice(&fs::read(batch_path).expect("read child PublishBatch"))
                .expect("deserialize child PublishBatch");
        let mut db = Database::open(path).expect("open child database");
        let _ = publish_with_test_checkpoint(&mut db, &batch, |_conn, ordinal| {
            if ordinal == target {
                println!("{P1_CHILD_READY}:{ordinal}");
                std::io::stdout().flush().expect("flush checkpoint marker");
                std::thread::park();
            }
            Ok(())
        });
    }

    /// Feature: cognis-one-toggle-engine, Property 1: Publish atomicity.
    /// The in-process branch runs 100 cases. Real process termination is reduced
    /// to 12 cases (spawning a test process per case is expensive), with three
    /// distinct checkpoints per case selected from the generated interruption
    /// ordinal; this directly enforces Requirement 24.4's minimum of three.
    /// **Validates: Requirements 7.1, 7.3, 7.4, 4.7, 24.3, 24.4**
    #[test]
    fn publish_atomicity_property_1() {
        if env::var_os(P1_CHILD_ENV).is_some() {
            run_publish_child();
            return;
        }

        let strategy = (arb_publish_batch(), arb_interruption_point());
        let mut fast = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(100));
        fast.run(&strategy, |(spec, mut point)| {
            if point.mode == InterruptionMode::ProcessTermination {
                point.mode = InterruptionMode::ErrorReturn;
            }
            let dir = tempfile::tempdir().expect("create fast atomicity case");
            let path = dir.path().join("publish.db");
            let db = Database::open(&path).expect("create generation g database");
            db.close_thread_connection();
            let batch = local_batch(&spec);
            let old = DurableState::read(&path);
            let new = state_after_commit(&path, &batch);
            inject_in_process(&path, &batch, point, &old, &new);
            Ok(())
        })
        .expect("100 in-process atomicity cases");

        let mut crash = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(12));
        crash
            .run(
                &(arb_publish_batch(), arb_interruption_point()),
                |(spec, generated)| {
                    for offset in 0..3 {
                        let ordinal = (generated.ordinal() + offset) % PUBLISH_CHECKPOINT_COUNT;
                        let point = InterruptionPoint {
                            stage: PublishStage::ALL[ordinal],
                            mode: InterruptionMode::ProcessTermination,
                        };
                        let dir = tempfile::tempdir().expect("create crash atomicity case");
                        let path = dir.path().join("publish.db");
                        let db = Database::open(&path).expect("create generation g database");
                        db.close_thread_connection();
                        let batch = local_batch(&spec);
                        let old = DurableState::read(&path);
                        let new = state_after_commit(&path, &batch);
                        inject_process_termination(&path, &batch, point, &old, &new);
                    }
                    Ok(())
                },
            )
            .expect("12 process-termination atomicity cases");
    }

    /// Feature: cognis-one-toggle-engine, Property 18: Vector-space identity is
    /// established and replaced atomically. The fast branch runs 100 generated
    /// fingerprint/interruption pairs. Following Property 1's publish-atomicity
    /// pattern, real process termination runs 12 cases at three checkpoints each.
    /// **Validates: Requirements 14.1, 14.6**
    #[test]
    fn vector_space_identity_is_established_and_replaced_atomically_property_18() {
        let strategy = (arb_fingerprint_pair(), arb_interruption_point());
        let mut fast = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(100));
        fast.run(&strategy, |(pair, mut point)| {
            if point.mode == InterruptionMode::ProcessTermination {
                point.mode = InterruptionMode::ErrorReturn;
            }
            let dir = tempfile::tempdir().expect("create fast vector identity case");
            let path = dir.path().join("vector-identity.db");
            let (first, reembed) = vector_identity_batches(&pair);
            let mut db = Database::open(&path).expect("create vector identity database");
            db.publish(&first)
                .expect("publish first vector and fingerprint");
            db.close_thread_connection();

            let old_identity = VectorIdentityState::read(&path);
            let producing = local_fingerprint(&pair.persisted);
            prop_assert_eq!(
                old_identity.fingerprint.as_ref(),
                Some(&(producing.digest, producing.dim as i64, producing.model_id,)),
                "first vector write did not persist its producing fingerprint"
            );
            prop_assert_eq!(old_identity.vectors.len(), 1);

            let old = DurableState::read(&path);
            let new = state_after_commit(&path, &reembed);
            let expected_path = path.with_extension("expected.db");
            let new_identity = VectorIdentityState::read(&expected_path);
            inject_in_process(&path, &reembed, point, &old, &new);
            assert_vector_identity_old_or_new(&path, &old_identity, &new_identity, point);
            Ok(())
        })
        .expect("100 in-process vector-space identity cases");

        let mut crash = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(12));
        crash
            .run(
                &(arb_fingerprint_pair(), arb_interruption_point()),
                |(pair, generated)| {
                    for offset in 0..3 {
                        let ordinal = (generated.ordinal() + offset) % PUBLISH_CHECKPOINT_COUNT;
                        let point = InterruptionPoint {
                            stage: PublishStage::ALL[ordinal],
                            mode: InterruptionMode::ProcessTermination,
                        };
                        let dir = tempfile::tempdir().expect("create crash vector identity case");
                        let path = dir.path().join("vector-identity.db");
                        let (first, reembed) = vector_identity_batches(&pair);
                        let mut db =
                            Database::open(&path).expect("create vector identity database");
                        db.publish(&first)
                            .expect("publish first vector and fingerprint");
                        db.close_thread_connection();

                        let old_identity = VectorIdentityState::read(&path);
                        let old = DurableState::read(&path);
                        let new = state_after_commit(&path, &reembed);
                        let expected_path = path.with_extension("expected.db");
                        let new_identity = VectorIdentityState::read(&expected_path);
                        inject_process_termination(&path, &reembed, point, &old, &new);
                        assert_vector_identity_old_or_new(
                            &path,
                            &old_identity,
                            &new_identity,
                            point,
                        );
                    }
                    Ok(())
                },
            )
            .expect("12 process-termination vector-space identity cases");
    }

    #[test]
    fn publish_reason_round_trips_its_db_label() {
        for reason in [
            PublishReason::Initial,
            PublishReason::Incremental,
            PublishReason::Reconcile,
            PublishReason::Full,
            PublishReason::ReEmbed,
        ] {
            let label = reason.as_db_str();
            assert_eq!(
                PublishReason::parse_db_str(label),
                Some(reason),
                "label {label:?} must round-trip"
            );
        }
        // The labels are exactly the ones migration 002 documents.
        assert_eq!(PublishReason::ReEmbed.as_db_str(), "reembed");
        assert_eq!(PublishReason::parse_db_str("Initial"), None);
        assert_eq!(PublishReason::parse_db_str(""), None);
    }

    #[test]
    fn empty_batch_is_empty_and_covers_nothing() {
        let batch = PublishBatch::empty(PublishReason::Reconcile);
        assert!(batch.is_empty());
        assert!(batch.covered_paths().is_empty());
        assert_eq!(batch.reason, PublishReason::Reconcile);
        assert!(batch.fingerprint.is_none());
    }

    #[test]
    fn covered_paths_unions_upserted_and_deleted_files_sorted_and_deduped() {
        let mut batch = PublishBatch::empty(PublishReason::Incremental);
        batch.file_rows.push(FileCacheRow {
            path: "src/b.py".into(),
            language: "python".into(),
            size_bytes: 12,
            content_hash: "aa".into(),
            parsed_at: 1_700_000_000,
            parse_status: ParseStatus::Ok,
        });
        batch.file_deletes.push("src/a.py".into());
        // A path can be both rewritten and (in another planner pass) removed;
        // `publish_batch_file` is keyed by (generation, path), so it must dedupe.
        batch.file_deletes.push("src/b.py".into());

        assert!(!batch.is_empty());
        assert_eq!(
            batch.covered_paths(),
            vec!["src/a.py".to_string(), "src/b.py".to_string()]
        );
    }

    #[test]
    fn a_delete_only_batch_is_not_empty() {
        let mut batch = PublishBatch::empty(PublishReason::Reconcile);
        batch.symbol_deletes.push("python:src/a.py:a.f@00".into());
        assert!(!batch.is_empty());
    }

    #[test]
    fn publish_applies_every_category_and_returns_committed_generations() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("publish.db")).unwrap();

        let caller = symbol("caller", "caller", "src/caller.rs");
        let doomed = symbol("doomed", "doomed", "src/doomed.rs");
        let keep = symbol("keep", "keep", "src/keep.rs");
        let first_space = fingerprint("space-one", 2);
        let mut initial = PublishBatch::empty(PublishReason::Initial);
        initial.symbol_upserts = vec![caller.clone(), doomed.clone(), keep.clone()];
        initial.edge_upserts = vec![
            edge(
                "caller",
                "doomed",
                EdgeKind::Calls,
                serde_json::json!({"note": "audit"}),
            ),
            edge("doomed", "keep", EdgeKind::Calls, serde_json::Value::Null),
            edge("caller", "keep", EdgeKind::Imports, serde_json::Value::Null),
        ];
        initial.vector_upserts = vec![
            ("caller".into(), vec![1.0, 0.0]),
            ("doomed".into(), vec![0.0, 1.0]),
            ("keep".into(), vec![0.5, 0.5]),
        ];
        initial.file_rows = vec![
            file_row("src/caller.rs", "caller-v1"),
            file_row("src/doomed.rs", "doomed-v1"),
            file_row("src/keep.rs", "keep-v1"),
        ];
        initial.fingerprint = Some(first_space);

        let first = db.publish(&initial).unwrap();
        assert_eq!((first.source_generation, first.semantic_generation), (1, 1));
        assert_eq!(first.pending_vector_count, 0);

        let renamed = symbol("renamed", "renamed", "src/new.rs");
        let mut changed_caller = caller;
        changed_caller.body_excerpt = Some("changed caller".into());
        let replacement_space = fingerprint("space-two", 2);
        let mut batch = PublishBatch::empty(PublishReason::ReEmbed);
        batch.symbol_upserts = vec![changed_caller, renamed];
        batch.edge_upserts = vec![edge(
            "caller",
            "renamed",
            EdgeKind::Calls,
            serde_json::Value::Null,
        )];
        batch.edge_deletes = vec![("caller".into(), "keep".into(), "imports".into())];
        batch.vector_upserts = vec![
            ("caller".into(), vec![0.8, 0.2]),
            ("renamed".into(), vec![0.2, 0.8]),
        ];
        batch.vector_deletes = vec!["keep".into()];
        batch.file_rows = vec![
            file_row("src/caller.rs", "caller-v2"),
            file_row("src/new.rs", "new-v1"),
        ];
        batch.file_deletes = vec!["src/doomed.rs".into()];
        batch.queue_enqueue = vec![QueuedSymbol {
            symbol_id: "keep".into(),
            file_path: "src/keep.rs".into(),
        }];
        batch.queue_remove = vec!["doomed".into()];
        batch.fingerprint = Some(replacement_space.clone());

        let receipt = db.publish(&batch).unwrap();
        assert_eq!(receipt.source_generation, 2);
        assert_eq!(receipt.semantic_generation, 1);
        assert_eq!(receipt.pending_vector_count, 1);
        assert_eq!(
            receipt.files,
            vec!["src/caller.rs", "src/doomed.rs", "src/new.rs"]
        );

        assert_eq!(
            db.list_symbols()
                .unwrap()
                .into_iter()
                .map(|symbol| symbol.id)
                .collect::<Vec<_>>(),
            vec!["caller", "keep", "renamed"]
        );
        assert!(db.fts_match_ids("doomed", 10).unwrap().is_empty());
        assert_eq!(db.fts_match_ids("renamed", 10).unwrap(), vec!["renamed"]);
        assert_eq!(db.vec_symbol_ids().unwrap(), vec!["caller", "renamed"]);

        let edges = db.list_edges().unwrap();
        assert_eq!(edges.len(), 2);
        let inbound = edges
            .iter()
            .find(|edge| edge.src_id == "caller" && edge.dst_id == "doomed")
            .unwrap();
        assert!(inbound.dst_missing());
        assert_eq!(inbound.meta["note"], "audit");
        assert!(edges
            .iter()
            .any(|edge| edge.src_id == "caller" && edge.dst_id == "renamed"));

        let conn = db.connect().unwrap();
        let queue: (String, String) = conn
            .query_row(
                "SELECT symbol_id, file_path FROM embedding_queue",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(queue, ("keep".into(), "src/keep.rs".into()));
        let persisted_space: (String, i64, String) = conn
            .query_row(
                "SELECT fingerprint, embedding_dim, model_id FROM vector_space WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            persisted_space,
            (
                replacement_space.digest,
                replacement_space.dim as i64,
                replacement_space.model_id
            )
        );
        let audit = conn
            .prepare(
                "SELECT path, change FROM publish_batch_file \
                 WHERE source_generation = 2 ORDER BY path",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            audit,
            vec![
                ("src/caller.rs".into(), "changed".into()),
                ("src/doomed.rs".into(), "removed".into()),
                ("src/new.rs".into(), "added".into()),
            ]
        );
        let batch_audit: (i64, String) = conn
            .query_row(
                "SELECT file_count, reason FROM publish_batch WHERE source_generation = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(batch_audit, (3, "reembed".into()));
        drop(conn);

        let mut completed = PublishBatch::empty(PublishReason::Incremental);
        completed.vector_upserts = vec![("keep".into(), vec![0.4, 0.6])];
        completed.queue_remove = vec!["keep".into()];
        let completed = db.publish(&completed).unwrap();
        assert_eq!(
            (completed.source_generation, completed.semantic_generation),
            (3, 3)
        );
        assert_eq!(completed.pending_vector_count, 0);
        assert!(completed.files.is_empty());

        db.close_thread_connection();
    }

    #[test]
    fn publish_error_rolls_back_rows_generation_and_audit() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("rollback.db")).unwrap();
        let mut batch = PublishBatch::empty(PublishReason::Incremental);
        batch.symbol_upserts = vec![symbol("bad", "bad", "src/bad.rs")];
        batch.vector_upserts = vec![("bad".into(), vec![1.0])];
        batch.file_rows = vec![file_row("src/bad.rs", "bad-v1")];
        batch.fingerprint = Some(fingerprint("two-dimensional", 2));

        let error = db.publish(&batch).unwrap_err().to_string();
        assert!(error.contains("has dimension 1, expected 2"), "{error}");
        assert_eq!(db.count("symbol").unwrap(), 0);
        assert_eq!(db.count("symbol_fts").unwrap(), 0);
        assert_eq!(db.count("file").unwrap(), 0);
        assert_eq!(db.count("vector_space").unwrap(), 0);
        assert_eq!(db.count("publish_batch").unwrap(), 0);
        let conn = db.connect().unwrap();
        let generation: (i64, i64) = conn
            .query_row(
                "SELECT source_generation, semantic_generation FROM generation WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, (0, 0));
        drop(conn);
        db.close_thread_connection();
    }

    fn symbol(id: &str, name: &str, file_path: &str) -> Symbol {
        Symbol {
            id: id.into(),
            kind: SymbolKind::Function,
            name: name.into(),
            qualified_name: format!("module.{name}"),
            language: "rust".into(),
            module: "module".into(),
            file_path: file_path.into(),
            line_start: 1,
            line_end: 2,
            signature: Some(format!("fn {name}()")),
            docstring: None,
            content_hash: format!("hash-{name}"),
            body_excerpt: Some(format!("body {name}")),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        }
    }

    fn edge(src_id: &str, dst_id: &str, kind: EdgeKind, meta: serde_json::Value) -> Edge {
        Edge {
            src_id: src_id.into(),
            dst_id: dst_id.into(),
            kind,
            confidence: 1.0,
            meta,
        }
    }

    fn file_row(path: &str, content_hash: &str) -> FileCacheRow {
        FileCacheRow {
            path: path.into(),
            language: "rust".into(),
            size_bytes: 42,
            content_hash: content_hash.into(),
            parsed_at: 1_700_000_000,
            parse_status: ParseStatus::Ok,
        }
    }

    fn fingerprint(digest: &str, dim: usize) -> VectorSpaceFingerprint {
        VectorSpaceFingerprint {
            digest: digest.into(),
            dim,
            model_id: format!("model-{digest}"),
        }
    }
}
