//! Property test for generation-addressed publish audit rows.

use std::collections::BTreeSet;

use cognis_store::{Database, IndexPublisher};
use cognis_testgen::{arb_publish_batch, PublishBatchSpec};
use proptest::prelude::*;
use rusqlite::params;

fn arb_publish_sequence() -> impl Strategy<Value = Vec<PublishBatchSpec>> {
    prop::collection::vec(arb_publish_batch(), 1..9)
}

fn source_generation(db: &Database) -> i64 {
    db.connect()
        .expect("open generation connection")
        .query_row(
            "SELECT source_generation FROM generation WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("read source generation")
}

fn audit_paths(db: &Database, generation: i64) -> Vec<String> {
    let conn = db.connect().expect("open publish audit connection");
    let mut stmt = conn
        .prepare(
            "SELECT path FROM publish_batch_file \
             WHERE source_generation = ?1 ORDER BY path",
        )
        .expect("prepare publish path query");
    stmt.query_map(params![generation], |row| row.get(0))
        .expect("query publish paths")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read publish paths")
}

fn covered_paths(spec: &PublishBatchSpec) -> Vec<String> {
    spec.file_rows
        .iter()
        .map(|row| row.path.clone())
        .chain(spec.file_deletes.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 2: Generation monotonicity.
    /// **Validates: Requirements 7.2, 7.5**
    #[test]
    fn committed_generations_strictly_increase_and_record_exact_batch_paths(
        sequence in arb_publish_sequence(),
    ) {
        let dir = tempfile::tempdir().expect("create isolated publish sequence");
        let path = dir.path().join("generation-monotonicity.db");
        let mut db = Database::open(&path).expect("open property database");
        let mut previous_generation = source_generation(&db);

        for spec in &sequence {
            let batch = spec.to_batch();
            let receipt = db.publish(&batch).expect("commit generated publish batch");
            let observed_generation = source_generation(&db);
            let expected_paths = covered_paths(spec);

            prop_assert!(
                observed_generation > previous_generation,
                "source_generation did not strictly increase: previous={}, observed={}",
                previous_generation,
                observed_generation,
            );
            prop_assert_eq!(receipt.source_generation, observed_generation);
            prop_assert_eq!(&receipt.files, &expected_paths);
            prop_assert_eq!(
                audit_paths(&db, observed_generation),
                expected_paths,
                "publish_batch_file rows differ from the generated batch coverage at generation {}",
                observed_generation,
            );

            previous_generation = observed_generation;
        }

        db.close_thread_connection();
    }
}
