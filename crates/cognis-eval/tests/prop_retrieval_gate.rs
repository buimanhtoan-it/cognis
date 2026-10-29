//! Feature: cognis-one-toggle-engine, Property 39: For any query in the golden
//! set, the top-k identifier sequence before a change equals the sequence after
//! the change at the same committed source_generation for every query whose
//! retrieval path the change does not intentionally alter; for any (before,
//! after, tolerance) triple, the gate fails exactly when a metric degrades by
//! more than the tolerance (or an unaltered ranking drifts); and two executions
//! of the same evaluation at the same committed generation produce identical
//! metric values.
//!
//! Validates: Requirements 26.2, 26.4, 26.6
//! Properties: P39
//!
//! The decision oracle uses the same comparison operators as
//! `degraded_metrics` so ±tolerance boundaries agree by construction.

use cognis_eval::bench::{compute_metrics, Metrics};
use cognis_eval::gate::{degraded_metrics, QueryRanking, RetrievalGate, RetrievalSide};
use proptest::prelude::*;
use std::collections::BTreeSet;

fn metric() -> impl Strategy<Value = f64> {
    0.0f64..=1.0f64
}

fn metrics() -> impl Strategy<Value = Metrics> {
    (metric(), metric(), metric(), metric()).prop_map(|(recall, precision, mrr, contamination)| {
        Metrics {
            recall,
            precision,
            mrr,
            contamination,
        }
    })
}

fn ident() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("alpha".to_string()),
        Just("beta".to_string()),
        Just("gamma".to_string()),
        Just("delta".to_string()),
    ]
}

fn ranking(query: &'static str) -> impl Strategy<Value = QueryRanking> {
    (prop::collection::vec(ident(), 1..4), proptest::bool::ANY).prop_map(
        move |(top_k, intentionally_altered)| QueryRanking {
            query: query.to_string(),
            top_k,
            intentionally_altered,
        },
    )
}

fn pair_for_query(query: &'static str) -> impl Strategy<Value = (QueryRanking, QueryRanking)> {
    ranking(query).prop_flat_map(move |before| {
        let clone = before.clone();
        proptest::bool::ANY.prop_flat_map(move |keep| {
            let before = before.clone();
            let clone = clone.clone();
            if keep || before.intentionally_altered {
                Just((before.clone(), clone)).boxed()
            } else {
                ranking(query)
                    .prop_map(move |mut after| {
                        after.intentionally_altered = false;
                        (before.clone(), after)
                    })
                    .boxed()
            }
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop39_retrieval_ranking_stable_and_gate_deterministic(
        before_metrics in metrics(),
        after_metrics in metrics(),
        tolerance in 0.0f64..=0.25f64,
        q0 in pair_for_query("q0"),
        q1 in pair_for_query("q1"),
        generation in 1i64..50,
    ) {
        let (b0, a0) = q0;
        let (b1, a1) = q1;
        let before = RetrievalSide {
            source_generation: generation,
            corpus_id: "golden".into(),
            metrics: before_metrics,
            queries: vec![b0.clone(), b1.clone()],
        };
        let after = RetrievalSide {
            source_generation: generation,
            corpus_id: "golden".into(),
            metrics: after_metrics,
            queries: vec![a0.clone(), a1.clone()],
        };

        let gate = RetrievalGate::new(tolerance);
        prop_assert_eq!(gate.tolerance(), tolerance.max(0.0));

        let first = gate.evaluate(&before, &after).expect("matching generation/corpus");
        let second = gate.evaluate(&before, &after).expect("deterministic re-run");
        prop_assert_eq!(&first, &second, "two executions at the same generation must match");
        prop_assert_eq!(first.before, before_metrics);
        prop_assert_eq!(first.after, after_metrics);

        let ranking_ok = (b0.intentionally_altered || b0.top_k == a0.top_k)
            && (b1.intentionally_altered || b1.top_k == a1.top_k);
        let oracle_degraded = degraded_metrics(&before_metrics, &after_metrics, gate.tolerance());
        let metrics_ok = oracle_degraded.is_empty();

        prop_assert_eq!(
            first.ranking_stable,
            ranking_ok,
            "unaltered top-k sequences must match (26.2)"
        );
        prop_assert_eq!(&first.degraded, &oracle_degraded);
        prop_assert_eq!(
            first.passed,
            ranking_ok && metrics_ok,
            "gate fails exactly when a metric degrades beyond tolerance or an unaltered ranking drifts"
        );

        let relevant: BTreeSet<String> = ["alpha".into()].into_iter().collect();
        let hubs: BTreeSet<String> = ["delta".into()].into_iter().collect();
        let order = b0.top_k.clone();
        let m1 = compute_metrics(&order, &relevant, &hubs, order.len().max(1));
        let m2 = compute_metrics(&order, &relevant, &hubs, order.len().max(1));
        prop_assert_eq!(m1, m2, "identical evaluation at the same generation is deterministic (26.6)");
    }
}
