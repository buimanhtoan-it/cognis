//! Lazy single-flight model lifecycle (`ModelSlot`).
//!
//! Shared by `StoreEngine` (mcpd) and `IndexerPipeline` (indexd) so both heavy
//! processes honor the same demand-driven ONNX lifecycle (Requirement 2.5 / 2.6;
//! Correctness Properties 6–7; bug facets
//! `processLoadsDuplicateModelWithoutDemand` and
//! `concurrentDemandCanDuplicateLoadOrRetryStorm`).
//!
//! ## State machine
//!
//! ```text
//! Empty ──borrow_or_load──► Loading ──Ok──► Ready { Arc, in_flight, last_used }
//!                              │
//!                              └──Err──► Failed { until } ──(after cooldown)──► Empty
//! Ready ──try_idle_evict (in_flight == 0 ∧ idle)──► Empty
//! Ready ──try_idle_evict (in_flight > 0)──► InFlight (refused)
//! ```
//!
//! * **Single-flight.** Concurrent `borrow_or_load` coalesces into one factory
//!   call; all waiters observe the same `Ok` / `Err`.
//! * **Failure cooldown.** After a load error the slot suppresses retries until
//!   `Instant::now() >= until`, then allows another attempt. The cooldown
//!   duration is supplied by the caller (env-resolved or test override).
//! * **In-flight-safe.** Borrowing increments `in_flight_count`; the
//!   [`ModelBorrow`] decrements on drop. Eviction is refused while
//!   `in_flight_count > 0`.
//! * **Empty degradation.** `try_borrow` on Empty/Failed returns `None`;
//!   callers treat that as "no embedder" (empty semantic leg / pending
//!   vectors), never a hard tool error.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cognis_core::Result;

use crate::Embedder;

// ---------------------------------------------------------------------------
// Env / defaults
// ---------------------------------------------------------------------------

/// Environment variable overriding the failure-cooldown window (seconds).
///
/// Accepted by [`failure_cooldown_from_env`]. Empty / missing / invalid →
/// [`DEFAULT_FAILURE_COOLDOWN`].
pub const FAILURE_COOLDOWN_ENV: &str = "COGNIS_MODEL_FAILURE_COOLDOWN_S";

/// Environment variable overriding the idle-eviction window (seconds).
///
/// Accepted by [`idle_evict_after_from_env`]. Empty / missing / invalid →
/// [`DEFAULT_IDLE_EVICT_AFTER`].
pub const IDLE_EVICT_ENV: &str = "COGNIS_MODEL_IDLE_EVICT_S";

/// Default failure-cooldown window after a load error (Requirement 2.5).
///
/// Thirty seconds is long enough to absorb a retry storm after a missing-asset
/// / transient backend failure without starving a later successful demand.
pub const DEFAULT_FAILURE_COOLDOWN: Duration = Duration::from_secs(30);

/// Default idle interval before a Ready session may be released (Requirement
/// 2.6 — primarily indexd; optional for mcpd).
pub const DEFAULT_IDLE_EVICT_AFTER: Duration = Duration::from_secs(300);

/// Resolve the failure-cooldown from [`FAILURE_COOLDOWN_ENV`] (seconds, `u64`).
///
/// Missing / empty / non-numeric / zero → [`DEFAULT_FAILURE_COOLDOWN`].
pub fn failure_cooldown_from_env() -> Duration {
    duration_from_env_secs(FAILURE_COOLDOWN_ENV, DEFAULT_FAILURE_COOLDOWN)
}

/// Resolve the idle-eviction interval from [`IDLE_EVICT_ENV`] (seconds, `u64`).
///
/// Missing / empty / non-numeric / zero → [`DEFAULT_IDLE_EVICT_AFTER`].
pub fn idle_evict_after_from_env() -> Duration {
    duration_from_env_secs(IDLE_EVICT_ENV, DEFAULT_IDLE_EVICT_AFTER)
}

fn duration_from_env_secs(var: &str, default: Duration) -> Duration {
    match std::env::var(var) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return default;
            }
            match trimmed.parse::<u64>() {
                Ok(0) | Err(_) => default,
                Ok(secs) => Duration::from_secs(secs),
            }
        }
        Err(_) => default,
    }
}

// ---------------------------------------------------------------------------
// Errors / outcomes
// ---------------------------------------------------------------------------

/// Why a [`ModelSlot::borrow_or_load`] call could not yield an embedder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DemandError {
    /// A previous load failed and the cooldown has not yet elapsed.
    Cooldown {
        /// Instant at which a retry becomes allowed.
        until: Instant,
        /// Human-readable message from the last failure.
        message: String,
    },
    /// The factory returned an error (also recorded as Failed).
    LoadFailed {
        /// Human-readable factory error.
        message: String,
    },
}

impl fmt::Display for DemandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DemandError::Cooldown { until, message } => {
                write!(f, "model load in cooldown until {until:?}: {message}")
            }
            DemandError::LoadFailed { message } => {
                write!(f, "model load failed: {message}")
            }
        }
    }
}

impl std::error::Error for DemandError {}

/// Outcome of [`ModelSlot::try_idle_evict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictOutcome {
    /// Session released; slot is now Empty.
    Evicted,
    /// Nothing was resident (already Empty / Failed / Loading).
    AlreadyEmpty,
    /// At least one [`ModelBorrow`] is still live; eviction refused.
    InFlight {
        /// Current in-flight borrow count.
        count: usize,
    },
    /// Session is Ready but the idle interval has not elapsed.
    NotIdle {
        /// Time since `last_used`.
        idle_for: Duration,
        /// Configured idle threshold.
        need: Duration,
    },
}

impl fmt::Display for EvictOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvictOutcome::Evicted => write!(f, "model session evicted"),
            EvictOutcome::AlreadyEmpty => write!(f, "model slot already empty"),
            EvictOutcome::InFlight { count } => {
                write!(f, "refuse eviction: {count} in-flight borrow(s)")
            }
            EvictOutcome::NotIdle { idle_for, need } => {
                write!(f, "not idle yet ({idle_for:?} < {need:?})")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

enum SlotState {
    Empty,
    Loading {
        /// Diagnostic waiter count (condvar is the real coordination).
        waiters: usize,
    },
    Ready {
        embedder: Arc<dyn Embedder>,
        in_flight_count: usize,
        last_used: Instant,
    },
    Failed {
        until: Instant,
        message: String,
    },
}

struct SlotInner {
    state: SlotState,
}

// ---------------------------------------------------------------------------
// ModelSlot
// ---------------------------------------------------------------------------

/// Lazy, single-flight, cooldown-aware holder for a shared [`Embedder`].
///
/// Construct via [`ModelSlot::empty`] or [`ModelSlot::from_optional`]. All
/// methods are `&self` and are safe for concurrent callers.
pub struct ModelSlot {
    inner: Mutex<SlotInner>,
    cv: Condvar,
}

impl fmt::Debug for ModelSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelSlot")
            .field("loaded", &self.is_loaded())
            .field("in_flight", &self.in_flight_count())
            .finish()
    }
}

impl Default for ModelSlot {
    fn default() -> Self {
        Self::empty()
    }
}

impl ModelSlot {
    /// Empty slot: zero session resident. Demand loads via
    /// [`borrow_or_load`](ModelSlot::borrow_or_load).
    pub fn empty() -> Self {
        Self {
            inner: Mutex::new(SlotInner {
                state: SlotState::Empty,
            }),
            cv: Condvar::new(),
        }
    }

    /// Seed from an optional embedder: `Some` → Ready, `None` → Empty.
    ///
    /// Used by Eager open (after a best-effort `build_embedder`) and by tests
    /// that inject a deterministic backend.
    pub fn from_optional(embedder: Option<Box<dyn Embedder>>) -> Self {
        match embedder {
            Some(e) => Self::ready(e),
            None => Self::empty(),
        }
    }

    /// Ready slot pre-seeded with an already-built embedder.
    pub fn ready(embedder: Box<dyn Embedder>) -> Self {
        let emb: Arc<dyn Embedder> = Arc::from(embedder);
        Self {
            inner: Mutex::new(SlotInner {
                state: SlotState::Ready {
                    embedder: emb,
                    in_flight_count: 0,
                    last_used: Instant::now(),
                },
            }),
            cv: Condvar::new(),
        }
    }

    /// `true` iff a session is currently resident. Does **not** trigger a load.
    pub fn is_loaded(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        matches!(guard.state, SlotState::Ready { .. })
    }

    /// Alias for [`is_loaded`](ModelSlot::is_loaded) — StoreEngine / tests.
    pub fn is_ready(&self) -> bool {
        self.is_loaded()
    }

    /// Current in-flight borrow count (`0` when not Ready).
    pub fn in_flight_count(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.state {
            SlotState::Ready {
                in_flight_count, ..
            } => in_flight_count,
            _ => 0,
        }
    }

    /// Last-used timestamp when Ready; `None` otherwise.
    pub fn last_used(&self) -> Option<Instant> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.state {
            SlotState::Ready { last_used, .. } => Some(last_used),
            _ => None,
        }
    }

    /// Non-loading borrow: `Some` only when already Ready. Never starts a load
    /// and never waits on Loading — waiters that need the session should use
    /// [`borrow_or_load`](ModelSlot::borrow_or_load).
    pub fn try_borrow(&self) -> Option<ModelBorrow<'_>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &mut guard.state {
            SlotState::Ready {
                embedder,
                in_flight_count,
                last_used,
            } => {
                *in_flight_count += 1;
                *last_used = Instant::now();
                let emb = Arc::clone(embedder);
                Some(ModelBorrow {
                    slot: self,
                    embedder: emb,
                    released: false,
                })
            }
            _ => None,
        }
    }

    /// Demand the embedder: load (single-flight) if needed, then borrow.
    ///
    /// * Ready → increment in-flight and return.
    /// * Loading → wait for the in-flight load, then re-check.
    /// * Failed within cooldown → [`DemandError::Cooldown`].
    /// * Failed past cooldown / Empty → claim Loading, run `factory` outside
    ///   the lock, publish Ready or Failed, wake waiters.
    ///
    /// Concurrent callers coalesce into one factory call and observe the same
    /// outcome.
    pub fn borrow_or_load<F>(
        &self,
        cooldown: Duration,
        factory: F,
    ) -> std::result::Result<ModelBorrow<'_>, DemandError>
    where
        F: FnOnce() -> Result<Box<dyn Embedder>>,
    {
        // `factory` is FnOnce — we only invoke it if we become the loader.
        // Waiters never need it. Wrap in Option so we can take it once.
        let mut factory = Some(factory);

        loop {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match &mut guard.state {
                SlotState::Ready {
                    embedder,
                    in_flight_count,
                    last_used,
                } => {
                    *in_flight_count += 1;
                    *last_used = Instant::now();
                    let emb = Arc::clone(embedder);
                    return Ok(ModelBorrow {
                        slot: self,
                        embedder: emb,
                        released: false,
                    });
                }
                SlotState::Failed { until, message } => {
                    let now = Instant::now();
                    if now < *until {
                        return Err(DemandError::Cooldown {
                            until: *until,
                            message: message.clone(),
                        });
                    }
                    // Cooldown elapsed → reset to Empty and claim load below.
                    guard.state = SlotState::Empty;
                    // Re-check as Empty on next iteration.
                    drop(guard);
                    continue;
                }
                SlotState::Loading { waiters } => {
                    *waiters = waiters.saturating_add(1);
                    let waited = self
                        .cv
                        .wait_while(guard, |g| matches!(g.state, SlotState::Loading { .. }))
                        .unwrap_or_else(|e| e.into_inner());
                    drop(waited);
                    continue;
                }
                SlotState::Empty => {
                    // Claim the single-flight load.
                    guard.state = SlotState::Loading { waiters: 0 };
                    drop(guard);

                    let factory = factory.take().expect(
                        "ModelSlot::borrow_or_load: factory already consumed; \
                         only the loader thread should reach Empty claim",
                    );
                    return self.run_factory_and_borrow(cooldown, factory);
                }
            }
        }
    }

    fn run_factory_and_borrow<F>(
        &self,
        cooldown: Duration,
        factory: F,
    ) -> std::result::Result<ModelBorrow<'_>, DemandError>
    where
        F: FnOnce() -> Result<Box<dyn Embedder>>,
    {
        let result = factory();

        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(embedder) => {
                let emb: Arc<dyn Embedder> = Arc::from(embedder);
                guard.state = SlotState::Ready {
                    embedder: Arc::clone(&emb),
                    in_flight_count: 1,
                    last_used: Instant::now(),
                };
                self.cv.notify_all();
                Ok(ModelBorrow {
                    slot: self,
                    embedder: emb,
                    released: false,
                })
            }
            Err(err) => {
                let message = err.to_string();
                let until = Instant::now() + cooldown;
                guard.state = SlotState::Failed {
                    until,
                    message: message.clone(),
                };
                self.cv.notify_all();
                Err(DemandError::LoadFailed { message })
            }
        }
    }

    /// Best-effort warm: load via `factory` without retaining a long-lived
    /// borrow. Used by Eager open. Failures leave Failed+cooldown.
    pub fn warm_with<F>(
        &self,
        cooldown: Duration,
        factory: F,
    ) -> std::result::Result<(), DemandError>
    where
        F: FnOnce() -> Result<Box<dyn Embedder>>,
    {
        let borrow = self.borrow_or_load(cooldown, factory)?;
        drop(borrow);
        Ok(())
    }

    /// Attempt idle eviction of a Ready session.
    ///
    /// * Refuses while `in_flight_count > 0` ([`EvictOutcome::InFlight`]).
    /// * Refuses while `now - last_used < idle_after` ([`EvictOutcome::NotIdle`]).
    /// * On success transitions Ready → Empty ([`EvictOutcome::Evicted`]).
    /// * Already Empty/Failed/Loading → [`EvictOutcome::AlreadyEmpty`].
    pub fn try_idle_evict(&self, idle_after: Duration) -> EvictOutcome {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &guard.state {
            SlotState::Ready {
                in_flight_count,
                last_used,
                ..
            } => {
                if *in_flight_count > 0 {
                    return EvictOutcome::InFlight {
                        count: *in_flight_count,
                    };
                }
                let idle_for = last_used.elapsed();
                if idle_for < idle_after {
                    return EvictOutcome::NotIdle {
                        idle_for,
                        need: idle_after,
                    };
                }
                guard.state = SlotState::Empty;
                EvictOutcome::Evicted
            }
            SlotState::Empty | SlotState::Failed { .. } | SlotState::Loading { .. } => {
                EvictOutcome::AlreadyEmpty
            }
        }
    }

    /// Force-evict if no borrowers (ignores idle interval). Used by tests.
    pub fn try_evict(&self) -> EvictOutcome {
        self.try_idle_evict(Duration::ZERO)
    }

    fn release_borrow(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let SlotState::Ready {
            in_flight_count,
            last_used,
            ..
        } = &mut guard.state
        {
            *in_flight_count = in_flight_count.saturating_sub(1);
            *last_used = Instant::now();
        }
    }
}

// ---------------------------------------------------------------------------
// ModelBorrow
// ---------------------------------------------------------------------------

/// RAII borrow of a ready embedder. Decrements `in_flight_count` on drop.
pub struct ModelBorrow<'a> {
    slot: &'a ModelSlot,
    embedder: Arc<dyn Embedder>,
    released: bool,
}

impl fmt::Debug for ModelBorrow<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelBorrow")
            .field("released", &self.released)
            .field("dim", &self.embedder.embedding_dim())
            .finish_non_exhaustive()
    }
}

impl ModelBorrow<'_> {
    /// Shared reference to the underlying embedder.
    pub fn embedder(&self) -> &dyn Embedder {
        self.embedder.as_ref()
    }

    /// Shared `Arc` handle (cheap clone) for callers that need ownership.
    pub fn arc(&self) -> Arc<dyn Embedder> {
        Arc::clone(&self.embedder)
    }
}

impl std::ops::Deref for ModelBorrow<'_> {
    type Target = dyn Embedder;

    fn deref(&self) -> &Self::Target {
        self.embedder.as_ref()
    }
}

impl Drop for ModelBorrow<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.released = true;
            self.slot.release_borrow();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StubEmbedder;
    use cognis_core::CognisError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    fn ok_factory(dim: usize) -> impl FnOnce() -> Result<Box<dyn Embedder>> {
        move || Ok(Box::new(StubEmbedder::new(dim)) as Box<dyn Embedder>)
    }

    #[test]
    fn empty_starts_unloaded() {
        let slot = ModelSlot::empty();
        assert!(!slot.is_loaded());
        assert!(slot.try_borrow().is_none());
        assert_eq!(slot.in_flight_count(), 0);
    }

    #[test]
    fn from_optional_none_is_empty() {
        let slot = ModelSlot::from_optional(None);
        assert!(!slot.is_loaded());
    }

    #[test]
    fn from_optional_some_is_ready() {
        let slot = ModelSlot::from_optional(Some(Box::new(StubEmbedder::new(4))));
        assert!(slot.is_loaded());
        let b = slot.try_borrow().expect("ready");
        assert_eq!(b.embedder().embedding_dim(), 4);
        assert_eq!(slot.in_flight_count(), 1);
        drop(b);
        assert_eq!(slot.in_flight_count(), 0);
    }

    #[test]
    fn borrow_or_load_single_flights() {
        let calls = Arc::new(AtomicUsize::new(0));
        let slot = Arc::new(ModelSlot::empty());
        let cooldown = Duration::from_secs(5);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = Arc::clone(&slot);
            let c = Arc::clone(&calls);
            handles.push(thread::spawn(move || {
                let b = s
                    .borrow_or_load(cooldown, || {
                        c.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(40));
                        Ok(Box::new(StubEmbedder::new(2)) as Box<dyn Embedder>)
                    })
                    .expect("borrow");
                assert_eq!(b.embedder().embedding_dim(), 2);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "all concurrent demanders must share one factory call"
        );
        assert!(slot.is_loaded());
        assert_eq!(slot.in_flight_count(), 0);
    }

    #[test]
    fn failure_enters_cooldown_then_allows_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let slot = ModelSlot::empty();
        let cooldown = Duration::from_millis(40);

        let err = slot
            .borrow_or_load(cooldown, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(CognisError::Model("boom".into()))
            })
            .unwrap_err();
        assert!(matches!(err, DemandError::LoadFailed { .. }), "{err:?}");

        // Immediate retry suppressed.
        let err2 = slot
            .borrow_or_load(cooldown, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(StubEmbedder::new(1)) as Box<dyn Embedder>)
            })
            .unwrap_err();
        assert!(matches!(err2, DemandError::Cooldown { .. }), "{err2:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        thread::sleep(Duration::from_millis(50));
        let b = slot
            .borrow_or_load(cooldown, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(StubEmbedder::new(1)) as Box<dyn Embedder>)
            })
            .expect("retry");
        assert_eq!(b.embedder().embedding_dim(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn concurrent_waiters_share_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let slot = Arc::new(ModelSlot::empty());
        let cooldown = Duration::from_secs(5);

        let mut handles = Vec::new();
        for _ in 0..6 {
            let s = Arc::clone(&slot);
            let c = Arc::clone(&calls);
            handles.push(thread::spawn(move || {
                s.borrow_or_load(cooldown, || {
                    c.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(40));
                    Err(CognisError::Model("shared-fail".into()))
                })
                .unwrap_err()
            }));
        }
        let mut saw_load_failed = 0;
        let mut saw_cooldown = 0;
        for h in handles {
            match h.join().unwrap() {
                DemandError::LoadFailed { .. } => saw_load_failed += 1,
                DemandError::Cooldown { .. } => saw_cooldown += 1,
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(saw_load_failed + saw_cooldown, 6);
        assert!(saw_load_failed >= 1);
    }

    #[test]
    fn try_idle_evict_refused_while_in_flight() {
        let slot = ModelSlot::ready(Box::new(StubEmbedder::new(4)));
        let b = slot.try_borrow().unwrap();
        match slot.try_idle_evict(Duration::ZERO) {
            EvictOutcome::InFlight { count } => assert_eq!(count, 1),
            other => panic!("expected InFlight, got {other:?}"),
        }
        drop(b);
        assert_eq!(slot.try_idle_evict(Duration::ZERO), EvictOutcome::Evicted);
        assert!(!slot.is_loaded());
    }

    #[test]
    fn try_idle_evict_respects_idle_interval() {
        let slot = ModelSlot::ready(Box::new(StubEmbedder::new(4)));
        // Touch last_used.
        drop(slot.try_borrow().unwrap());
        match slot.try_idle_evict(Duration::from_secs(60)) {
            EvictOutcome::NotIdle { .. } => {}
            other => panic!("expected NotIdle, got {other:?}"),
        }
        assert!(slot.is_loaded());
    }

    #[test]
    fn warm_with_is_best_effort() {
        let slot = ModelSlot::empty();
        slot.warm_with(Duration::from_secs(1), ok_factory(7))
            .unwrap();
        assert!(slot.is_loaded());
        assert_eq!(slot.in_flight_count(), 0);
    }

    #[test]
    fn duration_from_env_helpers_default() {
        // Don't assert on ambient env; just ensure the helpers return a
        // positive duration (default or override).
        assert!(failure_cooldown_from_env() > Duration::ZERO);
        assert!(idle_evict_after_from_env() > Duration::ZERO);
    }
}
