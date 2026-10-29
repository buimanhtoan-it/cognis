//! Property 23: The watcher queue is bounded, coalescing, and truthful.
//!
//! For any event burst, entry count never exceeds the cap; a repeat path
//! replaces rather than appends; reaching the cap collapses to one Reconcile;
//! every offer returns without waiting on drain; reported status depth equals
//! the queue's actual depth.
//!
//! **Validates: Requirements 18.1, 18.2, 18.3, 18.4, 18.5**

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cognis_core::Capabilities;
use cognis_indexer::PlanRequest;
use cognis_owner::{
    owner_status_path, write_owner_status, Observation, ObservationKind, OwnerStatus, WatcherQueue,
};
use cognis_testgen::{arb_event_burst, EventBurst, FsEvent, FsEventKind};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::Value;

fn to_observation(event: &FsEvent) -> Observation {
    Observation {
        path: PathBuf::from(&event.path),
        kind: match event.kind {
            FsEventKind::Create => ObservationKind::Create,
            FsEventKind::Modify => ObservationKind::Modify,
            FsEventKind::Delete => ObservationKind::Delete,
        },
    }
}

/// Independent replay of [`WatcherQueue::offer`] so reported depth can be
/// checked against actual occupancy, not against the getter that produces it.
struct Expected {
    live: BTreeMap<String, FsEventKind>,
    overflowed: bool,
    cap: usize,
}

impl Expected {
    fn new(cap: usize) -> Self {
        Self {
            live: BTreeMap::new(),
            overflowed: false,
            cap,
        }
    }

    fn offer(&mut self, event: &FsEvent) {
        if self.overflowed {
            return;
        }
        if self.live.contains_key(&event.path) {
            self.live.insert(event.path.clone(), event.kind);
            return;
        }
        if self.cap == 0 || self.live.len() >= self.cap {
            self.live.clear();
            self.overflowed = true;
            return;
        }
        self.live.insert(event.path.clone(), event.kind);
    }

    fn depth(&self) -> usize {
        if self.overflowed {
            1
        } else {
            self.live.len()
        }
    }
}

fn sample_status(depth: usize) -> OwnerStatus {
    OwnerStatus {
        pid: std::process::id(),
        active: true,
        phase: "watching".into(),
        message: "Owner running.".into(),
        progress_percent: Some(100.0),
        pending_count: 0,
        pending_files: Vec::new(),
        inflight_count: 0,
        inflight_files: Vec::new(),
        recent_files: Vec::new(),
        updated_at: OwnerStatus::now_secs(),
        last_error: None,
        source_generation: 0,
        semantic_generation: 0,
        pending_vector_count: 0,
        watcher_queue_depth: depth,
        model_resident: false,
        fingerprint_match: true,
        capabilities: Capabilities {
            semantic_backend: false,
            accelerated_vector_search: false,
        },
        canonical_workspace: "p23".into(),
    }
}

fn unique_status_root() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "cognis-p23-status-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn report_depth_on_status_surface(queue: &WatcherQueue) -> std::io::Result<usize> {
    let root = unique_status_root();
    let path = owner_status_path(&root);
    write_owner_status(&path, &sample_status(queue.depth()))?;
    let text = fs::read_to_string(&path)?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let _ = fs::remove_dir_all(&root);
    value["watcherQueueDepth"]
        .as_u64()
        .map(|depth| depth as usize)
        .ok_or_else(|| std::io::Error::other("owner-status missing watcherQueueDepth"))
}

fn assert_burst(burst: EventBurst) -> Result<(), TestCaseError> {
    let queue = WatcherQueue::new(burst.cap);
    prop_assert_eq!(queue.cap(), burst.cap, "cap is the configured maximum");

    let mut expected = Expected::new(burst.cap);
    let started = Instant::now();
    for event in &burst.events {
        let before = queue.depth();
        let already_queued = expected.live.contains_key(&event.path) && !expected.overflowed;
        queue.offer(to_observation(event));
        expected.offer(event);

        prop_assert!(
            queue.depth() <= burst.cap,
            "entry count must never exceed the configured maximum"
        );
        prop_assert_eq!(
            queue.depth(),
            expected.depth(),
            "reported depth must equal actual occupancy"
        );
        prop_assert_eq!(queue.overflowed(), expected.overflowed);
        if already_queued {
            prop_assert_eq!(
                queue.depth(),
                before,
                "a repeat path must replace rather than append"
            );
        }
        if expected.overflowed {
            prop_assert_eq!(
                queue.depth(),
                1,
                "overflow collapses occupancy to one reconcile request"
            );
        }
    }
    prop_assert!(
        started.elapsed() < Duration::from_millis(500),
        "offer must not block the watcher thread waiting on drain"
    );

    let reported = report_depth_on_status_surface(&queue)
        .map_err(|error| TestCaseError::fail(format!("status surface: {error}")))?;
    prop_assert_eq!(
        reported,
        queue.depth(),
        "status surface watcherQueueDepth must equal the queue's actual depth"
    );

    if expected.overflowed {
        prop_assert_eq!(queue.take(), Some(PlanRequest::Reconcile));
    } else {
        let latest = burst.expected_latest_wins();
        prop_assert_eq!(latest.len(), expected.live.len());
        match queue.take() {
            Some(PlanRequest::Touched(paths)) => {
                let expected_paths: Vec<PathBuf> =
                    expected.live.keys().map(PathBuf::from).collect();
                prop_assert_eq!(paths, expected_paths);
            }
            None => prop_assert!(expected.live.is_empty()),
            other => {
                return Err(TestCaseError::fail(format!(
                    "under-cap burst must drain as Touched, got {other:?}"
                )));
            }
        }
    }
    prop_assert!(!queue.overflowed());
    prop_assert_eq!(queue.depth(), 0);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 23: The watcher queue is bounded, coalescing, and truthful.
    /// **Validates: Requirements 18.1, 18.2, 18.3, 18.4, 18.5**
    #[test]
    fn watcher_queue_is_bounded_coalescing_and_truthful(burst in arb_event_burst()) {
        assert_burst(burst)?;
    }
}
