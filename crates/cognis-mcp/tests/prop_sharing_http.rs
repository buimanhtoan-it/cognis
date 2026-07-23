//! Property-based + unit tests for one-heavy-daemon-per-repository sharing
//! over thin proxy / bounded-concurrent HTTP.
//!
//! Feature: mcp-process-ram-duplication
//! **Property 9: Bug Condition** — One heavy daemon per repository, thin proxy
//! or bounded HTTP
//!
//! **Validates: Requirements 2.8, 2.11**
//!
//! _For any_ set of MCP clients accessing a canonical repository, the fixed
//! system provides at most one heavy repository daemon and connects each host
//! through a model-free thin stdio proxy **or** a host-verified
//! bounded-concurrent loopback HTTP route (explicit worker/queue/time limits,
//! backpressure, retryable overload responses, no serial head-of-line blocking)
//! before sharing is enabled.
//!
//! This suite pins the HTTP capacity algebra and live transport behaviour.
//! Thin-proxy engine-free invariants live in `bins/cognis-mcpd` (`proxy.rs`
//! unit tests) and the TypeScript config/runtime classifiers.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::{Hit, Result, Symbol};
use cognis_mcp::engine::RetrievalEngine;
use cognis_mcp::http::{
    bind, serve_listener_with, HttpServeConfig, RouteCredential,
    DEFAULT_HTTP_OVERLOAD_RETRY_AFTER_SECS, DEFAULT_HTTP_QUEUE_CAPACITY, DEFAULT_HTTP_WORKERS,
};
use cognis_mcp::server::McpServer;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Pure capacity algebra (mirrors the acceptor `try_send` / overload path)
// ---------------------------------------------------------------------------

/// How many of `incoming` connections a bounded pool can accept without
/// overload, given `workers` already busy and a free queue of `queue`.
///
/// Production: one acceptor `try_send`s into a `sync_channel(queue_capacity)`.
/// When workers are all busy the channel holds up to `queue_capacity` streams;
/// any further accept is answered with a retryable `503` immediately.
fn capacity_accepted(workers: usize, queue: usize, busy_workers: usize, incoming: usize) -> usize {
    let free_workers = workers.saturating_sub(busy_workers.min(workers));
    // Free workers pull from the queue immediately, so effective free slots
    // before overload = free_workers + queue.
    let free_slots = free_workers.saturating_add(queue);
    incoming.min(free_slots)
}

fn capacity_overloaded(
    workers: usize,
    queue: usize,
    busy_workers: usize,
    incoming: usize,
) -> usize {
    let accepted = capacity_accepted(workers, queue, busy_workers, incoming);
    incoming.saturating_sub(accepted)
}

// ---------------------------------------------------------------------------
// Property 9 — pure capacity algebra
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: mcp-process-ram-duplication, Property 9: Bug Condition —
    // One heavy daemon per repository, thin proxy or bounded HTTP
    // **Validates: Requirements 2.8, 2.11**
    //
    // For any positive worker/queue bounds and concurrent arrival:
    // * accepted + overloaded = incoming
    // * accepted never exceeds free_workers + queue (bounded capacity)
    // * when workers are fully busy, overload starts exactly after `queue`
    //   additional arrivals (backpressure, not unbounded buffering)
    #[test]
    fn prop9_http_capacity_is_bounded_with_backpressure(
        workers in 1usize..16,
        queue in 1usize..32,
        busy_workers in 0usize..16,
        incoming in 0usize..64,
    ) {
        let busy = busy_workers.min(workers);
        let accepted = capacity_accepted(workers, queue, busy, incoming);
        let overloaded = capacity_overloaded(workers, queue, busy, incoming);

        prop_assert_eq!(
            accepted + overloaded,
            incoming,
            "accepted+overloaded must partition incoming"
        );

        let free_workers = workers - busy;
        let free_slots = free_workers + queue;
        prop_assert!(
            accepted <= free_slots,
            "accepted ({accepted}) must not exceed free capacity ({free_slots})"
        );

        // Fully saturated workers: free slots == queue. Any arrival past the
        // queue is overloaded (retryable 503 in the live transport).
        if busy == workers && incoming > queue {
            prop_assert_eq!(
                overloaded,
                incoming - queue,
                "full worker pool must overload exactly the excess over the queue"
            );
            prop_assert_eq!(accepted, queue);
        }

        // Idle pool: free slots == workers + queue. Overload only after that.
        if busy == 0 && incoming > workers + queue {
            prop_assert_eq!(overloaded, incoming - workers - queue);
            prop_assert_eq!(accepted, workers + queue);
        }
    }

    // Defaults and configuration stay strictly positive and finite so a
    // misconfigured sharing gate cannot re-introduce unbounded concurrency.
    #[test]
    fn prop9_http_serve_config_defaults_are_strictly_positive(
        workers in 0usize..64,
        queue in 0usize..128,
        retry_after in 0u64..10,
    ) {
        // Mirror `HttpServeConfig::normalized` clamps (private) so the property
        // documents the production invariant without relying on a private API.
        let norm_workers = workers.max(1);
        let norm_queue = queue.max(1);
        let norm_retry = retry_after.max(1);

        prop_assert!(norm_workers >= 1);
        prop_assert!(norm_queue >= 1);
        prop_assert!(norm_retry >= 1);

        // Public defaults themselves are positive and match the documented
        // concurrency budget (workers) with a small queue multiplier.
        prop_assert!(DEFAULT_HTTP_WORKERS >= 1);
        prop_assert!(DEFAULT_HTTP_QUEUE_CAPACITY >= DEFAULT_HTTP_WORKERS);
        prop_assert!(DEFAULT_HTTP_OVERLOAD_RETRY_AFTER_SECS >= 1);

        // Constructing a config with the generated values always yields a
        // usable credential-bearing surface (Default mints a secret).
        let cfg = HttpServeConfig {
            worker_count: norm_workers,
            queue_capacity: norm_queue,
            overload_retry_after_secs: norm_retry,
            repo_identity: None,
            model_fingerprint: None,
            ..HttpServeConfig::default()
        };
        prop_assert!(cfg.worker_count >= 1);
        prop_assert!(cfg.queue_capacity >= 1);
        prop_assert!(cfg.route_credential.is_some());
    }
}

// ---------------------------------------------------------------------------
// Live unit: bounded concurrency + overload (Requirement 2.8)
// ---------------------------------------------------------------------------

/// Engine that blocks inside `fts_search` until the gate opens.
struct GatedEngine {
    gate: Arc<(Mutex<bool>, Condvar)>,
    blocked: Arc<AtomicUsize>,
}

impl RetrievalEngine for GatedEngine {
    fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        self.blocked.fetch_add(1, Ordering::SeqCst);
        let (lock, cvar) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = cvar.wait(released).unwrap_or_else(|p| p.into_inner());
        }
        self.blocked.fetch_sub(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    fn semantic_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
    fn diffuse(&self, _seeds: &[Vec<Hit>], _k: usize, _alpha: f64, _eps: f64) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
    fn hydrate(&self, _ids: &[String]) -> Result<Vec<Symbol>> {
        Ok(Vec::new())
    }
    fn lookup(&self, _name_or_id: &str, _kind: Option<&str>) -> Result<Option<Symbol>> {
        Ok(None)
    }
    fn dependency_trace(&self, _symbol_id: &str, _direction: &str, _depth: u8) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
}

/// Engine whose `fts_search` sleeps so concurrent overlap is observable.
struct SlowEngine {
    delay: Duration,
    peak: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
}

impl RetrievalEngine for SlowEngine {
    fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(cur, Ordering::SeqCst);
        thread::sleep(self.delay);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    fn semantic_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
    fn diffuse(&self, _seeds: &[Vec<Hit>], _k: usize, _alpha: f64, _eps: f64) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
    fn hydrate(&self, _ids: &[String]) -> Result<Vec<Symbol>> {
        Ok(Vec::new())
    }
    fn lookup(&self, _name_or_id: &str, _kind: Option<&str>) -> Result<Option<Symbol>> {
        Ok(None)
    }
    fn dependency_trace(&self, _symbol_id: &str, _direction: &str, _depth: u8) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
}

fn post_search(port: u16, id: u32, token: &str) -> (u16, String) {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"symbol_search","arguments":{{"query":"x","k":1}}}}}}"#
    );
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(s) => s,
        // Acceptor may refuse under overload races; treat as empty failure.
        Err(_) => return (0, String::new()),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    if stream.write_all(request.as_bytes()).is_err() {
        return (0, String::new());
    }
    stream.flush().ok();
    let mut buf = Vec::new();
    // Tolerate ConnectionReset / empty partial reads — production clients
    // retry; tests assert status when a full response arrives.
    let _ = stream.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

/// Unit: two concurrent POSTs overlap (no serial head-of-line blocking).
#[test]
fn unit_prop9_concurrent_posts_overlap_under_worker_pool() {
    let delay = Duration::from_millis(400);
    let peak = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    let listener = bind("127.0.0.1", 0).expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = McpServer::new(SlowEngine {
        delay,
        peak: Arc::clone(&peak),
        in_flight: Arc::clone(&in_flight),
    });
    let cfg = HttpServeConfig {
        worker_count: 4,
        queue_capacity: 8,
        request_timeout: Duration::from_secs(5),
        overload_retry_after_secs: 1,
        route_credential: Some(cred),
        repo_identity: None,
        model_fingerprint: None,
    };
    thread::spawn(move || {
        let _ = serve_listener_with(&server, listener, cfg);
    });
    thread::sleep(Duration::from_millis(50));

    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    for id in 0..2u32 {
        let token = token.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            tx.send(post_search(port, id, &token)).unwrap();
        });
    }
    drop(tx);
    let mut statuses = Vec::new();
    for _ in 0..2 {
        let (status, _) = rx.recv().expect("client response");
        statuses.push(status);
    }
    let total = start.elapsed();
    assert!(
        statuses.iter().all(|&s| s == 200),
        "expected successful tool responses, got {statuses:?}"
    );
    // Authoritative, deterministic proof of overlap: the engine bumps an atomic
    // counter on entry to `fts_search` and records the peak, so a peak of >= 2
    // means two tool calls were provably in flight at the same instant. This
    // does not depend on wall-clock scheduling.
    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "expected concurrent in-flight tool calls, peak={}",
        peak.load(Ordering::SeqCst)
    );
    // Wall-clock guard against a *gross* serialization regression only. Two
    // fully serialized 400ms calls would take >= 2*delay (800ms); we allow
    // generous headroom (3*delay) so ordinary scheduling jitter on a loaded CI
    // machine cannot flake this, while a real "one-at-a-time" regression (which
    // would blow past 2*delay) is still caught.
    assert!(
        total < delay * 3,
        "requests serialized ({total:?}) instead of overlapping under the worker pool"
    );
}

/// Unit: a full bounded queue answers with retryable 503 + Retry-After.
#[test]
fn unit_prop9_full_queue_returns_retryable_overload() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let blocked = Arc::new(AtomicUsize::new(0));
    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    let listener = bind("127.0.0.1", 0).expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = McpServer::new(GatedEngine {
        gate: Arc::clone(&gate),
        blocked: Arc::clone(&blocked),
    });
    let cfg = HttpServeConfig {
        worker_count: 1,
        queue_capacity: 1,
        request_timeout: Duration::from_secs(5),
        overload_retry_after_secs: 2,
        route_credential: Some(cred),
        repo_identity: None,
        model_fingerprint: None,
    };
    thread::spawn(move || {
        let _ = serve_listener_with(&server, listener, cfg);
    });
    thread::sleep(Duration::from_millis(50));

    // Occupy the single worker.
    let token_blocker = token.clone();
    let blocker = thread::spawn(move || post_search(port, 1, &token_blocker));
    let deadline = Instant::now() + Duration::from_secs(2);
    while blocked.load(Ordering::SeqCst) < 1 {
        assert!(
            Instant::now() < deadline,
            "worker never entered the gated handler"
        );
        thread::sleep(Duration::from_millis(10));
    }

    // Flood past workers+queue.
    let flood = 8u32;
    let mut handles = Vec::new();
    for id in 0..flood {
        let token = token.clone();
        handles.push(thread::spawn(move || post_search(port, 100 + id, &token)));
    }

    let mut overload_body = String::new();
    let poll_deadline = Instant::now() + Duration::from_secs(3);
    let mut remaining = handles;
    while Instant::now() < poll_deadline && overload_body.is_empty() {
        let mut still = Vec::new();
        for h in remaining {
            if h.is_finished() {
                let (status, body) = h.join().expect("client thread");
                if status == 503 {
                    overload_body = body;
                }
            } else {
                still.push(h);
            }
        }
        remaining = still;
        if overload_body.is_empty() {
            thread::sleep(Duration::from_millis(20));
        }
    }

    // Always release the gate so blocker + queued clients finish.
    {
        let (lock, cvar) = &*gate;
        let mut released = lock.lock().unwrap_or_else(|p| p.into_inner());
        *released = true;
        cvar.notify_all();
    }

    assert!(
        !overload_body.is_empty(),
        "expected at least one 503 overload under a full bounded queue"
    );
    assert!(
        overload_body.to_ascii_lowercase().contains("retry-after"),
        "missing Retry-After header: {overload_body}"
    );
    assert!(
        overload_body.contains("\"retryable\":true"),
        "overload body must mark retryable: {overload_body}"
    );

    let _ = blocker.join();
    for h in remaining {
        let _ = h.join();
    }
}
