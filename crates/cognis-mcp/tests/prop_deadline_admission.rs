//! Property 24: Deadline termination and admission.
//!
//! Every call returns complete, truncated-with-indicator, or deadline-exceeded
//! within the hard limit plus a bounded margin; no stage starts after the soft
//! limit; deadlines are independent; `source_generation` is unchanged; the
//! count of running deadlines never exceeds the concurrency cap; and every
//! capped rejection is retryable with no deadline started.
//!
//! **Validates: Requirements 19.1, 19.2, 19.3, 19.4, 19.6, 19.7, 19.8, 24.6**

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::{Hit, Result as CognisResult, Symbol};
use cognis_mcp::caps::{Caps, Deadline, StageDecision};
use cognis_mcp::jsonrpc::Request;
use cognis_mcp::server::ToolCtx;
use cognis_mcp::{is_error_envelope, McpServer, RetrievalEngine};
use cognis_store::{Database, GenerationSnapshot, IndexPublisher, PublishBatch, PublishReason};
use cognis_testgen::{arb_arrival_sequence, ArrivalSequence, StageCosts, STAGES};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{json, Value};

const WALL_MARGIN: Duration = Duration::from_millis(150);

fn isolated_db() -> (PathBuf, Database) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p24-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let db = Database::open(dir.join("deadline.db")).expect("open Property 24 database");
    (dir, db)
}

fn cleanup(dir: &Path, db: &Database) {
    db.close_thread_connection();
    let _ = std::fs::remove_dir_all(dir);
}

fn source_generation(db: &Database) -> i64 {
    db.connect()
        .expect("generation connection")
        .query_row(
            "SELECT source_generation FROM generation WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("read source_generation")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CallOutcome {
    Complete { started: usize },
    Truncated { started: usize },
    Timeout { started: usize },
}

fn expected_outcome(costs: &StageCosts, soft: Duration, hard: Duration) -> CallOutcome {
    let mut elapsed = Duration::ZERO;
    let mut started = 0usize;
    for cost in costs.millis {
        if elapsed >= hard {
            return CallOutcome::Timeout { started };
        }
        if elapsed >= soft {
            return CallOutcome::Truncated { started };
        }
        started += 1;
        elapsed += Duration::from_millis(cost);
    }
    CallOutcome::Complete { started }
}

fn run_stages(ctx: &ToolCtx<'_>, costs: &StageCosts) -> CallOutcome {
    let mut started = 0usize;
    for (index, stage) in STAGES.iter().enumerate() {
        match ctx.stage_gate(stage) {
            Err(_) => return CallOutcome::Timeout { started },
            Ok(StageDecision::Truncate) => return CallOutcome::Truncated { started },
            Ok(StageDecision::Continue) => {
                started += 1;
                ctx.add_elapsed(Duration::from_millis(costs.millis[index]));
            }
        }
    }
    CallOutcome::Complete { started }
}

fn tool_args(tool: &str) -> Value {
    match tool {
        "symbol_lookup" => json!({ "name_or_id": "alpha" }),
        "resolve_symbols" => json!({ "symbol_ids": ["alpha"] }),
        "dependency_trace" => json!({
            "symbol_id": "alpha",
            "direction": "out",
            "depth": 1,
        }),
        "retrieve_context_capsule" => json!({ "task": "alpha", "max_tokens": 800 }),
        _ => json!({ "query": "alpha", "k": 4 }),
    }
}

fn call_tool<E: RetrievalEngine>(server: &McpServer<E>, name: &str) -> Value {
    let response = server
        .handle(Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": tool_args(name) })),
        })
        .expect("tools/call response");
    response.result.expect("tools/call result")["structuredContent"].clone()
}

struct BlockingEngine {
    db: Database,
    snapshots: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl BlockingEngine {
    fn wait_until_released(&self) {
        let (lock, cv) = &*self.release;
        let mut opened = lock.lock().expect("release lock");
        while !*opened {
            opened = cv.wait(opened).expect("release wait");
        }
    }

    fn track_open(&self) {
        let current = self.snapshots.fetch_add(1, Ordering::SeqCst) + 1;
        let mut peak = self.peak.load(Ordering::SeqCst);
        while current > peak {
            match self
                .peak
                .compare_exchange_weak(peak, current, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(observed) => peak = observed,
            }
        }
        self.wait_until_released();
    }
}

impl RetrievalEngine for BlockingEngine {
    fn open_snapshot(&self) -> CognisResult<GenerationSnapshot<'_>> {
        self.track_open();
        GenerationSnapshot::open(&self.db)
    }

    fn fts_search(&self, _ctx: &ToolCtx<'_>, _query: &str, _k: usize) -> CognisResult<Vec<Hit>> {
        Ok(Vec::new())
    }

    fn semantic_search(
        &self,
        _ctx: &ToolCtx<'_>,
        _query: &str,
        _k: usize,
    ) -> CognisResult<Vec<Hit>> {
        Ok(Vec::new())
    }

    fn diffuse(
        &self,
        _ctx: &ToolCtx<'_>,
        _seeds: &[Vec<Hit>],
        _k: usize,
        _alpha: f64,
        _eps: f64,
    ) -> CognisResult<Vec<Hit>> {
        Ok(Vec::new())
    }

    fn hydrate(&self, _ctx: &ToolCtx<'_>, _ids: &[String]) -> CognisResult<Vec<Symbol>> {
        Ok(Vec::new())
    }

    fn lookup(
        &self,
        _ctx: &ToolCtx<'_>,
        _name_or_id: &str,
        _kind: Option<&str>,
    ) -> CognisResult<Option<Symbol>> {
        Ok(None)
    }

    fn dependency_trace(
        &self,
        _ctx: &ToolCtx<'_>,
        _symbol_id: &str,
        _direction: &str,
        _depth: u8,
    ) -> CognisResult<Vec<Hit>> {
        Ok(Vec::new())
    }
}

fn wait_for_admitted(snapshots: &AtomicUsize, want: usize, budget: Duration) -> usize {
    let start = Instant::now();
    loop {
        let got = snapshots.load(Ordering::SeqCst);
        if got >= want || start.elapsed() >= budget {
            return got;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn assert_admission(seq: &ArrivalSequence, db: &Database) -> Result<(), TestCaseError> {
    let snapshots = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let engine = BlockingEngine {
        db: db.clone(),
        snapshots: Arc::clone(&snapshots),
        peak: Arc::clone(&peak),
        release: Arc::clone(&release),
    };
    let caps = Caps {
        max_concurrency: seq.concurrency_cap,
        soft_timeout: Duration::from_secs(5),
        hard_timeout: Duration::from_secs(30),
        ..Caps::default()
    };
    let server = Arc::new(McpServer::new(engine).with_caps(caps));
    let n = seq.arrivals.len();
    let barrier = Arc::new(Barrier::new(n + 1));
    let mut handles = Vec::new();
    for arrival in &seq.arrivals {
        let server = Arc::clone(&server);
        let barrier = Arc::clone(&barrier);
        let tool = arrival.tool;
        handles.push(thread::spawn(move || {
            barrier.wait();
            call_tool(server.as_ref(), tool)
        }));
    }
    barrier.wait();
    let want = seq.max_running_deadlines();
    let admitted = wait_for_admitted(&snapshots, want, Duration::from_secs(2));
    prop_assert!(
        admitted <= want,
        "running deadlines {} exceeded cap-derived bound {}",
        admitted,
        want
    );
    if seq.concurrency_cap != 0 {
        prop_assert!(
            server.in_flight_calls() <= seq.concurrency_cap,
            "in-flight {} exceeded cap {}",
            server.in_flight_calls(),
            seq.concurrency_cap
        );
    }

    {
        let (lock, cv) = &*release;
        let mut opened = lock.lock().expect("release lock");
        *opened = true;
        cv.notify_all();
    }

    let mut rejected = 0usize;
    let mut accepted = 0usize;
    for handle in handles {
        let payload = handle.join().expect("worker thread");
        if is_error_envelope(&payload)
            && payload
                .get("error")
                .and_then(|error| error.get("retryable"))
                .and_then(Value::as_bool)
                == Some(true)
            && payload.get("source_generation").is_none()
        {
            rejected += 1;
            prop_assert!(
                payload.get("truncated").is_none(),
                "capped rejection must not start a truncated deadline"
            );
        } else {
            accepted += 1;
        }
    }
    if seq.concurrency_cap != 0 && seq.exceeds_cap() {
        prop_assert!(
            rejected >= seq.arrivals.len() - seq.concurrency_cap,
            "over-subscribed sequence must reject overflow calls (rejected={rejected}, n={}, cap={})",
            seq.arrivals.len(),
            seq.concurrency_cap
        );
        prop_assert!(
            accepted <= seq.concurrency_cap,
            "accepted {} calls over cap {}",
            accepted,
            seq.concurrency_cap
        );
    }
    if seq.concurrency_cap == 0 {
        prop_assert_eq!(rejected, 0, "unbounded cap must admit every arrival");
    }
    let _ = peak;
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 24: Deadline termination and admission.
    /// **Validates: Requirements 19.1, 19.2, 19.3, 19.4, 19.6, 19.7, 19.8, 24.6**
    #[test]
    fn deadline_termination_and_admission(seq in arb_arrival_sequence()) {
        let (dir, mut db) = isolated_db();
        db.publish(&PublishBatch::empty(PublishReason::Initial))
            .expect("seed generation");
        let generation = source_generation(&db);
        let caps = Caps {
            max_concurrency: seq.concurrency_cap,
            soft_timeout: seq.soft_limit,
            hard_timeout: seq.hard_limit,
            ..Caps::default()
        };

        for arrival in &seq.arrivals {
            let wall_start = Instant::now();
            let snapshot = GenerationSnapshot::open(&db).expect("pin snapshot");
            let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
            let expected = expected_outcome(&arrival.costs, seq.soft_limit, seq.hard_limit);
            let observed = run_stages(&ctx, &arrival.costs);
            prop_assert_eq!(&observed, &expected);
            match &observed {
                CallOutcome::Complete { started } => {
                    prop_assert_eq!(*started, STAGES.len());
                    prop_assert!(!ctx.truncated());
                }
                CallOutcome::Truncated { started } => {
                    prop_assert!(ctx.truncated(), "soft limit must set the truncation indicator");
                    prop_assert!(*started < STAGES.len(), "no stage starts after the soft limit");
                }
                CallOutcome::Timeout { started } => {
                    prop_assert!(*started < STAGES.len());
                }
            }
            prop_assert!(
                wall_start.elapsed() <= seq.hard_limit.saturating_add(WALL_MARGIN),
                "call exceeded hard limit plus margin: elapsed={:?} hard={:?}",
                wall_start.elapsed(),
                seq.hard_limit
            );
            prop_assert_eq!(
                ctx.snapshot.source_generation(),
                generation,
                "tool calls must not mutate source_generation"
            );
        }

        if seq.arrivals.len() >= 2 {
            let first = &seq.arrivals[0];
            let second = &seq.arrivals[1];
            let snapshot = GenerationSnapshot::open(&db).expect("independent first snapshot");
            let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
            let _ = run_stages(&ctx, &first.costs);
            drop(ctx);
            let snapshot = GenerationSnapshot::open(&db).expect("independent second snapshot");
            let ctx = ToolCtx::new(snapshot, Deadline::start(&caps), &caps);
            let observed = run_stages(&ctx, &second.costs);
            prop_assert_eq!(
                observed,
                expected_outcome(&second.costs, seq.soft_limit, seq.hard_limit),
                "each admitted call's deadline is independent of every other"
            );
        }

        prop_assert_eq!(
            source_generation(&db),
            generation,
            "committed source_generation is unchanged by tool calls"
        );

        assert_admission(&seq, &db)?;
        cleanup(&dir, &db);
    }
}
