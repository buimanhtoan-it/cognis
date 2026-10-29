//! Smoke check that the shared generator crate is reachable as a
//! dev-dependency from `cognis-indexer` (cognis-one-toggle-engine task 2.1).
//!
//! The planner properties (P5, P6, P7, P10, P11) are written against these
//! generators in task 7.x. This test exists so a broken dev-dependency wiring
//! fails here, at the wiring, rather than inside an unrelated property.

use cognis_testgen::{arb_file_cache_state, arb_transition_sequence_covering_all, Transition};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use std::collections::BTreeSet;

#[test]
fn shared_generators_are_reachable_from_cognis_indexer() {
    let mut runner = TestRunner::deterministic();

    // A covering sequence exercises all eleven Transition_Matrix entries, which
    // is the shape Requirement 24.1 ("FOR every transition") asks for.
    let transitions = arb_transition_sequence_covering_all()
        .new_tree(&mut runner)
        .expect("arb_transition_sequence_covering_all produces a value")
        .current();
    let labels: BTreeSet<&str> = transitions.iter().map(Transition::label).collect();
    assert_eq!(labels.len(), Transition::LABELS.len());

    // The cache generator partitions paths into re-index and skip sets, which
    // is what the planner's three-part skip rule is checked against.
    let state = arb_file_cache_state()
        .new_tree(&mut runner)
        .expect("arb_file_cache_state produces a value")
        .current();
    let batch: BTreeSet<String> = state.expected_batch_paths().into_iter().collect();
    let skipped: BTreeSet<String> = state.expected_skipped_paths().into_iter().collect();
    assert!(batch.is_disjoint(&skipped));
}

#[test]
fn generated_trees_materialize_on_disk() {
    // The non-UTF-8 and unicode-path cases have to survive a real filesystem
    // round-trip, not just exist as in-memory descriptions.
    let mut runner = TestRunner::deterministic();
    let tree = cognis_testgen::arb_source_tree()
        .new_tree(&mut runner)
        .expect("arb_source_tree produces a value")
        .current();

    let root = std::env::temp_dir().join(format!("cognis-testgen-wiring-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root is creatable");

    tree.materialize(&root).expect("tree materializes");
    for (rel, bytes) in tree.render() {
        let on_disk = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let read_back = std::fs::read(&on_disk).expect("generated file exists on disk");
        assert_eq!(
            read_back,
            bytes,
            "byte-exact round trip for {}",
            on_disk.display()
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
