//! Property-based + unit tests for the lazy single-flight model lifecycle.
//!
//! Feature: mcp-process-ram-duplication
//! **Property 6: Bug Condition** — StoreEngine lazy single-flight with cooldown
//!
//! **Validates: Requirements 2.5**
//!
//! _For any_ concurrent demand schedule with injected load-failure outcomes,
//! [`ModelSlot`] keeps zero session resident before demand, coalesces concurrent
//! first demand into one factory call, makes every waiter observe the same
//! success or failure, suppresses retries for a bounded cooldown after failure,
//! allows retry after the cooldown, and refuses eviction while a borrow is
//! in flight.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::CognisError;
use cognis_embed::{DemandError, Embedder, EvictOutcome, ModelSlot, StubEmbedder};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ok_stub(dim: usize) -> Box<dyn Embedder> {
    Box::new(StubEmbedder::new(dim))
}

/// One wave of concurrent demand against a shared slot.
///
/// Returns `(factory_calls_during_wave, outcomes)`.
fn concurrent_demand(
    slot: &Arc<ModelSlot>,
    n: usize,
    cooldown: Duration,
    load_counter: &Arc<AtomicUsize>,
    succeed: bool,
    load_sleep: Duration,
) -> (usize, Vec<Result<(), DemandError>>) {
    let before = load_counter.load(Ordering::SeqCst);
    let start = Arc::new(Barrier::new(n));
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let s = Arc::clone(slot);
        let c = Arc::clone(load_counter);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            let result = s.borrow_or_load(cooldown, || {
                c.fetch_add(1, Ordering::SeqCst);
                if load_sleep > Duration::ZERO {
                    thread::sleep(load_sleep);
                }
                if succeed {
                    Ok(ok_stub(4))
                } else {
                    Err(CognisError::Model("injected-fail".into()))
                }
            });
            match result {
                Ok(borrow) => {
                    // Hold the borrow briefly so concurrent waves can observe
                    // in-flight > 0 when relevant, then drop.
                    let _dim = borrow.embedder().embedding_dim();
                    drop(borrow);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }));
    }
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    let after = load_counter.load(Ordering::SeqCst);
    (after - before, outcomes)
}

// ---------------------------------------------------------------------------
// Property 6
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(80))]

    // Feature: mcp-process-ram-duplication, Property 6: Bug Condition —
    // StoreEngine lazy single-flight with cooldown
    // **Validates: Requirements 2.5**
    //
    // For random concurrent demand + injected load-failure schedules:
    // * zero session resident before first demand
    // * at most one load is in flight (exactly one factory call per wave)
    // * all waiters share the outcome
    // * retries respect the cooldown
    #[test]
    fn single_flight_cooldown_and_shared_outcome(
        n_threads in 2usize..12,
        fail_waves in 0usize..3,
        cooldown_ms in 200u64..350,
    ) {
        let slot = Arc::new(ModelSlot::empty());
        let loads = Arc::new(AtomicUsize::new(0));
        let cooldown = Duration::from_millis(cooldown_ms);
        let load_sleep = Duration::from_millis(30);

        // Zero ONNX / session resident before demand.
        prop_assert!(!slot.is_loaded());
        prop_assert_eq!(slot.in_flight_count(), 0);
        prop_assert!(slot.try_borrow().is_none());

        // Failure waves: each concurrent wave shares exactly one factory call
        // and every waiter observes a failure-class outcome.
        for _ in 0..fail_waves {
            let (calls, outcomes) = concurrent_demand(
                &slot, n_threads, cooldown, &loads, false, load_sleep,
            );
            prop_assert_eq!(
                calls, 1,
                "concurrent failure demand must single-flight (calls={})",
                calls
            );
            prop_assert!(!slot.is_loaded());
            prop_assert_eq!(outcomes.len(), n_threads);
            for o in &outcomes {
                prop_assert!(
                    matches!(o, Err(DemandError::LoadFailed { .. }) | Err(DemandError::Cooldown { .. })),
                    "waiters must share a failure outcome, got {o:?}"
                );
            }
            // At least the loader sees LoadFailed.
            prop_assert!(
                outcomes.iter().any(|o| matches!(o, Err(DemandError::LoadFailed { .. }))),
                "loader must surface LoadFailed"
            );

            // Immediate retry within cooldown: zero additional factory calls.
            let before = loads.load(Ordering::SeqCst);
            let err = slot
                .borrow_or_load(cooldown, || {
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(ok_stub(4))
                })
                .unwrap_err();
            prop_assert!(
                matches!(err, DemandError::Cooldown { .. }),
                "retry inside cooldown must be suppressed: {err:?}"
            );
            prop_assert_eq!(
                loads.load(Ordering::SeqCst),
                before,
                "cooldown must not invoke the factory"
            );

            // Wait for cooldown expiry before the next wave / success wave.
            thread::sleep(cooldown + Duration::from_millis(5));
        }

        // Success wave: single-flight load, all waiters Ok, slot Ready.
        let (calls, outcomes) = concurrent_demand(
            &slot, n_threads, cooldown, &loads, true, load_sleep,
        );
        prop_assert_eq!(
            calls, 1,
            "concurrent success demand must single-flight (calls={})",
            calls
        );
        prop_assert!(slot.is_loaded());
        prop_assert_eq!(slot.in_flight_count(), 0);
        for o in &outcomes {
            prop_assert!(o.is_ok(), "all waiters share Ok, got {o:?}");
        }

        // Already-Ready concurrent demand never reloads.
        let before = loads.load(Ordering::SeqCst);
        let (calls2, outcomes2) = concurrent_demand(
            &slot, n_threads, cooldown, &loads, true, Duration::ZERO,
        );
        prop_assert_eq!(calls2, 0, "Ready slot must not re-run the factory");
        prop_assert_eq!(loads.load(Ordering::SeqCst), before);
        for o in &outcomes2 {
            prop_assert!(o.is_ok());
        }
    }
}

// ---------------------------------------------------------------------------
// Unit: ModelSlot state machine (Requirement 2.5)
// ---------------------------------------------------------------------------

#[test]
fn empty_has_zero_session_before_demand() {
    let slot = ModelSlot::empty();
    assert!(!slot.is_loaded(), "zero session resident before demand");
    assert!(!slot.is_ready());
    assert_eq!(slot.in_flight_count(), 0);
    assert!(slot.last_used().is_none());
    assert!(slot.try_borrow().is_none());
    assert_eq!(slot.try_evict(), EvictOutcome::AlreadyEmpty);
}

#[test]
fn empty_to_loading_to_ready_state_machine() {
    let slot = ModelSlot::empty();
    let b = slot
        .borrow_or_load(Duration::from_secs(1), || Ok(ok_stub(8)))
        .expect("load");
    assert!(slot.is_loaded());
    assert_eq!(slot.in_flight_count(), 1);
    assert_eq!(b.embedder().embedding_dim(), 8);
    drop(b);
    assert_eq!(slot.in_flight_count(), 0);
    assert!(slot.is_ready());
}

#[test]
fn empty_to_loading_to_failed_then_cooldown_expiry() {
    let slot = ModelSlot::empty();
    let cooldown = Duration::from_millis(30);
    let err = slot
        .borrow_or_load(cooldown, || Err(CognisError::Model("boom".into())))
        .unwrap_err();
    assert!(matches!(err, DemandError::LoadFailed { .. }));
    assert!(!slot.is_loaded());

    // Still in cooldown.
    let t0 = Instant::now();
    let err2 = slot
        .borrow_or_load(cooldown, || Ok(ok_stub(1)))
        .unwrap_err();
    assert!(matches!(err2, DemandError::Cooldown { .. }));
    assert!(t0.elapsed() < cooldown);

    thread::sleep(cooldown + Duration::from_millis(10));
    let b = slot
        .borrow_or_load(cooldown, || Ok(ok_stub(3)))
        .expect("retry after cooldown");
    assert_eq!(b.embedder().embedding_dim(), 3);
    drop(b);
    assert!(slot.is_loaded());
}

#[test]
fn in_flight_refuse_evict_and_idle_evict_when_clear() {
    let slot = ModelSlot::ready(ok_stub(4));
    let b = slot.try_borrow().expect("ready");
    match slot.try_idle_evict(Duration::ZERO) {
        EvictOutcome::InFlight { count } => assert_eq!(count, 1),
        other => panic!("expected InFlight, got {other:?}"),
    }
    // Idle interval not met when last_used is fresh.
    drop(b);
    match slot.try_idle_evict(Duration::from_secs(60)) {
        EvictOutcome::NotIdle { .. } => {}
        other => panic!("expected NotIdle, got {other:?}"),
    }
    // Idle-evict with no pending work (zero idle threshold).
    assert_eq!(slot.try_idle_evict(Duration::ZERO), EvictOutcome::Evicted);
    assert!(!slot.is_loaded(), "session released after idle-evict");
    assert_eq!(
        slot.try_idle_evict(Duration::ZERO),
        EvictOutcome::AlreadyEmpty
    );
}

#[test]
fn concurrent_success_waiters_share_same_ready_session() {
    let slot = Arc::new(ModelSlot::empty());
    let loads = Arc::new(AtomicUsize::new(0));
    let dims = Arc::new(AtomicUsize::new(0));
    let cooldown = Duration::from_secs(2);

    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = Arc::clone(&slot);
        let c = Arc::clone(&loads);
        let d = Arc::clone(&dims);
        handles.push(thread::spawn(move || {
            let b = s
                .borrow_or_load(cooldown, || {
                    c.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(30));
                    Ok(ok_stub(11))
                })
                .expect("borrow");
            d.fetch_add(b.embedder().embedding_dim(), Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(dims.load(Ordering::SeqCst), 10 * 11);
    assert!(slot.is_loaded());
    assert_eq!(slot.in_flight_count(), 0);
}
