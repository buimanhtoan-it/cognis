//! Property 3: semantic generation never claims completeness without vectors.

use cognis_store::{Database, IndexPublisher};
use cognis_testgen::arb_publish_batch;
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
struct SemanticState {
    source_generation: i64,
    semantic_generation: i64,
    pending_vector_count: i64,
    missing_vector_count: i64,
}

fn semantic_state(db: &Database) -> SemanticState {
    let conn = db.connect().expect("open property database connection");
    conn.query_row(
        "SELECT g.source_generation, g.semantic_generation, \
                (SELECT COUNT(*) FROM embedding_queue), \
                (SELECT COUNT(*) FROM symbol AS s \
                   LEFT JOIN symbol_vec AS v ON v.symbol_id = s.id \
                  WHERE v.symbol_id IS NULL) \
           FROM generation AS g WHERE g.id = 1",
        [],
        |row| {
            Ok(SemanticState {
                source_generation: row.get(0)?,
                semantic_generation: row.get(1)?,
                pending_vector_count: row.get(2)?,
                missing_vector_count: row.get(3)?,
            })
        },
    )
    .expect("read semantic generation state")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 3: For any reachable database state,
    // semantic_generation == source_generation implies every symbol has a persisted vector,
    // and semantic_generation advances on commit iff the Embedding_Queue is empty.
    /// **Validates: Requirements 7.6, 7.7, 11.5**
    #[test]
    fn semantic_generation_is_sound_for_committed_batches(
        batches in prop::collection::vec(arb_publish_batch(), 1..7),
    ) {
        let dir = tempfile::tempdir().expect("create isolated property case");
        let mut db = Database::open(dir.path().join("semantic-generation.db"))
            .expect("open property database");
        let mut committed_batches = 0usize;
        for spec in batches {
            let mut batch = spec.to_batch();

            // Keep generated batches within Property 3's reachable-state
            // precondition: each newly persisted symbol either gets its vector
            // in this commit or is durably recorded as required work.
            let vector_ids = batch
                .vector_upserts
                .iter()
                .map(|(symbol_id, _)| symbol_id.as_str())
                .collect::<std::collections::HashSet<_>>();
            batch.queue_enqueue.extend(
                batch
                    .symbol_upserts
                    .iter()
                    .filter(|symbol| !vector_ids.contains(symbol.id.as_str()))
                    .map(|symbol| cognis_store::QueuedSymbol {
                        symbol_id: symbol.id.clone(),
                        file_path: symbol.file_path.clone(),
                    }),
            );
            batch.queue_remove.retain(|symbol_id| {
                !batch
                    .queue_enqueue
                    .iter()
                    .any(|queued| queued.symbol_id == *symbol_id)
            });

            let before = semantic_state(&db);
            let receipt = match db.publish(&batch) {
                Ok(receipt) => receipt,
                // The broad shared generator deliberately includes invalid
                // cross-category combinations; failed publishes are not
                // reachable committed states and are outside this property.
                Err(_) => continue,
            };
            committed_batches += 1;
            let after = semantic_state(&db);

            prop_assert_eq!(receipt.source_generation, after.source_generation);
            prop_assert_eq!(receipt.semantic_generation, after.semantic_generation);
            prop_assert_eq!(receipt.pending_vector_count, after.pending_vector_count);

            let queue_empty = after.pending_vector_count == 0;
            let semantic_advanced = after.semantic_generation != before.semantic_generation;
            prop_assert_eq!(
                semantic_advanced,
                queue_empty,
                "semantic generation must advance on commit iff the queue is empty",
            );
            if after.semantic_generation == after.source_generation {
                prop_assert_eq!(
                    after.missing_vector_count,
                    0,
                    "a current semantic generation must cover every persisted symbol",
                );
            }
        }

        prop_assert!(committed_batches > 0, "every case must exercise a committed batch");
        db.close_thread_connection();
    }
}
