//! Property 6: Index idempotence (task 7.7).
//!
//! For any generated source tree — including after a `Transition_Matrix`
//! sequence — indexing it twice with no intervening change leaves the
//! `Database_Digest` unchanged (Requirement 12.5). The second pass is either a
//! full reparse or a cache-driven reconcile.
//!
//! Uses [`arb_source_tree`] and [`arb_transition_sequence`].
//!
//! Validates: Requirements 12.5
//! Properties: P6
//!
//! Feature: cognis-one-toggle-engine, Property 6: Indexing the same tree twice
//! with no intervening change leaves the digest unchanged.

use std::fs;
use std::path::Path;

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::{database_digest, Database};
use cognis_testgen::{apply_sequence, arb_source_tree, arb_transition_sequence, SourceTree};
use proptest::prelude::*;
use tempfile::TempDir;

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<Path>) -> Self {
        Self(Database::open(path).expect("open P6 database"))
    }

    fn get(&self) -> &Database {
        &self.0
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        self.0.close_thread_connection();
    }
}

fn sync_repo(root: &Path, tree: &SourceTree) {
    if root.exists() {
        fs::remove_dir_all(root).expect("clear P6 repo");
    }
    fs::create_dir_all(root).expect("create P6 repo");
    tree.materialize(root).expect("materialize P6 tree");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 6: Indexing the same tree twice
    // with no intervening change leaves the digest unchanged.
    #[test]
    fn indexing_twice_with_no_change_leaves_the_digest_unchanged(
        tree in arb_source_tree(),
        transitions in arb_transition_sequence(),
        second_full in any::<bool>(),
    ) {
        let (final_tree, _, _) = apply_sequence(&tree, &transitions);
        let workspace = TempDir::new().expect("P6 workspace");
        let repo = workspace.path().join("repo");
        sync_repo(&repo, &final_tree);

        let testdb = TestDatabase::open(workspace.path().join("index.db"));
        let mut pipe = IndexerPipeline::new(testdb.get().clone(), Config::default());
        pipe.index_repo(&repo, true).expect("first index");
        let first = database_digest(testdb.get()).expect("digest after first index");

        pipe.index_repo(&repo, second_full)
            .expect("second index with no source change");
        drop(pipe);
        let second = database_digest(testdb.get()).expect("digest after second index");

        prop_assert_eq!(
            &first.combined,
            &second.combined,
            "second index mutated the digest; categories={:?}",
            first.differing_categories(&second)
        );
    }
}
