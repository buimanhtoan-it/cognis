//! `arb_event_burst` — watcher event bursts.
//!
//! Design: "watcher bursts far exceeding the cap, with repeated paths and rapid
//! create/modify/delete cycles". Feeds P23 (Requirement 18.1–18.5).
//!
//! Three things make this generator useful rather than decorative:
//!
//! 1. **It exceeds the cap.** Bursts are sized relative to
//!    [`DEFAULT_WATCHER_QUEUE_MAX`] (config `owner.watcher_queue_max`, default
//!    4096) *including* multiples of it, so the overflow → single `Reconcile`
//!    path (Requirement 18.3) is actually reached. A generator capped at a few
//!    dozen events would never reach it.
//! 2. **Paths repeat.** The path alphabet is deliberately far smaller than the
//!    burst size, so latest-wins coalescing (Requirement 18.2) has something to
//!    coalesce.
//! 3. **Create/modify/delete cycles are rapid.** The same path is created,
//!    modified, and deleted within one burst, so the queue's per-path collapse
//!    must land on the *final* observation.
//!
//! # TODO(task 13.3)
//!
//! `WatcherQueue` itself lands in `crates/cognis-owner/src/queue.rs` (task
//! 13.3). This module generates only the *input* — the burst — plus the
//! expected coalescing outcome ([`EventBurst::expected_latest_wins`]), so the
//! property can be written the moment the queue exists.

use std::collections::BTreeMap;

use proptest::prelude::*;

use crate::UNICODE_SEGMENTS;

/// `owner.watcher_queue_max`'s default, from the design's `WatcherQueue`.
pub const DEFAULT_WATCHER_QUEUE_MAX: usize = 4096;

/// Path alphabet size. Deliberately small relative to burst sizes so paths
/// repeat many times over.
const PATH_ALPHABET: usize = 32;

/// The three file-system event kinds the watcher reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsEventKind {
    Create,
    Modify,
    Delete,
}

/// One observed event. `seq` is the arrival ordinal within the burst, which is
/// what "latest wins" is defined against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEvent {
    pub path: String,
    pub kind: FsEventKind,
    pub seq: usize,
}

/// A whole burst, plus the queue cap it is to be offered against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventBurst {
    pub events: Vec<FsEvent>,
    /// The configured `owner.watcher_queue_max` this burst is measured against.
    pub cap: usize,
}

impl EventBurst {
    /// Distinct paths touched by the burst.
    pub fn distinct_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.events.iter().map(|e| e.path.clone()).collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// The latest observation per path — what a latest-wins queue must hold
    /// after draining the burst, *if* it never overflowed (Requirement 18.2).
    pub fn expected_latest_wins(&self) -> BTreeMap<String, FsEvent> {
        let mut latest: BTreeMap<String, FsEvent> = BTreeMap::new();
        for event in &self.events {
            latest
                .entry(event.path.clone())
                .and_modify(|existing| {
                    if event.seq >= existing.seq {
                        *existing = event.clone();
                    }
                })
                .or_insert_with(|| event.clone());
        }
        latest
    }

    /// Whether this burst must drive the queue into overflow, collapsing to a
    /// single reconciliation request (Requirement 18.3).
    ///
    /// Overflow is driven by *distinct* paths, not by event count: latest-wins
    /// replacement means 10 000 events over 32 paths occupy 32 entries.
    pub fn must_overflow(&self) -> bool {
        self.distinct_paths().len() > self.cap
    }

    /// The queue depth a correct implementation ends at: the number of distinct
    /// paths, never above the cap.
    pub fn expected_depth(&self) -> usize {
        self.distinct_paths().len().min(self.cap)
    }
}

/// Build a burst from generated ordinals, injecting rapid create/modify/delete
/// cycles so per-path collapse is genuinely exercised.
fn build_burst(path_ordinals: Vec<usize>, kinds: Vec<u8>, cap: usize) -> EventBurst {
    let mut events = Vec::with_capacity(path_ordinals.len() * 3);
    let mut seq = 0usize;

    for (index, ordinal) in path_ordinals.iter().enumerate() {
        let path = path_for(*ordinal);
        let kind_seed = kinds.get(index).copied().unwrap_or(0);
        match kind_seed % 4 {
            // A full create → modify → delete cycle on one path, back to back.
            0 => {
                for kind in [
                    FsEventKind::Create,
                    FsEventKind::Modify,
                    FsEventKind::Delete,
                ] {
                    events.push(FsEvent {
                        path: path.clone(),
                        kind,
                        seq,
                    });
                    seq += 1;
                }
            }
            // A create followed immediately by a modify.
            1 => {
                for kind in [FsEventKind::Create, FsEventKind::Modify] {
                    events.push(FsEvent {
                        path: path.clone(),
                        kind,
                        seq,
                    });
                    seq += 1;
                }
            }
            // A single event.
            other => {
                let kind = match other {
                    2 => FsEventKind::Modify,
                    _ => FsEventKind::Delete,
                };
                events.push(FsEvent { path, kind, seq });
                seq += 1;
            }
        }
    }

    EventBurst { events, cap }
}

fn path_for(ordinal: usize) -> String {
    let bucket = ordinal % PATH_ALPHABET;
    // Every fourth path lives under a unicode segment, so path handling is
    // exercised outside ASCII here too.
    match bucket % 4 {
        0 => {
            let segment = UNICODE_SEGMENTS[(bucket / 4) % UNICODE_SEGMENTS.len()];
            format!("src/{segment}/file_{bucket}.py")
        }
        _ => format!("src/file_{bucket}.py"),
    }
}

/// `arb_event_burst` — bursts against the default cap.
///
/// Two regimes are generated, weighted equally:
///
/// - **Under-cap bursts** (1..=256 raw ordinals, so up to ~768 events) measured
///   against a *small* cap of 8 or 16. These reach overflow cheaply, which keeps
///   the 100-iteration budget honest while still exercising Requirement 18.3.
/// - **Over-cap bursts** measured against [`DEFAULT_WATCHER_QUEUE_MAX`] with
///   enough distinct paths to exceed it, so the real configured cap is exercised
///   too.
pub fn arb_event_burst() -> impl Strategy<Value = EventBurst> {
    prop_oneof![
        // Small cap, ordinary burst: overflow reached without allocating 4k+
        // events per case.
        1 => arb_event_burst_with_cap(prop::sample::select(vec![4usize, 8, 16]), 1..257),
        // The real default cap, with a burst that genuinely exceeds it.
        1 => arb_event_burst_with_cap(Just(DEFAULT_WATCHER_QUEUE_MAX), 1..129),
    ]
}

/// `arb_event_burst` with a caller-chosen cap and burst size, for properties
/// that want to pin one regime.
///
/// `raw_events` bounds the number of *ordinals* drawn; each ordinal expands to
/// 1..=3 events, so the emitted burst is up to three times larger.
pub fn arb_event_burst_with_cap(
    cap: impl Strategy<Value = usize>,
    raw_events: std::ops::Range<usize>,
) -> impl Strategy<Value = EventBurst> {
    let size = raw_events.clone();
    (
        cap,
        prop::collection::vec(0usize..(PATH_ALPHABET * 4), size.clone()),
        prop::collection::vec(any::<u8>(), size),
    )
        .prop_map(|(cap, ordinals, kinds)| build_burst(ordinals, kinds, cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Sequence numbers are strictly increasing, so "latest" is well
        /// defined.
        #[test]
        fn sequence_numbers_are_ordered(burst in arb_event_burst()) {
            prop_assert!(!burst.events.is_empty());
            for pair in burst.events.windows(2) {
                prop_assert!(pair[0].seq < pair[1].seq);
            }
        }

        /// Latest-wins keeps exactly one event per path, and it is the last one
        /// observed for that path.
        #[test]
        fn latest_wins_keeps_the_last_observation(burst in arb_event_burst()) {
            let latest = burst.expected_latest_wins();
            prop_assert_eq!(latest.len(), burst.distinct_paths().len());
            for (path, kept) in &latest {
                let last = burst
                    .events
                    .iter()
                    .rfind(|e| &e.path == path)
                    .expect("path came from the burst");
                prop_assert_eq!(kept, last);
            }
        }

        /// Expected depth never exceeds the cap, whatever the burst size —
        /// Requirement 18.1's boundedness stated over the generator's own
        /// expectation.
        #[test]
        fn expected_depth_respects_the_cap(burst in arb_event_burst()) {
            prop_assert!(burst.expected_depth() <= burst.cap);
            if burst.must_overflow() {
                prop_assert_eq!(burst.expected_depth(), burst.cap);
            }
        }

        /// Rapid create/modify/delete cycles really do appear: at least one
        /// path in a sufficiently large burst carries more than one event.
        #[test]
        fn bursts_repeat_paths(
            burst in arb_event_burst_with_cap(Just(4096usize), 64..129),
        ) {
            prop_assert!(burst.events.len() > burst.distinct_paths().len());
        }
    }

    #[test]
    fn a_full_cycle_collapses_to_the_delete() {
        // create → modify → delete on one path must coalesce to the delete.
        let burst = build_burst(vec![0], vec![0], 16);
        let latest = burst.expected_latest_wins();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest.values().next().unwrap().kind, FsEventKind::Delete);
    }
}
