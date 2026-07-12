// Feature: non-code-artifact-coverage, Property 22: The non-regression gate decision is correct
//
// Property 22 (design.md): the non-regression gate permits a release **iff**
// both non-code metrics (Coverage, Recall@k) strictly improve AND the code
// metrics (MRR, Contamination@k) stay within the pre-declared tolerance ε;
// otherwise it blocks — including whenever the before/after measurement is
// missing (unmeasured) — and every block names the axis that failed.
//
// Validates: Requirements 14.3, 14.4, 14.5, 15.3, 15.4, 16.2
//
// The decision oracle below is encoded INDEPENDENTLY of the gate. To keep the
// oracle and the gate in agreement on float boundary cases (Δ exactly ±ε), the
// oracle uses the identical comparison operators and the identical Δ
// computation (`cand − base`) as `CoverageRegressionGate::decide`, so boundary
// inputs agree by construction rather than by luck.

use cognis_eval::bench::{
    ClaimStatus, CodeMetrics, CoverageGateInput, CoverageMeasurement, CoverageRegressionGate,
    NonCodeMetrics, DEFAULT_COVERAGE_EPSILON,
};
use proptest::prelude::*;

/// A bounded metric value in `[0, 1]` (metrics are fractions).
fn metric() -> impl Strategy<Value = f64> {
    0.0f64..=1.0f64
}

/// A `CoverageMeasurement` with each of its four metrics in `[0, 1]`.
fn measurement() -> impl Strategy<Value = CoverageMeasurement> {
    (metric(), metric(), metric(), metric()).prop_map(|(mrr, contam, cov, rec)| {
        CoverageMeasurement::new(CodeMetrics::new(mrr, contam), NonCodeMetrics::new(cov, rec))
    })
}

/// An optional measurement — `None` models the "unmeasured" case.
fn maybe_measurement() -> impl Strategy<Value = Option<CoverageMeasurement>> {
    proptest::option::of(measurement())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn prop22_non_regression_gate_decision_is_correct(
        baseline in maybe_measurement(),
        candidate in maybe_measurement(),
    ) {
        let eps = DEFAULT_COVERAGE_EPSILON;
        let gate = CoverageRegressionGate::default();
        let input = CoverageGateInput { baseline, candidate };
        let verdict = gate.decide(&input);

        match (baseline, candidate) {
            // -------- Unmeasured: a missing before/after ALWAYS blocks. -------
            (None, _) | (_, None) => {
                prop_assert!(
                    !verdict.is_permit(),
                    "unmeasured input must block (baseline={:?}, candidate={:?})",
                    baseline.is_some(),
                    candidate.is_some()
                );
                // A block leaves the improvement claim conjectured (Req 16.1/16.2).
                prop_assert_eq!(verdict.claim_status(), ClaimStatus::Conjectured);
                // The block reason names it as unmeasured.
                prop_assert!(
                    verdict.reasons().iter().any(|r| r.contains("unmeasured")),
                    "unmeasured block must say so: {:?}",
                    verdict.reasons()
                );
            }

            // -------- Fully measured: exact ship/no-ship oracle. --------------
            (Some(base), Some(cand)) => {
                // Δ computed exactly as the gate does, so ±ε boundaries agree.
                let d_mrr = cand.code.mrr - base.code.mrr;
                let d_contam = cand.code.contamination - base.code.contamination;

                let code_ok = d_mrr >= -eps && d_contam <= eps;
                let noncode_ok = cand.non_code.coverage > base.non_code.coverage
                    && cand.non_code.recall > base.non_code.recall;
                let permit_condition = code_ok && noncode_ok;

                // Core of Property 22: permit iff both non-code strictly improve
                // AND code stays within ε (Requirements 14.4 / 14.5 / 15.3).
                prop_assert_eq!(
                    verdict.is_permit(),
                    permit_condition,
                    "permit mismatch: d_mrr={}, d_contam={}, \
                     cov {}->{}, rec {}->{}",
                    d_mrr,
                    d_contam,
                    base.non_code.coverage,
                    cand.non_code.coverage,
                    base.non_code.recall,
                    cand.non_code.recall
                );

                if permit_condition {
                    // Permit records the claim verified (Requirement 16.3) with
                    // no reasons.
                    prop_assert_eq!(verdict.claim_status(), ClaimStatus::Verified);
                    prop_assert!(verdict.reasons().is_empty());
                } else {
                    // Block records the claim conjectured (Requirement 16.1).
                    prop_assert_eq!(verdict.claim_status(), ClaimStatus::Conjectured);
                    prop_assert!(!verdict.reasons().is_empty());

                    let reasons = verdict.reasons();
                    // Each failed axis must be named (Requirements 14.3 / 15.4).
                    if d_mrr < -eps {
                        prop_assert!(
                            reasons.iter().any(|r| r.contains("MRR")),
                            "MRR regression must name MRR: {:?}",
                            reasons
                        );
                        prop_assert!(
                            reasons.iter().any(|r| r.contains("ΔMRR")),
                            "MRR regression must name ΔMRR: {:?}",
                            reasons
                        );
                    }
                    if d_contam > eps {
                        prop_assert!(
                            reasons.iter().any(|r| r.contains("Contamination")),
                            "contamination regression must name Contamination: {:?}",
                            reasons
                        );
                    }
                    if cand.non_code.coverage <= base.non_code.coverage {
                        prop_assert!(
                            reasons.iter().any(|r| r.contains("Coverage")),
                            "coverage shortfall must name Coverage: {:?}",
                            reasons
                        );
                    }
                    if cand.non_code.recall <= base.non_code.recall {
                        prop_assert!(
                            reasons.iter().any(|r| r.contains("Recall")),
                            "recall shortfall must name Recall: {:?}",
                            reasons
                        );
                    }
                }
            }
        }
    }
}
