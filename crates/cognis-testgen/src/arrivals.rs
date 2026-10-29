//! `arb_arrival_sequence` — tool-call arrival patterns.
//!
//! Design: "tool-call arrival patterns with generated per-stage costs, up to and
//! beyond the concurrency cap". Feeds P24 (Requirement 19.1–19.8).
//!
//! P24 asserts five things at once, and each needs something specific from the
//! generator:
//!
//! - *every call returns complete, truncated, or deadline-exceeded within the
//!   hard limit* — needs per-stage costs that sometimes exceed the soft limit
//!   and sometimes exceed the hard limit;
//! - *no stage starts after the soft limit* — needs a stage sequence, not a
//!   single opaque cost;
//! - *deadlines are independent* — needs several concurrent calls;
//! - *the count of running deadlines never exceeds the cap* — needs arrivals
//!   that go **beyond** the cap, which is why [`ArrivalSequence`] carries the cap
//!   and the generator deliberately over-subscribes it;
//! - *every capped rejection is retryable with no deadline started* — needs
//!   rejections to actually happen.

use std::time::Duration;

use proptest::prelude::*;

use cognis_core::MCP_TOOLS;

/// The four retrieval stages a tool call runs through, in order. `stage_gate`
/// is checked between them.
pub const STAGES: [&str; 4] = ["lexical", "semantic", "graph", "hydration"];

/// Per-stage simulated cost for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCosts {
    /// Milliseconds each stage takes, in [`STAGES`] order.
    pub millis: [u64; 4],
}

impl StageCosts {
    /// Total cost if every stage runs.
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.millis.iter().sum())
    }

    /// Cumulative cost after each stage, which is what a soft-limit check
    /// compares against.
    pub fn cumulative(&self) -> [Duration; 4] {
        let mut running = 0u64;
        let mut out = [Duration::ZERO; 4];
        for (index, cost) in self.millis.iter().enumerate() {
            running += cost;
            out[index] = Duration::from_millis(running);
        }
        out
    }

    /// The number of stages that start before `soft` elapses.
    ///
    /// Requirement 19.2: no stage is *initiated* once the soft limit has
    /// elapsed. Stage `i` starts at the cumulative cost of stages `0..i`, so it
    /// starts iff that prefix is strictly below the soft limit.
    pub fn stages_started_before(&self, soft: Duration) -> usize {
        let mut running = Duration::ZERO;
        let mut started = 0;
        for cost in self.millis {
            if running >= soft {
                break;
            }
            started += 1;
            running += Duration::from_millis(cost);
        }
        started
    }

    /// Whether a call with these costs must be truncated under `soft`.
    pub fn must_truncate(&self, soft: Duration) -> bool {
        self.stages_started_before(soft) < STAGES.len()
    }

    /// Whether a call with these costs would blow the hard limit if allowed to
    /// run every stage.
    pub fn would_exceed(&self, hard: Duration) -> bool {
        self.total() > hard
    }
}

/// One arriving tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    /// Which of the eight contract tools was called.
    pub tool: &'static str,
    /// Milliseconds after the start of the window that this call arrives.
    pub arrives_at_ms: u64,
    pub costs: StageCosts,
}

impl Arrival {
    pub fn arrives_at(&self) -> Duration {
        Duration::from_millis(self.arrives_at_ms)
    }
}

/// A whole arrival pattern, together with the limits it is measured against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrivalSequence {
    pub arrivals: Vec<Arrival>,
    /// `ConcurrencyLimiter` cap. `0` means unbounded, as
    /// `crates/cognis-mcp/src/caps.rs` defines it.
    pub concurrency_cap: usize,
    pub soft_limit: Duration,
    pub hard_limit: Duration,
}

impl ArrivalSequence {
    /// Arrivals in arrival order — ties broken by position so the ordering is
    /// total and the expectation below is deterministic.
    pub fn in_arrival_order(&self) -> Vec<(usize, &Arrival)> {
        let mut ordered: Vec<(usize, &Arrival)> = self.arrivals.iter().enumerate().collect();
        ordered.sort_by_key(|(index, arrival)| (arrival.arrives_at_ms, *index));
        ordered
    }

    /// Whether this sequence over-subscribes the cap, so at least one arrival
    /// must be rejected with a retryable envelope and **no deadline started**
    /// (Requirement 19.8).
    pub fn exceeds_cap(&self) -> bool {
        self.concurrency_cap != 0 && self.arrivals.len() > self.concurrency_cap
    }

    /// The greatest number of calls that may hold a running deadline at once.
    /// Requirement 19.7 / P24: the count of running deadlines never exceeds the
    /// concurrency cap.
    pub fn max_running_deadlines(&self) -> usize {
        if self.concurrency_cap == 0 {
            self.arrivals.len()
        } else {
            self.arrivals.len().min(self.concurrency_cap)
        }
    }

    /// Arrivals that must be truncated rather than completed.
    pub fn expected_truncations(&self) -> Vec<usize> {
        self.arrivals
            .iter()
            .enumerate()
            .filter(|(_, a)| a.costs.must_truncate(self.soft_limit))
            .map(|(index, _)| index)
            .collect()
    }
}

fn arb_stage_millis(max: u64) -> impl Strategy<Value = u64> {
    // Skewed low: most stages are cheap, a minority are pathological. A uniform
    // distribution would make almost every call exceed the limits and the
    // "complete" branch would never be exercised.
    prop_oneof![
        6 => 0u64..8,
        2 => 8u64..40,
        1 => 40..max,
    ]
}

/// Per-stage costs, spanning cheap calls through calls that blow the hard limit.
pub fn arb_stage_costs() -> impl Strategy<Value = StageCosts> {
    [
        arb_stage_millis(200),
        arb_stage_millis(200),
        arb_stage_millis(200),
        arb_stage_millis(200),
    ]
    .prop_map(|millis| StageCosts { millis })
}

fn arb_arrival() -> impl Strategy<Value = Arrival> {
    (
        prop::sample::select(MCP_TOOLS.to_vec()),
        0u64..250,
        arb_stage_costs(),
    )
        .prop_map(|(tool, arrives_at_ms, costs)| Arrival {
            tool,
            arrives_at_ms,
            costs,
        })
}

/// `arb_arrival_sequence` — arrivals up to **and beyond** the concurrency cap.
///
/// Caps are drawn from a small set including `0` (unbounded, as `caps.rs`
/// defines it) and the arrival count runs to `2 × cap + 2`, so both the
/// under-subscribed and the over-subscribed regimes appear. Without the
/// over-subscribed regime the "every capped rejection is retryable with no
/// deadline started" clause of P24 would be vacuous.
pub fn arb_arrival_sequence() -> impl Strategy<Value = ArrivalSequence> {
    (
        prop::sample::select(vec![0usize, 1, 2, 4, 8]),
        // Soft must be < hard; generated as (soft, extra) so the invariant holds
        // by construction rather than by filtering.
        20u64..200,
        10u64..400,
    )
        .prop_flat_map(|(cap, soft_ms, extra_ms)| {
            // Beyond the cap: an unbounded cap still gets a sensible spread.
            let max_arrivals = if cap == 0 { 6 } else { cap * 2 + 2 };
            (
                Just(cap),
                Just(soft_ms),
                Just(extra_ms),
                prop::collection::vec(arb_arrival(), 1..=max_arrivals),
            )
        })
        .prop_map(
            |(concurrency_cap, soft_ms, extra_ms, arrivals)| ArrivalSequence {
                arrivals,
                concurrency_cap,
                soft_limit: Duration::from_millis(soft_ms),
                hard_limit: Duration::from_millis(soft_ms + extra_ms),
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// The soft limit is always strictly below the hard limit, so the
        /// truncate and deadline-exceeded branches are distinguishable.
        #[test]
        fn limits_are_ordered(seq in arb_arrival_sequence()) {
            prop_assert!(seq.soft_limit < seq.hard_limit);
            prop_assert!(!seq.arrivals.is_empty());
        }

        /// Running deadlines never exceed the cap — Requirement 19.7 stated
        /// over the generator's own expectation.
        #[test]
        fn running_deadlines_respect_the_cap(seq in arb_arrival_sequence()) {
            if seq.concurrency_cap != 0 {
                prop_assert!(seq.max_running_deadlines() <= seq.concurrency_cap);
            }
        }

        /// Every arrival names one of the eight contract tools.
        #[test]
        fn arrivals_use_contract_tools(seq in arb_arrival_sequence()) {
            for arrival in &seq.arrivals {
                prop_assert!(MCP_TOOLS.contains(&arrival.tool), "{}", arrival.tool);
            }
        }

        /// Stage accounting is consistent: the number of stages started is
        /// bounded by the stage count, and a call that starts every stage is
        /// exactly one that is not truncated.
        #[test]
        fn stage_accounting_is_consistent(costs in arb_stage_costs(), soft_ms in 1u64..400) {
            let soft = Duration::from_millis(soft_ms);
            let started = costs.stages_started_before(soft);
            prop_assert!(started <= STAGES.len());
            prop_assert!(started >= 1, "the first stage always starts");
            prop_assert_eq!(costs.must_truncate(soft), started < STAGES.len());
        }

        /// Cumulative costs are monotonically non-decreasing and end at the
        /// total.
        #[test]
        fn cumulative_costs_are_monotone(costs in arb_stage_costs()) {
            let cumulative = costs.cumulative();
            for pair in cumulative.windows(2) {
                prop_assert!(pair[0] <= pair[1]);
            }
            prop_assert_eq!(cumulative[STAGES.len() - 1], costs.total());
        }
    }

    #[test]
    fn an_over_subscribed_sequence_is_detected() {
        let arrival = Arrival {
            tool: MCP_TOOLS[0],
            arrives_at_ms: 0,
            costs: StageCosts {
                millis: [1, 1, 1, 1],
            },
        };
        let seq = ArrivalSequence {
            arrivals: vec![arrival.clone(), arrival.clone(), arrival],
            concurrency_cap: 2,
            soft_limit: Duration::from_millis(100),
            hard_limit: Duration::from_millis(200),
        };
        assert!(seq.exceeds_cap());
        assert_eq!(seq.max_running_deadlines(), 2);
    }

    #[test]
    fn a_zero_cap_is_unbounded() {
        let arrival = Arrival {
            tool: MCP_TOOLS[0],
            arrives_at_ms: 0,
            costs: StageCosts {
                millis: [1, 1, 1, 1],
            },
        };
        let seq = ArrivalSequence {
            arrivals: vec![arrival.clone(), arrival],
            concurrency_cap: 0,
            soft_limit: Duration::from_millis(100),
            hard_limit: Duration::from_millis(200),
        };
        assert!(!seq.exceeds_cap());
        assert_eq!(seq.max_running_deadlines(), 2);
    }
}
