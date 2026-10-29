//! Constant-fixity unit test for the retrieval fusion damping constant
//! (Task 13.3).
//!
//! Feature: non-code-artifact-coverage — no-migration smoke + constant fixity.
//!
//! Validates: Requirement 10.6 (Retrieval_Fusion introduces no fitted or
//! benchmark-tuned constant, keeping `rrf_k` fixed at 60).
//!
//! `DEFAULT_RRF_K` is the standard RRF damping constant (Cormack et al. 2009),
//! declared once as a `pub const` in `cognis_retrieval::fusion` and never
//! derived from — or tuned to — any cognis benchmark sample. This test pins its
//! value so that fusing typed non-code artifact nodes alongside code symbols can
//! never silently introduce a fitted ranking constant. It is a plain example
//! assertion (not a property test): the constant is a compile-time literal.

use cognis_retrieval::DEFAULT_RRF_K;

/// `rrf_k == 60`, fixed and not sample-derived (Req 10.6).
#[test]
fn rrf_k_is_the_fixed_pre_declared_constant() {
    assert_eq!(
        DEFAULT_RRF_K, 60.0,
        "rrf_k must stay pinned at 60 (Req 10.6): it is a pre-declared damping \
         constant, never tuned to a benchmark sample"
    );
}
