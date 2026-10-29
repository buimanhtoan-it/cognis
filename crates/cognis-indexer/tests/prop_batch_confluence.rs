//! Property 7: Batch confluence (task 7.8).
//!
//! For any set of file changes, applying them as one `Publish_Batch` yields
//! the same `Database_Digest` as applying the same changes as any permutation
//! of singleton batches (Requirement 10.8).
//!
//! Uses [`arb_source_tree`] and [`arb_transition_sequence`].
//!
//! Validates: Requirements 10.8
//! Properties: P7
//!
//! Feature: cognis-one-toggle-engine, Property 7: Applying a change set as one
//! batch yields the same digest as applying it as any permutation of smaller
//! batches.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::{database_digest, diff, Database};
use cognis_testgen::{apply_sequence, arb_source_tree, arb_transition_sequence, SourceTree};
use proptest::prelude::*;
use tempfile::TempDir;

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<Path>) -> Self {
        Self(Database::open(path).expect("open P7 database"))
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
        fs::remove_dir_all(root).expect("clear P7 repo");
    }
    fs::create_dir_all(root).expect("create P7 repo");
    tree.materialize(root).expect("materialize P7 tree");
}

fn abs_paths(repo: &Path, rels: &[String]) -> Vec<PathBuf> {
    rels.iter()
        .map(|rel| repo.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .collect()
}

fn shuffle_with(mut items: Vec<String>, mut seed: u64) -> Vec<String> {
    if items.len() < 2 {
        return items;
    }
    seed |= 1;
    for i in (1..items.len()).rev() {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let j = (seed as usize) % (i + 1);
        items.swap(i, j);
    }
    items
}

fn full_index(db: &Database, repo: &Path) -> IndexerPipeline {
    let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
    pipe.index_repo(repo, true).expect("P7 full index");
    pipe
}

fn format_mismatch(one: &Database, many: &Database) -> String {
    let left = database_digest(one).expect("one-batch digest");
    let right = database_digest(many).expect("split-batch digest");
    let categories = left.differing_categories(&right);
    let diffs = diff(one, many).expect("digest diff");
    format!(
        "one-batch digest != permuted singleton batches; categories={categories:?}; diffs={diffs:?}"
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 7: Applying a change set as one
    // batch yields the same digest as applying it as any permutation of smaller
    // batches.
    #[test]
    fn one_batch_equals_any_permutation_of_smaller_batches(
        tree in arb_source_tree(),
        transitions in arb_transition_sequence(),
        shuffle_seed in any::<u64>(),
    ) {
        let (final_tree, per_step, _) = apply_sequence(&tree, &transitions);
        let change_set: BTreeSet<String> = per_step.into_iter().flatten().collect();
        let all_paths: Vec<String> = change_set.iter().cloned().collect();
        let shuffled = shuffle_with(all_paths.clone(), shuffle_seed);

        let workspace = TempDir::new().expect("P7 workspace");
        let repo = workspace.path().join("repo");
        sync_repo(&repo, &tree);

        let one_db = TestDatabase::open(workspace.path().join("one.db"));
        let many_db = TestDatabase::open(workspace.path().join("many.db"));
        let mut one_pipe = full_index(one_db.get(), &repo);
        let mut many_pipe = full_index(many_db.get(), &repo);

        sync_repo(&repo, &final_tree);
        if !all_paths.is_empty() {
            one_pipe
                .index_batch(&repo, &abs_paths(&repo, &all_paths))
                .expect("one coalesced batch");
            for path in &shuffled {
                many_pipe
                    .index_batch(&repo, &abs_paths(&repo, std::slice::from_ref(path)))
                    .expect("singleton batch");
            }
        }
        drop(one_pipe);
        drop(many_pipe);

        let one_digest = database_digest(one_db.get()).expect("one-batch digest");
        let many_digest = database_digest(many_db.get()).expect("split-batch digest");
        prop_assert_eq!(
            &one_digest.combined,
            &many_digest.combined,
            "{}",
            format_mismatch(one_db.get(), many_db.get())
        );
    }
}
