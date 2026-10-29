//! Smoke check that the shared generator crate is reachable as a
//! dev-dependency from `cognis-store` (cognis-one-toggle-engine task 2.1).
//!
//! The storage properties (P1, P2, P3, P8, P18) are written against these
//! generators in tasks 3.x and 4.x. This test exists so a broken dev-dependency
//! wiring fails here, at the wiring, rather than inside an unrelated property.

use cognis_testgen::{arb_publish_batch, arb_source_tree, PublishStage};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;

#[test]
fn shared_generators_are_reachable_from_cognis_store() {
    let mut runner = TestRunner::deterministic();

    let tree = arb_source_tree()
        .new_tree(&mut runner)
        .expect("arb_source_tree produces a value")
        .current();
    assert!(!tree.files.is_empty(), "a generated tree has files");

    let batch = arb_publish_batch()
        .new_tree(&mut runner)
        .expect("arb_publish_batch produces a value")
        .current();
    // Every generated symbol satisfies the core model invariants, so a storage
    // property failure is never merely an invalid fixture.
    for symbol in &batch.symbol_upserts {
        symbol.validate().expect("generated symbols validate");
    }

    // The generation bump sits after every row mutation and before the commit —
    // the ordering the publish-atomicity property (P1) is written against.
    assert!(PublishStage::SymbolUpserts.ordinal() < PublishStage::GenerationBump.ordinal());
    assert!(PublishStage::GenerationBump.ordinal() < PublishStage::Commit.ordinal());
}
