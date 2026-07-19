//! Bug facet #7 — Serial / unbounded / unauthenticated HTTP transport.
//!
//! These are BUG-CONDITION EXPLORATION tests (Requirements 1.8, 1.12; expected
//! behavior 2.8, 2.12). They encode the *expected* (fixed) behavior and
//! therefore MUST FAIL on the unfixed code:
//!
//!   * Head-of-line blocking: two concurrent POSTs to the HTTP transport must
//!     be served with bounded concurrency (they overlap in time). On the
//!     unfixed code `serve_listener` accepts and handles one connection at a
//!     time on the calling thread, so a slow in-flight request serializes the
//!     next one — they cannot overlap.
//!
//!   * Loopback-only + auth: a non-loopback bind must be rejected (or require
//!     an explicit opt-in) and shared routes must require a scoped credential.
//!     On the unfixed code `bind(host, port)` binds whatever `--host` is passed
//!     (e.g. `0.0.0.0`) with no authorization at all.
//!
//! The engine used here is a tiny in-crate fake so the transport is exercised
//! without a live DB; one tool handler is made artificially slow so overlap is
//! observable in wall-clock time.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use cognis_core::{Hit, Result, Symbol};
use cognis_mcp::engine::RetrievalEngine;
use cognis_mcp::http::{bind, serve_listener_with, HttpServeConfig, RouteCredential};
use cognis_mcp::server::McpServer;

/// A retrieval engine whose `fts_search` blocks for a fixed delay, so a single
/// in-flight request occupies the server long enough that a second concurrent
/// request would have to wait if (and only if) the transport is serial.
struct SlowEngine {
    delay: Duration,
}

impl RetrievalEngine for SlowEngine {
    fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        thread::sleep(self.delay);
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

/// Fire one `tools/call` POST for the (slow) `symbol_search` tool at
/// `127.0.0.1:port` and return the wall-clock span from just-before-send to
/// full-response-read. Presents the scoped route credential (Requirement 2.12).
fn timed_search_call(port: u16, id: u32, token: &str) -> Duration {
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
    let start = Instant::now();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().ok();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read response");
    start.elapsed()
}

/// Head-of-line blocking: two concurrent requests must overlap under a
/// bounded-concurrent transport. Serial handling forces the second to wait for
/// the first, so total wall-clock ≈ 2×delay (no overlap).
#[test]
fn concurrent_posts_are_served_with_bounded_concurrency_not_serialized() {
    let delay = Duration::from_millis(600);
    let listener = bind("127.0.0.1", 0).expect("bind loopback ephemeral");
    let port = listener.local_addr().unwrap().port();
    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();

    // Serve on a background thread; the process exits at test end so the
    // blocking accept loop needs no explicit shutdown.
    let server = McpServer::new(SlowEngine { delay });
    let cfg = HttpServeConfig {
        route_credential: Some(cred),
        ..HttpServeConfig::default()
    };
    thread::spawn(move || {
        let _ = serve_listener_with(&server, listener, cfg);
    });

    // Two clients fire as concurrently as we can arrange them.
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    for id in 0..2u32 {
        let tx = tx.clone();
        let token = token.clone();
        handles.push(thread::spawn(move || {
            tx.send(timed_search_call(port, id, &token)).unwrap();
        }));
    }
    drop(tx);

    let wall = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let total = wall.elapsed();
    let _each: Vec<Duration> = rx.iter().collect();

    // EXPECTED (fixed): with true overlap, total ≈ 1×delay. We allow generous
    // slack for scheduling/handshake. A serial transport takes ≈ 2×delay.
    let overlap_ceiling = delay + delay / 2; // 1.5×delay
    assert!(
        total < overlap_ceiling,
        "two concurrent POSTs took {total:?} (≥ {overlap_ceiling:?}), i.e. they \
         serialized (~2×{delay:?}) instead of overlapping; the unfixed HTTP \
         transport handles one connection at a time on the accept thread \
         (head-of-line blocking) with no bounded worker pool"
    );
}

/// Loopback-only + auth: binding a non-loopback host must be rejected (or gated
/// behind an explicit opt-in) rather than silently accepted with no auth. The
/// unfixed `bind` accepts any host the caller passes with zero authorization.
#[test]
fn non_loopback_bind_is_rejected_by_default() {
    // A routable non-loopback address the fix must refuse to bind by default
    // (it exposes the unauthenticated tool surface beyond the machine). We use
    // the wildcard `0.0.0.0`, which the unfixed transport binds happily.
    let result = bind("0.0.0.0", 0);

    if let Ok(listener) = result.as_ref() {
        // Prove the surface is reachable with NO credential: the bind succeeded
        // and the transport requires no auth token to accept a connection.
        let _ = listener; // binding alone is the exposure the fix must prevent
    }

    // EXPECTED (fixed): a non-loopback bind is rejected by default (loopback
    // only) unless the operator explicitly opts in. On unfixed code this bind
    // succeeds unconditionally and the served surface has no authentication.
    assert!(
        result.is_err(),
        "binding a non-loopback host (0.0.0.0) succeeded with no auth; the fix \
         must bind loopback-only by default and reject non-loopback binds unless \
         explicitly opted in, and require a scoped credential on shared routes"
    );
}
