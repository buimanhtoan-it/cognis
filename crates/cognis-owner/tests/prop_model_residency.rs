//! Property 25: Model residency is demand-driven and observable.
//!
//! Under the lazy policy no load occurs before first demand; a resident session
//! serves with zero reloads; concurrent demand after a release triggers at most
//! one load; release is refused unless idle, the embedding queue is empty, and
//! no tool call is in flight; a failed load is not retried inside the cooldown;
//! reported status residency equals the slot's actual state.
//!
//! **Validates: Requirements 20.1, 20.2, 20.3, 20.4, 20.5, 20.6, 20.7**

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use cognis_core::{ParseStatus, SemanticWarmPolicy, Symbol, SymbolKind};
use cognis_embed::{DemandError, Embedder, EvictOutcome, ModelSlot, StubEmbedder};
use cognis_owner::{become_owner_deferred, owner_status_path, OwnerSession, OwnerStatus};
use cognis_store::{
    Database, FileCacheRow, IndexPublisher, PublishBatch, PublishReason, QueuedSymbol,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::Value;

#[derive(Debug, Clone)]
enum Step {
    Demand { workers: usize },
    Evict,
    Fail,
}

fn arb_steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            (1usize..5).prop_map(|workers| Step::Demand { workers }),
            Just(Step::Evict),
            Just(Step::Fail),
        ],
        1..8,
    )
}

fn ok_stub() -> Box<dyn Embedder> {
    Box::new(StubEmbedder::new(4))
}

fn concurrent_demand(
    slot: &Arc<ModelSlot>,
    n: usize,
    cooldown: Duration,
    loads: &Arc<AtomicUsize>,
    succeed: bool,
) -> (usize, Vec<std::result::Result<(), DemandError>>) {
    let before = loads.load(Ordering::SeqCst);
    let start = Arc::new(Barrier::new(n));
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let slot = Arc::clone(slot);
        let loads = Arc::clone(loads);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            start.wait();
            let result = slot.borrow_or_load(cooldown, || {
                loads.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                if succeed {
                    Ok(ok_stub())
                } else {
                    Err(cognis_core::CognisError::Model("injected-fail".into()))
                }
            });
            match result {
                Ok(borrow) => {
                    let _ = borrow.embedding_dim();
                    drop(borrow);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }));
    }
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("demand worker"))
        .collect();
    let calls = loads.load(Ordering::SeqCst) - before;
    (calls, outcomes)
}

fn read_status(root: &Path) -> Value {
    let text = fs::read_to_string(owner_status_path(root)).expect("owner-status.json");
    serde_json::from_str(&text).expect("owner-status JSON")
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unique_repo() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p25-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("create workspace");
    dir
}

fn write_owner_config(repo: &Path) {
    let cognis = repo.join(".cognis");
    fs::create_dir_all(&cognis).expect("create .cognis");
    fs::write(
        cognis.join("config.yaml"),
        "owner:\n  lease_ttl_s: 1\n  watcher_debounce_ms: 1\n\
         embedder:\n  backend: stub\n  model: stub\n  dim: 8\n  batch_size: 4\n",
    )
    .expect("write config");
}

fn enqueue_pending(db: &mut Database, tag: usize) {
    let id = format!("p25-pending-{tag}");
    let path = "src/pending.rs";
    let symbol = Symbol {
        id: id.clone(),
        kind: SymbolKind::Function,
        name: id.clone(),
        qualified_name: format!("mod.{id}"),
        language: "rust".into(),
        module: "mod".into(),
        file_path: path.into(),
        line_start: 1,
        line_end: 2,
        signature: None,
        docstring: None,
        content_hash: format!("hash-{id}"),
        body_excerpt: Some("fn x() {}".into()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: Vec::new(),
        updated_at: 1,
    };
    let file = FileCacheRow {
        path: path.into(),
        language: "rust".into(),
        size_bytes: 8,
        content_hash: format!("hash-{id}"),
        parsed_at: 1,
        parse_status: ParseStatus::Ok,
    };
    let mut batch = PublishBatch::empty(PublishReason::Incremental);
    batch.symbol_upserts.push(symbol);
    batch.file_rows.push(file);
    batch.queue_enqueue.push(QueuedSymbol {
        symbol_id: id,
        file_path: path.into(),
    });
    db.publish(&batch).expect("enqueue pending vector");
}

fn acquire_owner(repo: &Path, policy: SemanticWarmPolicy) -> cognis_owner::OwnedWorkspace {
    match become_owner_deferred(repo, policy).expect("become owner") {
        OwnerSession::Acquired(owned) => owned,
        OwnerSession::Attached(_) => panic!("vacant workspace must be acquirable"),
    }
}

fn interpret_slot(steps: Vec<Step>) -> Result<(), TestCaseError> {
    let slot = Arc::new(ModelSlot::empty());
    let loads = Arc::new(AtomicUsize::new(0));
    let cooldown = Duration::from_secs(30);
    prop_assert!(!slot.is_loaded(), "no session resident before first demand");
    prop_assert_eq!(slot.in_flight_count(), 0);

    let mut cooling = false;
    for step in steps {
        match step {
            Step::Demand { workers } => {
                if cooling {
                    let before = loads.load(Ordering::SeqCst);
                    let err = slot
                        .borrow_or_load(cooldown, || {
                            loads.fetch_add(1, Ordering::SeqCst);
                            Ok(ok_stub())
                        })
                        .unwrap_err();
                    prop_assert!(
                        matches!(err, DemandError::Cooldown { .. }),
                        "no retry inside the cooldown, got {err:?}"
                    );
                    prop_assert_eq!(loads.load(Ordering::SeqCst), before);
                    prop_assert!(!slot.is_loaded());
                    continue;
                }
                let resident = slot.is_loaded();
                let before = loads.load(Ordering::SeqCst);
                let (calls, outcomes) = concurrent_demand(&slot, workers, cooldown, &loads, true);
                prop_assert_eq!(outcomes.len(), workers);
                for outcome in &outcomes {
                    prop_assert!(
                        outcome.is_ok(),
                        "success demand must serve, got {outcome:?}"
                    );
                }
                if resident {
                    prop_assert_eq!(calls, 0, "a resident session must serve with zero reloads");
                    prop_assert_eq!(loads.load(Ordering::SeqCst), before);
                } else {
                    prop_assert_eq!(
                        calls,
                        1,
                        "concurrent demand after empty/release must single-flight"
                    );
                }
                prop_assert!(slot.is_loaded());
                prop_assert_eq!(slot.in_flight_count(), 0);
            }
            Step::Evict => {
                let was_loaded = slot.is_loaded();
                let outcome = slot.try_idle_evict(Duration::ZERO);
                if was_loaded {
                    prop_assert_eq!(outcome, EvictOutcome::Evicted);
                    prop_assert!(!slot.is_loaded());
                } else {
                    prop_assert_eq!(outcome, EvictOutcome::AlreadyEmpty);
                    prop_assert!(!slot.is_loaded());
                }
            }
            Step::Fail => {
                if cooling {
                    let before = loads.load(Ordering::SeqCst);
                    let err = slot
                        .borrow_or_load(cooldown, || {
                            loads.fetch_add(1, Ordering::SeqCst);
                            Ok(ok_stub())
                        })
                        .unwrap_err();
                    prop_assert!(
                        matches!(err, DemandError::Cooldown { .. }),
                        "no retry inside the cooldown, got {err:?}"
                    );
                    prop_assert_eq!(loads.load(Ordering::SeqCst), before);
                    prop_assert!(!slot.is_loaded());
                    continue;
                }
                if slot.is_loaded() {
                    prop_assert_eq!(slot.try_idle_evict(Duration::ZERO), EvictOutcome::Evicted);
                }
                let before = loads.load(Ordering::SeqCst);
                let err = slot
                    .borrow_or_load(cooldown, || {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Err(cognis_core::CognisError::Model("injected-fail".into()))
                    })
                    .unwrap_err();
                prop_assert!(
                    matches!(err, DemandError::LoadFailed { .. }),
                    "first failure must be LoadFailed"
                );
                prop_assert_eq!(loads.load(Ordering::SeqCst), before + 1);
                let retry = slot
                    .borrow_or_load(cooldown, || {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(ok_stub())
                    })
                    .unwrap_err();
                prop_assert!(
                    matches!(retry, DemandError::Cooldown { .. }),
                    "failed load must not retry inside the cooldown"
                );
                prop_assert_eq!(loads.load(Ordering::SeqCst), before + 1);
                prop_assert!(!slot.is_loaded());
                cooling = true;
            }
        }
        prop_assert_eq!(
            slot.is_loaded(),
            slot.is_ready(),
            "reported residency must equal the slot's actual state"
        );
    }
    Ok(())
}

fn interpret_owner(
    lazy: bool,
    tool_in_flight: bool,
    seed_queue: bool,
) -> Result<(), TestCaseError> {
    let repo = unique_repo();
    let _cleanup = Cleanup(repo.clone());
    write_owner_config(&repo);
    let policy = if lazy {
        SemanticWarmPolicy::Lazy
    } else {
        SemanticWarmPolicy::Eager
    };
    let mut owned = acquire_owner(&repo, policy);
    let actual = owned.model_resident();
    if lazy {
        prop_assert!(!actual, "lazy policy must not load before first demand");
    } else {
        prop_assert!(actual, "eager stub warm must leave a resident session");
    }
    let status = read_status(Path::new(&owned.identity().root));
    prop_assert_eq!(
        status["modelResident"].clone(),
        serde_json::json!(actual),
        "status surface residency must equal the slot's actual state"
    );

    if seed_queue {
        let mut db = owned
            .database()
            .expect("acquired opens the database")
            .clone();
        enqueue_pending(&mut db, 1);
    }
    if tool_in_flight {
        owned.note_tool_call_start();
    }
    owned.set_idle_evict_after(Duration::ZERO);
    let before_ts = OwnerStatus::now_secs();
    let outcome = owned.try_idle_evict_model();
    let blocked = tool_in_flight || seed_queue;
    if blocked {
        prop_assert!(
            matches!(outcome, EvictOutcome::InFlight { .. }),
            "release only when idle, queue empty, and no call in flight, got {outcome:?}"
        );
        prop_assert_eq!(owned.model_resident(), actual);
    } else if actual {
        prop_assert_eq!(outcome, EvictOutcome::Evicted);
        prop_assert!(!owned.model_resident());
        let status = read_status(Path::new(&owned.identity().root));
        prop_assert_eq!(status["modelResident"].clone(), serde_json::json!(false));
        let updated = status["updatedAt"].as_f64().unwrap_or(0.0);
        prop_assert!(
            updated >= before_ts - 1.0,
            "every release must be recorded with a timestamp"
        );
    } else {
        prop_assert_eq!(outcome, EvictOutcome::AlreadyEmpty);
        prop_assert!(!owned.model_resident());
    }
    if tool_in_flight {
        owned.note_tool_call_end();
    }
    drop(owned);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 25: Model residency is demand-driven and observable.
    /// **Validates: Requirements 20.1, 20.4, 20.5, 20.6, 20.7**
    #[test]
    fn model_residency_sequence_is_demand_driven(steps in arb_steps()) {
        interpret_slot(steps)?;
    }

    /// Feature: cognis-one-toggle-engine, Property 25: Model residency is demand-driven and observable.
    /// **Validates: Requirements 20.1, 20.2, 20.3, 20.7**
    #[test]
    fn owner_residency_is_gated_and_observable(
        lazy in any::<bool>(),
        tool_in_flight in any::<bool>(),
        seed_queue in any::<bool>(),
    ) {
        interpret_owner(lazy, tool_in_flight, seed_queue)?;
    }
}
