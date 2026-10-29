//! Property 5: Incremental indexing equals a clean rebuild (task 7.6).
//!
//! For any generated source tree and any sequence over the eleven
//! `Transition_Matrix` transitions, `digest(incremental) == digest(clean_rebuild)`
//! over the same final tree — including when the changes arrive as one
//! coalesced watcher burst. A missed delete (Requirement 12.6) is followed by
//! the documented full rescan so the incremental path can converge.
//!
//! Uses [`arb_source_tree`] and [`arb_transition_sequence`].
//!
//! Validates: Requirements 12.1, 10.4, 10.5, 10.6, 10.7, 12.6, 18.6, 24.1
//! Properties: P5
//!
//! Feature: cognis-one-toggle-engine, Property 5: For any generated source tree
//! and any sequence of transitions drawn from the Transition_Matrix,
//! digest(incremental) == digest(clean_rebuild).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::{database_digest, diff, Database, DatabaseDigest};
use cognis_testgen::{
    apply_sequence, arb_source_tree, arb_transition_sequence, SourceFileSpec, SourceTree,
    Transition,
};
use proptest::prelude::*;
use tempfile::TempDir;

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<Path>) -> Self {
        Self(Database::open(path).expect("open P5 database"))
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
        fs::remove_dir_all(root).expect("clear P5 repo");
    }
    fs::create_dir_all(root).expect("create P5 repo");
    tree.materialize(root).expect("materialize P5 tree");
}

fn abs_paths(repo: &Path, rels: &[String]) -> Vec<PathBuf> {
    rels.iter()
        .map(|rel| repo.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .collect()
}

fn index_full(pipe: &mut IndexerPipeline, repo: &Path) {
    pipe.index_repo(repo, true).expect("full index for P5");
}

fn index_touched(pipe: &mut IndexerPipeline, repo: &Path, rels: &[String]) {
    if rels.is_empty() {
        return;
    }
    pipe.index_batch(repo, &abs_paths(repo, rels))
        .expect("incremental batch for P5");
}

fn remember_deletes(before: &SourceTree, after: &SourceTree, deleted: &mut Vec<SourceFileSpec>) {
    for file in &before.files {
        if after.file(&file.rel_path).is_none() {
            deleted.push(file.clone());
        }
    }
}

fn apply_incremental(
    pipe: &mut IndexerPipeline,
    repo: &Path,
    initial: &SourceTree,
    transitions: &[Transition],
    coalesced: bool,
) -> SourceTree {
    sync_repo(repo, initial);
    index_full(pipe, repo);

    if coalesced {
        let (final_tree, per_step, missed) = apply_sequence(initial, transitions);
        let burst: BTreeSet<String> = per_step.into_iter().flatten().collect();
        sync_repo(repo, &final_tree);
        index_touched(pipe, repo, &burst.into_iter().collect::<Vec<_>>());
        if missed {
            index_full(pipe, repo);
        }
        return final_tree;
    }

    let mut current = initial.clone();
    let mut deleted: Vec<SourceFileSpec> = Vec::new();
    for transition in transitions {
        let before = current.clone();
        let outcome = transition.apply(&current, &deleted);
        remember_deletes(&before, &outcome.tree, &mut deleted);
        current = outcome.tree;
        sync_repo(repo, &current);
        index_touched(pipe, repo, &outcome.touched);
        if outcome.missed {
            index_full(pipe, repo);
        }
    }
    current
}

fn clean_rebuild(repo: &Path, db_path: &Path, tree: &SourceTree) -> (TestDatabase, DatabaseDigest) {
    sync_repo(repo, tree);
    let testdb = TestDatabase::open(db_path);
    let mut pipe = IndexerPipeline::new(testdb.get().clone(), Config::default());
    index_full(&mut pipe, repo);
    drop(pipe);
    let digest = database_digest(testdb.get()).expect("clean-rebuild digest");
    (testdb, digest)
}

fn format_mismatch(incremental: &Database, clean: &Database) -> String {
    let left = database_digest(incremental).expect("incremental digest");
    let right = database_digest(clean).expect("clean digest");
    let categories = left.differing_categories(&right);
    let diffs = diff(incremental, clean).expect("digest diff");
    format!(
        "digest(incremental) != digest(clean_rebuild); categories={categories:?}; diffs={diffs:?}"
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 5: For any generated source tree
    // and any sequence of transitions drawn from the Transition_Matrix,
    // digest(incremental) == digest(clean_rebuild).
    #[test]
    fn incremental_indexing_equals_a_clean_rebuild(
        tree in arb_source_tree(),
        transitions in arb_transition_sequence(),
        coalesced in any::<bool>(),
    ) {
        let workspace = TempDir::new().expect("P5 workspace");
        let repo = workspace.path().join("repo");
        let inc_db = TestDatabase::open(workspace.path().join("incremental.db"));
        let mut pipe = IndexerPipeline::new(inc_db.get().clone(), Config::default());
        let final_tree = apply_incremental(&mut pipe, &repo, &tree, &transitions, coalesced);
        drop(pipe);

        let (clean_db, clean_digest) =
            clean_rebuild(&repo, &workspace.path().join("clean.db"), &final_tree);
        let incremental_digest =
            database_digest(inc_db.get()).expect("incremental digest");

        prop_assert_eq!(
            &incremental_digest.combined,
            &clean_digest.combined,
            "{}",
            format_mismatch(inc_db.get(), clean_db.get())
        );
    }
}
