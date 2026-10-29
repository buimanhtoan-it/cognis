//! Smoke check that the shared generator crate is reachable as a
//! dev-dependency from `cognis-mcp` (cognis-one-toggle-engine task 2.1).
//!
//! The query-router properties (P17 vector-space safety, P24 deadline
//! termination and admission) are written against these generators in task
//! 10.x. This test exists so a broken dev-dependency wiring fails here, at the
//! wiring, rather than inside an unrelated property.

use cognis_core::MCP_TOOLS;
use cognis_testgen::{arb_arrival_sequence, arb_fingerprint_pair};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;

#[test]
fn shared_generators_are_reachable_from_cognis_mcp() {
    let mut runner = TestRunner::deterministic();

    // The guard must withhold whenever the pair differs, including the
    // equal-dimension-different-model trap of Requirement 14.4.
    let pair = arb_fingerprint_pair()
        .new_tree(&mut runner)
        .expect("arb_fingerprint_pair produces a value")
        .current();
    assert_eq!(
        pair.must_withhold(),
        pair.persisted.digest() != pair.query.digest(),
        "withholding must track digest inequality",
    );

    // Arrivals name real contract tools and carry ordered soft/hard limits.
    let seq = arb_arrival_sequence()
        .new_tree(&mut runner)
        .expect("arb_arrival_sequence produces a value")
        .current();
    assert!(seq.soft_limit < seq.hard_limit);
    for arrival in &seq.arrivals {
        assert!(MCP_TOOLS.contains(&arrival.tool), "{}", arrival.tool);
    }
    if seq.concurrency_cap != 0 {
        assert!(seq.max_running_deadlines() <= seq.concurrency_cap);
    }
}
