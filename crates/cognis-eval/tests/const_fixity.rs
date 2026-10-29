//! Constant-fixity + verified/conjectured bookkeeping unit tests for the
//! non-regression coverage gate (Task 13.3).
//!
//! Feature: non-code-artifact-coverage — no-migration smoke + constant fixity.
//!
//! Validates:
//! - Requirement 14.1 / 16.4: the non-regression tolerance ε is a fixed,
//!   pre-declared non-negative constant (default 0.01), never tuned to a
//!   benchmark sample, and the default gate uses exactly that ε.
//! - Requirement 16.1 / 16.3: verified/conjectured status bookkeeping is
//!   correct — a permit records the improvement claim as `Verified`; a block
//!   leaves it `Conjectured`.
//!
//! These are plain example assertions (not property tests): ε is a compile-time
//! literal and the verdict → claim-status mapping is a total function over two
//! variants, so a couple of pinned examples fully cover it. Kept intentionally
//! non-overlapping with the gate-decision property test (13.2), which exercises
//! the block/permit *decision* logic rather than these fixed constants.

use cognis_eval::bench::{
    ClaimStatus, CoverageGateVerdict, CoverageRegressionGate, DEFAULT_COVERAGE_EPSILON,
};

/// ε is the fixed, pre-declared default and the default gate adopts it verbatim
/// (Req 14.1 / 16.4). ε must also be non-negative, as the requirement states.
#[test]
// The ε non-negativity check below is a deliberate compile-time-constant
// assertion documenting Req 14.1 (ε is a fixed, pre-declared non-negative
// constant). clippy flags constant assertions by default; the constant value is
// exactly the point here, so the lint is intentionally allowed.
#[allow(clippy::assertions_on_constants)]
fn coverage_epsilon_is_the_fixed_pre_declared_constant() {
    assert_eq!(
        DEFAULT_COVERAGE_EPSILON, 0.01,
        "ε must stay pinned at 0.01 (Req 14.1 / 16.4): declared before any \
         measurement and never tuned to a sample"
    );
    assert!(
        DEFAULT_COVERAGE_EPSILON >= 0.0,
        "ε must be non-negative (Req 14.1)"
    );
    assert_eq!(
        CoverageRegressionGate::default().epsilon,
        DEFAULT_COVERAGE_EPSILON,
        "the default gate must use the pre-declared ε, not a sample-derived one"
    );
}

/// A permit records the improvement claim as verified; a block leaves it
/// conjectured (Req 16.1 / 16.3).
#[test]
fn verdict_claim_status_bookkeeping_is_correct() {
    // Permit → Verified (Req 16.3).
    assert_eq!(
        CoverageGateVerdict::Permit.claim_status(),
        ClaimStatus::Verified,
        "a permit must record the improvement claim as Verified (Req 16.3)"
    );

    // Block → Conjectured, regardless of the (possibly empty) reason list
    // (Req 16.1).
    assert_eq!(
        CoverageGateVerdict::Block(vec!["ΔMRR < −ε (MRR)".to_string()]).claim_status(),
        ClaimStatus::Conjectured,
        "a block must leave the improvement claim Conjectured (Req 16.1)"
    );
    assert_eq!(
        CoverageGateVerdict::Block(Vec::new()).claim_status(),
        ClaimStatus::Conjectured,
        "a block leaves the claim Conjectured even with no reasons recorded"
    );

    // A permit is a permit; a block never is (sanity on the discriminator used
    // by the bookkeeping above).
    assert!(CoverageGateVerdict::Permit.is_permit());
    assert!(!CoverageGateVerdict::Block(Vec::new()).is_permit());
}
