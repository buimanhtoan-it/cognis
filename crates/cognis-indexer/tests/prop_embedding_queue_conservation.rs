//! Property 4: Embedding queue conservation (task 8.3).
//!
//! Over any sequence of publishes, embeddings, deletions, and embedding
//! failures, `pending_vector_count` equals both the `embedding_queue` row count
//! and the number of live symbols that lack a persisted vector. A failure does
//! not drop a queue row; a symbol delete does not leave a queue row behind.
//!
//! Validates: Requirements 11.1, 11.3, 11.4, 11.6, 11.7, 15.4
//! Properties: P4

use std::collections::BTreeSet;
use std::time::Duration;

use cognis_core::{Config, ParseStatus, Result, Symbol, SymbolKind};
use cognis_embed::{Embedder, ModelSlot};
use cognis_indexer::EmbeddingQueueDrainer;
use cognis_store::{
    Database, FileCacheRow, GenerationSnapshot, IndexPublisher, PublishBatch, PublishReason,
    QueuedSymbol,
};
use proptest::prelude::*;
use tempfile::TempDir;

#[derive(Debug, Clone, Copy)]
enum QueueOp {
    Publish(u8),
    DrainOk(u8),
    DrainFail(u8),
    Delete(u8),
}

fn arb_op() -> impl Strategy<Value = QueueOp> {
    prop_oneof![
        3 => (1u8..4).prop_map(QueueOp::Publish),
        2 => (1u8..4).prop_map(QueueOp::DrainOk),
        2 => (1u8..4).prop_map(QueueOp::DrainFail),
        2 => (0u8..8).prop_map(QueueOp::Delete),
    ]
}

fn arb_ops() -> impl Strategy<Value = Vec<QueueOp>> {
    prop::collection::vec(arb_op(), 1..10)
}

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

fn make_symbol(id: &str, file_path: &str) -> Symbol {
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

fn live_symbol_ids(db: &Database) -> BTreeSet<String> {
    let conn = db.connect().expect("connect symbols");
    let mut stmt = conn.prepare("SELECT id FROM symbol").expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<rusqlite::Result<BTreeSet<String>>>()
        .expect("ids")
}

fn queue_ids(db: &Database) -> BTreeSet<String> {
    let conn = db.connect().expect("connect queue");
    let mut stmt = conn
        .prepare("SELECT symbol_id FROM embedding_queue")
        .expect("prepare queue");
    stmt.query_map([], |row| row.get(0))
        .expect("query queue")
        .collect::<rusqlite::Result<BTreeSet<String>>>()
        .expect("queue ids")
}

fn queue_count(db: &Database) -> usize {
    db.connect()
        .expect("connect count")
        .query_row("SELECT COUNT(*) FROM embedding_queue", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count queue")
        .max(0) as usize
}

fn lacking_vector_count(db: &Database) -> usize {
    let symbols = live_symbol_ids(db);
    let vectors: BTreeSet<String> = db
        .vec_symbol_ids()
        .expect("vector ids")
        .into_iter()
        .collect();
    symbols.difference(&vectors).count()
}

fn mark_queue_due(db: &Database) {
    db.connect()
        .expect("connect due")
        .execute("UPDATE embedding_queue SET next_attempt_at = 0", [])
        .expect("mark due");
}

fn assert_conserved(db: &Database) -> std::result::Result<(), String> {
    let reported = EmbeddingQueueDrainer::snapshot(db)
        .map_err(|e| e.to_string())?
        .pending_vector_count;
    let queued = queue_count(db);
    let lacking = lacking_vector_count(db);
    let freshness = GenerationSnapshot::open(db)
        .map_err(|e| e.to_string())?
        .pending_vector_count() as usize;
    let live = live_symbol_ids(db);
    let qids = queue_ids(db);

    if reported != queued {
        return Err(format!(
            "pending_vector_count {reported} != queue row count {queued}"
        ));
    }
    if reported != lacking {
        return Err(format!(
            "pending_vector_count {reported} != symbols lacking vectors {lacking}"
        ));
    }
    if freshness != reported {
        return Err(format!(
            "Freshness_Metadata pending_vector_count {freshness} != {reported}"
        ));
    }
    if !qids.is_subset(&live) {
        return Err(format!(
            "queue rows survived their symbols: {:?}",
            qids.difference(&live).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

fn publish(db: &mut Database, batch: &PublishBatch) {
    db.publish(batch).expect("publish");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property 4: queue count, pending_vector_count, and vectorless symbols
    /// stay equal across publishes, successful drains, failed drains, and
    /// deletes.
    #[test]
    fn embedding_queue_conservation(ops in arb_ops()) {
        let dir = TempDir::new().expect("P4 workspace");
        let mut db = Database::open(dir.path().join("queue.db")).expect("open db");
        let config = Config::from_yaml_str("owner:\n  embed_backoff_ms: [1000, 2000]\n")
            .expect("backoff config");
        let ok_model = ModelSlot::ready(Box::new(BagOfLetters));
        let fail_model = ModelSlot::ready(Box::new(AlwaysFails));
        let mut next_id = 0usize;
        let mut live: Vec<String> = Vec::new();

        prop_assert!(assert_conserved(&db).is_ok());

        for op in &ops {
            match *op {
                QueueOp::Publish(n) => {
                    let mut batch = PublishBatch::empty(PublishReason::Incremental);
                    for _ in 0..n {
                        let id = format!("sym-{next_id}");
                        let path = format!("src/f{next_id}.rs");
                        next_id += 1;
                        batch.symbol_upserts.push(make_symbol(&id, &path));
                        batch.file_rows.push(file_row(&path));
                        batch.queue_enqueue.push(QueuedSymbol {
                            symbol_id: id.clone(),
                            file_path: path,
                        });
                        live.push(id);
                    }
                    publish(&mut db, &batch);
                }
                QueueOp::DrainOk(budget) => {
                    mark_queue_due(&db);
                    let mut drainer = EmbeddingQueueDrainer::new(
                        &mut db,
                        &ok_model,
                        &config,
                        false,
                        Duration::ZERO,
                    );
                    drainer.drain(budget as usize).expect("drain ok");
                }
                QueueOp::DrainFail(budget) => {
                    let before = queue_ids(&db);
                    mark_queue_due(&db);
                    let mut drainer = EmbeddingQueueDrainer::new(
                        &mut db,
                        &fail_model,
                        &config,
                        false,
                        Duration::ZERO,
                    );
                    drainer.drain(budget as usize).expect("drain fail");
                    let after = queue_ids(&db);
                    prop_assert!(
                        before.is_subset(&after),
                        "a failed embed must not drop queue rows: before={before:?} after={after:?}"
                    );
                }
                QueueOp::Delete(ordinal) => {
                    if live.is_empty() {
                        continue;
                    }
                    let idx = (ordinal as usize) % live.len();
                    let id = live.remove(idx);
                    let mut batch = PublishBatch::empty(PublishReason::Incremental);
                    batch.symbol_deletes.push(id.clone());
                    batch.vector_deletes.push(id.clone());
                    batch.queue_remove.push(id.clone());
                    publish(&mut db, &batch);
                    prop_assert!(
                        !queue_ids(&db).contains(&id),
                        "queue row for deleted symbol {id} must not survive"
                    );
                }
            }
            if let Err(message) = assert_conserved(&db) {
                prop_assert!(false, "{message} after {op:?}");
            }
        }
        db.close_thread_connection();
    }
}
