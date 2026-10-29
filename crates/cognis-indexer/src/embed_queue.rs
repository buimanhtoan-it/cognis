use std::collections::BTreeSet;
use std::time::Duration;

use cognis_core::{Config, Result, Symbol};
use cognis_embed::ModelSlot;
use cognis_store::{Database, SymbolWriter};
use rusqlite::Connection;

use crate::pipeline::embedding_text;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    pub drained: usize,
    pub pending_vector_count: usize,
    pub pending_files: Vec<String>,
}

pub struct EmbeddingQueueDrainer<'a> {
    db: &'a mut Database,
    model: &'a ModelSlot,
    config: &'a Config,
    allow_demand_load: bool,
    failure_cooldown: Duration,
}

impl<'a> EmbeddingQueueDrainer<'a> {
    pub fn new(
        db: &'a mut Database,
        model: &'a ModelSlot,
        config: &'a Config,
        allow_demand_load: bool,
        failure_cooldown: Duration,
    ) -> Self {
        Self {
            db,
            model,
            config,
            allow_demand_load,
            failure_cooldown,
        }
    }

    pub fn snapshot(db: &Database) -> Result<DrainOutcome> {
        let conn = db.connect()?;
        pending_snapshot(&conn)
    }

    pub fn drain(&mut self, budget: usize) -> Result<DrainOutcome> {
        if budget == 0 {
            return Self::snapshot(self.db);
        }

        let now = unix_timestamp()?;
        let conn = self.db.connect()?;
        let due = due_rows(&conn, now, budget)?;
        if due.is_empty() {
            return pending_snapshot(&conn);
        }

        let mut drained = 0usize;
        for row in due {
            let live_symbols = self.db.hydrate_ids(std::slice::from_ref(&row.symbol_id))?;
            let Some(symbol) = live_symbols.into_iter().next() else {
                remove_stale_queue_row(&conn, &row.symbol_id, now)?;
                continue;
            };

            let vectors = match self.embed_symbol(&symbol) {
                Ok(vectors) => vectors,
                Err(message) => {
                    record_failure(&conn, self.config, &row, &message, now)?;
                    continue;
                }
            };

            persist_success(&conn, &row.symbol_id, &vectors, now)?;
            drained += 1;
        }

        let mut outcome = pending_snapshot(&conn)?;
        outcome.drained = drained;
        Ok(outcome)
    }

    fn embed_symbol(&mut self, symbol: &Symbol) -> std::result::Result<Vec<Vec<f32>>, String> {
        let borrow = self.model.try_borrow().or_else(|| {
            if !self.allow_demand_load {
                return None;
            }
            let config = self.config.clone();
            self.model
                .borrow_or_load(self.failure_cooldown, || {
                    cognis_embed::build_embedder(&config)
                })
                .ok()
        });

        let Some(borrow) = borrow else {
            return Err("embedder unavailable".to_string());
        };

        let dim = borrow.embedder().embedding_dim();
        if dim > 0 {
            self.db
                .reconcile_embedding_dim(dim)
                .map_err(|error| error.to_string())?;
        }

        let texts = vec![embedding_text(symbol)];
        match borrow.embedder().embed_batch(&texts) {
            Ok(vectors) if vectors.len() == 1 => Ok(vectors),
            Ok(vectors) => Err(format!(
                "embed batch length mismatch: expected 1, got {}",
                vectors.len()
            )),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DueQueueRow {
    symbol_id: String,
    file_path: String,
    attempts: u32,
}

fn due_rows(conn: &Connection, now: i64, budget: usize) -> Result<Vec<DueQueueRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT symbol_id, file_path, attempts \
             FROM embedding_queue \
             WHERE next_attempt_at <= ?1 \
             ORDER BY next_attempt_at, enqueued_at, symbol_id \
             LIMIT ?2",
        )
        .map_err(store_error)?;
    let rows = stmt
        .query_map(rusqlite::params![now, budget as i64], |row| {
            Ok(DueQueueRow {
                symbol_id: row.get(0)?,
                file_path: row.get(1)?,
                attempts: row.get::<_, i64>(2)? as u32,
            })
        })
        .map_err(store_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(store_error)
}

fn pending_snapshot(conn: &Connection) -> Result<DrainOutcome> {
    let pending_vector_count = pending_vector_count(conn)?;
    let pending_files = pending_files(conn)?;
    Ok(DrainOutcome {
        drained: 0,
        pending_vector_count,
        pending_files,
    })
}

fn pending_vector_count(conn: &Connection) -> Result<usize> {
    conn.query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| count.max(0) as usize)
    .map_err(store_error)
}

fn pending_files(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT file_path \
             FROM embedding_queue \
             GROUP BY file_path \
             ORDER BY MIN(next_attempt_at), MIN(enqueued_at), file_path \
             LIMIT 8",
        )
        .map_err(store_error)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(store_error)?;
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for row in rows {
        let path = row.map_err(store_error)?;
        if seen.insert(path.clone()) {
            files.push(path);
        }
    }
    Ok(files)
}

fn delete_queue_row(conn: &Connection, symbol_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM embedding_queue WHERE symbol_id = ?1",
        rusqlite::params![symbol_id],
    )
    .map_err(store_error)?;
    Ok(())
}

fn with_write_txn(conn: &Connection, body: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(store_error)?;
    match body(conn) {
        Ok(()) => conn.execute_batch("COMMIT").map_err(store_error),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn advance_semantic_if_queue_empty(conn: &Connection, now: i64) -> Result<()> {
    let pending = pending_vector_count(conn)?;
    if pending == 0 {
        conn.execute(
            "UPDATE generation \
                SET semantic_generation = source_generation, \
                    updated_at = ?1 \
              WHERE id = 1",
            rusqlite::params![now],
        )
        .map_err(store_error)?;
    }
    Ok(())
}

fn remove_stale_queue_row(conn: &Connection, symbol_id: &str, now: i64) -> Result<()> {
    with_write_txn(conn, |conn| {
        delete_queue_row(conn, symbol_id)?;
        advance_semantic_if_queue_empty(conn, now)
    })
}

fn floats_to_le_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn upsert_symbol_vectors(conn: &Connection, symbol_id: &str, vectors: &[Vec<f32>]) -> Result<()> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbol WHERE id = ?1",
            rusqlite::params![symbol_id],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    if exists == 0 {
        return Ok(());
    }
    let mut delete = conn
        .prepare("DELETE FROM symbol_vec WHERE symbol_id = ?1")
        .map_err(store_error)?;
    let mut insert = conn
        .prepare("INSERT INTO symbol_vec(symbol_id, embedding) VALUES (?1, ?2)")
        .map_err(store_error)?;
    for vector in vectors {
        delete
            .execute(rusqlite::params![symbol_id])
            .map_err(store_error)?;
        insert
            .execute(rusqlite::params![symbol_id, floats_to_le_bytes(vector)])
            .map_err(store_error)?;
    }
    Ok(())
}

fn persist_success(
    conn: &Connection,
    symbol_id: &str,
    vectors: &[Vec<f32>],
    now: i64,
) -> Result<()> {
    with_write_txn(conn, |conn| {
        upsert_symbol_vectors(conn, symbol_id, vectors)?;
        delete_queue_row(conn, symbol_id)?;
        advance_semantic_if_queue_empty(conn, now)
    })
}

fn record_failure(
    conn: &Connection,
    config: &Config,
    row: &DueQueueRow,
    message: &str,
    now: i64,
) -> Result<()> {
    let attempts = row.attempts.saturating_add(1);
    let backoff_ms = config
        .owner
        .backoff_ms_for_attempt(attempts.saturating_sub(1));
    let delay_s = backoff_ms.div_ceil(1_000) as i64;
    let next_attempt_at = now.saturating_add(delay_s);
    conn.execute(
        "UPDATE embedding_queue \
            SET attempts = ?2, \
                last_error = ?3, \
                next_attempt_at = ?4 \
          WHERE symbol_id = ?1",
        rusqlite::params![row.symbol_id, attempts as i64, message, next_attempt_at],
    )
    .map_err(store_error)?;
    Ok(())
}

fn unix_timestamp() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(store_error)
}

fn store_error<E: std::fmt::Display>(error: E) -> cognis_core::CognisError {
    cognis_core::CognisError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use cognis_core::{ParseStatus, SymbolKind};
    use cognis_embed::Embedder;
    use cognis_store::{FileCacheRow, IndexPublisher, PublishBatch, PublishReason, QueuedSymbol};

    #[derive(Debug)]
    struct BagOfLetters;

    impl Embedder for BagOfLetters {
        fn embedding_dim(&self) -> usize {
            26
        }

        fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
            Ok(bag_of_letters(text))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| bag_of_letters(text)).collect())
        }
    }

    #[derive(Debug)]
    struct AlwaysFails;

    impl Embedder for AlwaysFails {
        fn embedding_dim(&self) -> usize {
            26
        }

        fn embed_text(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.0; 26])
        }

        fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn drain_reads_due_rows_by_next_attempt_at_and_persists_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue.db")).unwrap();
        seed_queue_fixture(
            &mut db,
            &[
                queued_symbol("due-a", "src/a.rs"),
                queued_symbol("due-b", "src/b.rs"),
            ],
        );

        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE embedding_queue SET next_attempt_at = 0 WHERE symbol_id IN ('due-a', 'due-b')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO embedding_queue(symbol_id, file_path, enqueued_at, attempts, next_attempt_at, last_error) \
             VALUES('later', 'src/later.rs', 1, 0, 9999999999, NULL)",
            [],
        )
        .unwrap();

        let model = ModelSlot::ready(Box::new(BagOfLetters));
        let config = Config::default();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let outcome = drainer.drain(2).unwrap();

        assert_eq!(outcome.drained, 2);
        assert_eq!(outcome.pending_vector_count, 1);
        assert_eq!(outcome.pending_files, vec!["src/later.rs".to_string()]);
        assert_eq!(db.vec_row_count().unwrap(), 2);
        assert_eq!(
            db.vec_symbol_ids().unwrap(),
            vec!["due-a".to_string(), "due-b".to_string()]
        );
    }

    #[test]
    fn drain_failure_retains_row_and_applies_bounded_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue-fail.db")).unwrap();
        seed_queue_fixture(&mut db, &[queued_symbol("retry-me", "src/retry.rs")]);
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE embedding_queue SET attempts = 1, next_attempt_at = 0 WHERE symbol_id = 'retry-me'",
            [],
        )
        .unwrap();

        let model = ModelSlot::ready(Box::new(AlwaysFails));
        let config = Config::from_yaml_str("owner:\n  embed_backoff_ms: [1000, 2000]\n").unwrap();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let before = unix_timestamp().unwrap();
        let outcome = drainer.drain(1).unwrap();
        let after = unix_timestamp().unwrap();

        assert_eq!(outcome.drained, 0);
        assert_eq!(outcome.pending_vector_count, 1);
        let row = conn
            .query_row(
                "SELECT attempts, next_attempt_at, last_error FROM embedding_queue WHERE symbol_id = 'retry-me'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, 2);
        assert!(
            row.2.contains("length mismatch"),
            "unexpected last_error: {}",
            row.2
        );
        let min_next = before + 2;
        let max_next = after + 2;
        assert!(
            row.1 >= min_next && row.1 <= max_next,
            "next_attempt_at {} should be within [{min_next}, {max_next}]",
            row.1
        );
        assert_eq!(db.vec_row_count().unwrap(), 0);
    }

    fn generations(db: &Database) -> (i64, i64) {
        let conn = db.connect().unwrap();
        conn.query_row(
            "SELECT source_generation, semantic_generation FROM generation WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    fn queue_count(db: &Database) -> i64 {
        let conn = db.connect().unwrap();
        conn.query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn draining_last_row_advances_semantic_generation_in_the_same_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue-last.db")).unwrap();
        seed_queue_fixture(&mut db, &[queued_symbol("only", "src/only.rs")]);
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE embedding_queue SET next_attempt_at = 0 WHERE symbol_id = 'only'",
            [],
        )
        .unwrap();
        let (source_before, semantic_before) = generations(&db);
        assert!(source_before > semantic_before);

        let model = ModelSlot::ready(Box::new(BagOfLetters));
        let config = Config::default();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let outcome = drainer.drain(1).unwrap();

        assert_eq!(outcome.drained, 1);
        assert_eq!(outcome.pending_vector_count, 0);
        assert_eq!(queue_count(&db), 0);
        assert_eq!(db.vec_symbol_ids().unwrap(), vec!["only".to_string()]);
        let (source_after, semantic_after) = generations(&db);
        assert_eq!(source_after, source_before);
        assert_eq!(semantic_after, source_after);
    }

    #[test]
    fn draining_one_of_many_does_not_advance_semantic_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue-partial.db")).unwrap();
        seed_queue_fixture(
            &mut db,
            &[
                queued_symbol("first", "src/first.rs"),
                queued_symbol("second", "src/second.rs"),
            ],
        );
        let conn = db.connect().unwrap();
        conn.execute("UPDATE embedding_queue SET next_attempt_at = 0", [])
            .unwrap();
        let (source_before, semantic_before) = generations(&db);

        let model = ModelSlot::ready(Box::new(BagOfLetters));
        let config = Config::default();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let outcome = drainer.drain(1).unwrap();

        assert_eq!(outcome.drained, 1);
        assert_eq!(outcome.pending_vector_count, 1);
        assert_eq!(queue_count(&db), 1);
        let (source_after, semantic_after) = generations(&db);
        assert_eq!(source_after, source_before);
        assert_eq!(semantic_after, semantic_before);
    }

    #[test]
    fn persist_success_rolls_back_vector_and_queue_together() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue-atomic.db")).unwrap();
        seed_queue_fixture(&mut db, &[queued_symbol("boom", "src/boom.rs")]);
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE embedding_queue SET next_attempt_at = 0 WHERE symbol_id = 'boom'",
            [],
        )
        .unwrap();
        db.reconcile_embedding_dim(26).unwrap();
        conn.execute(
            "CREATE TRIGGER abort_vec AFTER INSERT ON symbol_vec BEGIN \
                 SELECT RAISE(ABORT, 'injected vector abort'); \
             END",
            [],
        )
        .unwrap();
        let (source_before, semantic_before) = generations(&db);

        let model = ModelSlot::ready(Box::new(BagOfLetters));
        let config = Config::default();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let error = drainer.drain(1).unwrap_err();
        assert!(
            error.to_string().contains("injected vector abort"),
            "unexpected error: {error}"
        );

        assert_eq!(queue_count(&db), 1);
        assert_eq!(db.vec_row_count().unwrap(), 0);
        let (source_after, semantic_after) = generations(&db);
        assert_eq!(
            (source_after, semantic_after),
            (source_before, semantic_before)
        );
    }

    #[test]
    fn stale_queue_row_removal_advances_semantic_generation_when_queue_empties() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Database::open(dir.path().join("queue-stale.db")).unwrap();
        seed_queue_fixture(&mut db, &[queued_symbol("gone", "src/gone.rs")]);
        let conn = db.connect().unwrap();
        conn.execute("DELETE FROM symbol WHERE id = 'gone'", [])
            .unwrap();
        conn.execute(
            "UPDATE embedding_queue SET next_attempt_at = 0 WHERE symbol_id = 'gone'",
            [],
        )
        .unwrap();

        let model = ModelSlot::ready(Box::new(BagOfLetters));
        let config = Config::default();
        let mut drainer =
            EmbeddingQueueDrainer::new(&mut db, &model, &config, false, Duration::ZERO);
        let outcome = drainer.drain(1).unwrap();

        assert_eq!(outcome.drained, 0);
        assert_eq!(outcome.pending_vector_count, 0);
        assert_eq!(queue_count(&db), 0);
        let (source_after, semantic_after) = generations(&db);
        assert_eq!(semantic_after, source_after);
    }

    fn seed_queue_fixture(db: &mut Database, queued: &[QueuedSymbol]) {
        let mut batch = PublishBatch::empty(PublishReason::Initial);
        batch.symbol_upserts = queued
            .iter()
            .map(|queued| symbol(&queued.symbol_id, &queued.file_path))
            .collect();
        batch.file_rows = queued
            .iter()
            .map(|queued| file_row(&queued.file_path))
            .collect();
        batch.queue_enqueue = queued.to_vec();
        db.publish(&batch).unwrap();
    }

    fn queued_symbol(symbol_id: &str, file_path: &str) -> QueuedSymbol {
        QueuedSymbol {
            symbol_id: symbol_id.to_string(),
            file_path: file_path.to_string(),
        }
    }

    fn symbol(id: &str, file_path: &str) -> Symbol {
        Symbol {
            id: id.into(),
            kind: SymbolKind::Function,
            name: id.into(),
            qualified_name: format!("module.{id}"),
            language: "rust".into(),
            module: "module".into(),
            file_path: file_path.into(),
            line_start: 1,
            line_end: 2,
            signature: Some(format!("fn {id}()")),
            docstring: None,
            content_hash: format!("hash-{id}"),
            body_excerpt: Some(format!("body {id}")),
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        }
    }

    fn file_row(path: &str) -> FileCacheRow {
        FileCacheRow {
            path: path.into(),
            language: "rust".into(),
            size_bytes: 42,
            content_hash: format!("hash-{path}"),
            parsed_at: 1_700_000_000,
            parse_status: ParseStatus::Ok,
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
}
