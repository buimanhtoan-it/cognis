//! Bounded, latest-wins watcher queue (task 13.3).
//!
//! The notify callback calls [`WatcherQueue::offer`], which never waits for the
//! planner to drain: a repeat path replaces the queued observation, and a new
//! path that would exceed the configured cap collapses the queue into a single
//! [`PlanRequest::Reconcile`] (Requirements 18.1–18.4).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use cognis_indexer::PlanRequest;

/// One filesystem observation, keyed in the queue by [`Observation::path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub path: PathBuf,
    pub kind: ObservationKind,
}

/// The three event kinds the owner watcher records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationKind {
    Create,
    Modify,
    Delete,
}

struct WatcherQueueInner {
    map: BTreeMap<PathBuf, Observation>,
    cap: usize,
    overflowed: bool,
    last_offer_at: Option<Instant>,
}

/// Bounded latest-wins queue between the file-system watcher and the planner.
pub struct WatcherQueue {
    inner: Mutex<WatcherQueueInner>,
}

impl WatcherQueue {
    /// Create a queue whose entry count will never exceed `cap`.
    ///
    /// A `cap` of `0` treats every offer as overflow (immediate reconcile).
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(WatcherQueueInner {
                map: BTreeMap::new(),
                cap,
                overflowed: false,
                last_offer_at: None,
            }),
        }
    }

    /// Record an observation without waiting for the consumer.
    ///
    /// Repeat paths replace in place (Requirement 18.2). A new path that would
    /// grow the queue past `cap` clears the map and flags overflow so the next
    /// drain is one [`PlanRequest::Reconcile`] (Requirement 18.3).
    pub fn offer(&self, observation: Observation) {
        let mut inner = lock_inner(&self.inner);
        inner.last_offer_at = Some(Instant::now());
        if inner.overflowed {
            return;
        }
        if inner.map.contains_key(&observation.path) {
            inner.map.insert(observation.path.clone(), observation);
            return;
        }
        if inner.cap == 0 || inner.map.len() >= inner.cap {
            inner.map.clear();
            inner.overflowed = true;
            return;
        }
        inner.map.insert(observation.path.clone(), observation);
    }

    /// Current status-surface depth: `1` while a reconcile is pending, else
    /// the number of distinct queued paths (Requirement 18.1, 18.5).
    pub fn depth(&self) -> usize {
        let inner = lock_inner(&self.inner);
        if inner.overflowed {
            1
        } else {
            inner.map.len()
        }
    }

    /// True after the queue collapsed to a single reconcile request.
    pub fn overflowed(&self) -> bool {
        lock_inner(&self.inner).overflowed
    }

    /// Configured maximum entry count (Requirement 18.1).
    pub fn cap(&self) -> usize {
        lock_inner(&self.inner).cap
    }

    /// Instant of the most recent offer, if any — used with the debounce window.
    pub fn last_offer_at(&self) -> Option<Instant> {
        lock_inner(&self.inner).last_offer_at
    }

    /// Take the queued work for the planner and empty the queue.
    pub fn take(&self) -> Option<PlanRequest> {
        let mut inner = lock_inner(&self.inner);
        if inner.overflowed {
            inner.overflowed = false;
            inner.map.clear();
            inner.last_offer_at = None;
            return Some(PlanRequest::Reconcile);
        }
        if inner.map.is_empty() {
            return None;
        }
        let paths: Vec<PathBuf> = inner.map.keys().cloned().collect();
        inner.map.clear();
        inner.last_offer_at = None;
        Some(PlanRequest::Touched(paths))
    }
}

fn lock_inner(inner: &Mutex<WatcherQueueInner>) -> std::sync::MutexGuard<'_, WatcherQueueInner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn obs(path: &str, kind: ObservationKind) -> Observation {
        Observation {
            path: PathBuf::from(path),
            kind,
        }
    }

    #[test]
    fn repeat_path_replaces_rather_than_appends() {
        let queue = WatcherQueue::new(8);
        queue.offer(obs("src/a.py", ObservationKind::Create));
        queue.offer(obs("src/a.py", ObservationKind::Modify));
        queue.offer(obs("src/a.py", ObservationKind::Delete));
        assert_eq!(queue.depth(), 1);
        assert!(!queue.overflowed());
        match queue.take() {
            Some(PlanRequest::Touched(paths)) => {
                assert_eq!(paths, vec![PathBuf::from("src/a.py")]);
            }
            other => panic!("expected Touched, got {other:?}"),
        }
        assert_eq!(queue.depth(), 0);
    }

    #[test]
    fn distinct_paths_under_cap_stay_touched() {
        let queue = WatcherQueue::new(4);
        queue.offer(obs("a.rs", ObservationKind::Create));
        queue.offer(obs("b.rs", ObservationKind::Modify));
        queue.offer(obs("c.rs", ObservationKind::Delete));
        assert_eq!(queue.depth(), 3);
        match queue.take() {
            Some(PlanRequest::Touched(paths)) => {
                assert_eq!(
                    paths,
                    vec![
                        PathBuf::from("a.rs"),
                        PathBuf::from("b.rs"),
                        PathBuf::from("c.rs"),
                    ]
                );
            }
            other => panic!("expected Touched, got {other:?}"),
        }
    }

    #[test]
    fn reaching_cap_plus_one_distinct_path_collapses_to_reconcile() {
        let queue = WatcherQueue::new(2);
        queue.offer(obs("a.rs", ObservationKind::Create));
        queue.offer(obs("b.rs", ObservationKind::Create));
        assert_eq!(queue.depth(), 2);
        assert!(!queue.overflowed());
        queue.offer(obs("c.rs", ObservationKind::Modify));
        assert!(queue.overflowed());
        assert_eq!(queue.depth(), 1);
        assert_eq!(queue.take(), Some(PlanRequest::Reconcile));
        assert!(!queue.overflowed());
        assert_eq!(queue.depth(), 0);
    }

    #[test]
    fn offers_after_overflow_are_accepted_without_growing() {
        let queue = WatcherQueue::new(1);
        queue.offer(obs("a.rs", ObservationKind::Create));
        queue.offer(obs("b.rs", ObservationKind::Create));
        assert!(queue.overflowed());
        let start = Instant::now();
        for i in 0..1000 {
            queue.offer(obs(&format!("extra-{i}.rs"), ObservationKind::Modify));
        }
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "offer must not block the watcher thread waiting on drain"
        );
        assert_eq!(queue.depth(), 1);
        assert_eq!(queue.take(), Some(PlanRequest::Reconcile));
    }

    #[test]
    fn depth_never_exceeds_cap() {
        let cap = 3;
        let queue = WatcherQueue::new(cap);
        for i in 0..20 {
            queue.offer(obs(&format!("f{i}.rs"), ObservationKind::Modify));
            assert!(queue.depth() <= cap);
        }
        assert!(queue.overflowed());
        assert_eq!(queue.depth(), 1);
    }

    #[test]
    fn debounce_timestamp_is_recorded_on_offer() {
        let queue = WatcherQueue::new(8);
        assert!(queue.last_offer_at().is_none());
        queue.offer(obs("a.rs", ObservationKind::Create));
        let first = queue.last_offer_at().expect("timestamp");
        queue.offer(obs("a.rs", ObservationKind::Modify));
        let second = queue.last_offer_at().expect("timestamp");
        assert!(second >= first);
        let _ = queue.take();
        assert!(queue.last_offer_at().is_none());
        let _ = Path::new("a.rs");
    }
}
