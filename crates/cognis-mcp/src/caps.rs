//! Hard caps — applied **before** a tool executes (Requirement 3.4).
//!
//! Rust mirror of the limit constants and clamping in
//! `apps/cognis-mcpd/cognis_mcpd/tools.py`. The MCP server is read-only and
//! bounds every dimension an untrusted caller can inflate **before** doing any
//! work, so a single request can never exhaust the process:
//!
//! * `k` (result count)              ≤ 50
//! * `depth` (dependency trace)      ≤ 8
//! * `max_tokens` (capsule budget)   ≤ 32000 (floored at 500)
//! * `symbol_ids` (batch resolve)    ≤ 50
//! * wall-time                       soft 5s / hard 10s
//! * concurrency                     ≤ 16 in-flight tool calls
//!
//! Clamping is *saturating*, mirroring the Python `max(1, min(k, _MAX_K))`:
//! over-limit values are reduced to the cap rather than rejected, so a caller
//! asking for `k = 1_000_000` gets the top 50 instead of an error.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::errors::McpError;

/// Max results returned by any ranked tool.
pub const MAX_K: usize = 50;
/// Max dependency-trace traversal depth.
pub const MAX_DEPTH: u8 = 8;
/// Max capsule token budget.
pub const MAX_TOKENS: u32 = 32_000;
/// Min capsule token budget (floor).
pub const MIN_TOKENS: u32 = 500;
/// Max ids accepted by `resolve_symbols`.
pub const MAX_RESOLVE_IDS: usize = 50;
/// Default in-flight tool-call concurrency cap.
pub const DEFAULT_MAX_CONCURRENCY: usize = 16;
/// Default soft wall-time (seconds).
pub const DEFAULT_SOFT_TIMEOUT_S: f64 = 5.0;
/// Default hard wall-time (seconds).
pub const DEFAULT_HARD_TIMEOUT_S: f64 = 10.0;

/// The hard-cap configuration enforced by the server.
///
/// [`Caps::default`] reproduces the Python server's baked-in limits. The cap
/// values are policy, not data — they are applied identically regardless of
/// caller input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    /// Max results for any ranked tool.
    pub max_k: usize,
    /// Max dependency-trace depth.
    pub max_depth: u8,
    /// Max capsule token budget.
    pub max_tokens: u32,
    /// Min capsule token budget.
    pub min_tokens: u32,
    /// Max ids per `resolve_symbols` call.
    pub max_resolve_ids: usize,
    /// Max in-flight tool calls.
    pub max_concurrency: usize,
    /// Soft wall-time budget.
    pub soft_timeout: Duration,
    /// Hard wall-time budget.
    pub hard_timeout: Duration,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            max_k: MAX_K,
            max_depth: MAX_DEPTH,
            max_tokens: MAX_TOKENS,
            min_tokens: MIN_TOKENS,
            max_resolve_ids: MAX_RESOLVE_IDS,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            soft_timeout: Duration::from_secs_f64(DEFAULT_SOFT_TIMEOUT_S),
            hard_timeout: Duration::from_secs_f64(DEFAULT_HARD_TIMEOUT_S),
        }
    }
}

impl Caps {
    /// Clamp a requested result count into `[1, max_k]` (mirrors
    /// `max(1, min(k, _MAX_K))`).
    pub fn clamp_k(&self, k: i64) -> usize {
        let upper = self.max_k as i64;
        k.clamp(1, upper) as usize
    }

    /// Clamp a requested trace depth into `[1, max_depth]`.
    pub fn clamp_depth(&self, depth: i64) -> u8 {
        let upper = i64::from(self.max_depth);
        depth.clamp(1, upper) as u8
    }

    /// Clamp a requested token budget into `[min_tokens, max_tokens]` (mirrors
    /// `max(500, min(max_tokens, _MAX_TOKENS))`).
    pub fn clamp_tokens(&self, tokens: i64) -> u32 {
        let lo = i64::from(self.min_tokens);
        let hi = i64::from(self.max_tokens);
        tokens.clamp(lo, hi) as u32
    }

    /// Validate a batch-resolve id count, erroring (not clamping) when it
    /// exceeds the cap — matches the Python `resolve_symbols`, which rejects an
    /// over-limit batch with `INVALID_ARGUMENT`.
    pub fn check_resolve_ids(&self, count: usize) -> Result<(), McpError> {
        if count > self.max_resolve_ids {
            return Err(McpError::invalid_argument(format!(
                "symbol_ids exceeds max {} ids",
                self.max_resolve_ids
            )));
        }
        Ok(())
    }
}

/// A wall-time budget tracker handed to a running handler.
///
/// Handlers call [`Deadline::check`] at checkpoints; it returns a retryable
/// `TIMEOUT` [`McpError`] once the hard budget is spent. This is the
/// cooperative, Windows-safe guard (a hard-kill watchdog thread is a later
/// hardening concern — design § Error Handling); the cap is still enforced
/// *before* execution by being constructed at admission time.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    start: Instant,
    soft: Duration,
    hard: Duration,
}

impl Deadline {
    /// Start a deadline now from the given caps.
    pub fn start(caps: &Caps) -> Self {
        Deadline {
            start: Instant::now(),
            soft: caps.soft_timeout,
            hard: caps.hard_timeout,
        }
    }

    /// Elapsed time since admission.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Error if the hard budget is exceeded, and (when `enforce_soft`) the soft
    /// budget. Mirrors `_check_elapsed`.
    pub fn check(&self, tool: &str, enforce_soft: bool) -> Result<(), McpError> {
        let elapsed = self.start.elapsed();
        if elapsed > self.hard {
            return Err(McpError::timeout(format!(
                "Tool '{tool}' exceeded hard wall time ({:.1}s > {:.1}s)",
                elapsed.as_secs_f64(),
                self.hard.as_secs_f64()
            )));
        }
        if enforce_soft && elapsed > self.soft {
            return Err(McpError::timeout(format!(
                "Tool '{tool}' exceeded soft wall time ({:.1}s > {:.1}s)",
                elapsed.as_secs_f64(),
                self.soft.as_secs_f64()
            )));
        }
        Ok(())
    }
}

/// A bounded counter capping the number of concurrently-executing tool calls.
///
/// Mirrors the Python `_CONCURRENCY_SEMAPHORE`: a saturated server fails fast
/// with a retryable `TIMEOUT` envelope rather than piling up work. [`acquire`]
/// returns a RAII [`ConcurrencySlot`] that releases on drop. A cap of `0`
/// disables the limit.
///
/// [`acquire`]: ConcurrencyLimiter::acquire
#[derive(Debug)]
pub struct ConcurrencyLimiter {
    in_flight: AtomicUsize,
    cap: usize,
}

impl ConcurrencyLimiter {
    /// Create a limiter with the given cap (`0` ⇒ unbounded).
    pub fn new(cap: usize) -> Self {
        ConcurrencyLimiter {
            in_flight: AtomicUsize::new(0),
            cap,
        }
    }

    /// Try to admit one call. Returns a slot guard, or a retryable `TIMEOUT`
    /// error when the server is already at capacity.
    pub fn acquire(&self, tool: &str) -> Result<ConcurrencySlot<'_>, McpError> {
        if self.cap == 0 {
            return Ok(ConcurrencySlot { limiter: None });
        }
        // Reserve a slot iff we stay within the cap (CAS loop, no lock).
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.cap {
                return Err(McpError::timeout(format!(
                    "Cognis MCP server is at capacity ({} concurrent calls); \
                     tool '{tool}' was not admitted. Retry shortly.",
                    self.cap
                )));
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(ConcurrencySlot {
                        limiter: Some(self),
                    })
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Current number of in-flight calls (for tests / introspection).
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }
}

/// RAII guard for an admitted tool call; releases its slot on drop.
#[derive(Debug)]
pub struct ConcurrencySlot<'a> {
    limiter: Option<&'a ConcurrencyLimiter>,
}

impl Drop for ConcurrencySlot<'_> {
    fn drop(&mut self) {
        if let Some(limiter) = self.limiter {
            limiter.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_k_saturates_both_ends() {
        let caps = Caps::default();
        assert_eq!(caps.clamp_k(1_000_000), 50);
        assert_eq!(caps.clamp_k(0), 1);
        assert_eq!(caps.clamp_k(-5), 1);
        assert_eq!(caps.clamp_k(10), 10);
    }

    #[test]
    fn clamp_depth_saturates() {
        let caps = Caps::default();
        assert_eq!(caps.clamp_depth(100), 8);
        assert_eq!(caps.clamp_depth(0), 1);
        assert_eq!(caps.clamp_depth(3), 3);
    }

    #[test]
    fn clamp_tokens_floor_and_ceiling() {
        let caps = Caps::default();
        assert_eq!(caps.clamp_tokens(10), 500);
        assert_eq!(caps.clamp_tokens(1_000_000), 32_000);
        assert_eq!(caps.clamp_tokens(8000), 8000);
    }

    #[test]
    fn resolve_ids_over_limit_errors() {
        let caps = Caps::default();
        assert!(caps.check_resolve_ids(50).is_ok());
        let err = caps.check_resolve_ids(51).unwrap_err();
        assert_eq!(err.code, crate::errors::INVALID_ARGUMENT);
    }

    #[test]
    fn concurrency_limiter_admits_up_to_cap() {
        let limiter = ConcurrencyLimiter::new(2);
        let s1 = limiter.acquire("a").unwrap();
        let s2 = limiter.acquire("b").unwrap();
        assert_eq!(limiter.in_flight(), 2);
        // Third call over the cap is rejected with a retryable timeout.
        let err = limiter.acquire("c").unwrap_err();
        assert_eq!(err.code, crate::errors::TIMEOUT);
        assert!(err.is_retryable());
        drop(s1);
        // A slot freed ⇒ next acquire succeeds.
        let _s3 = limiter.acquire("c").unwrap();
        drop(s2);
    }

    #[test]
    fn concurrency_cap_zero_is_unbounded() {
        let limiter = ConcurrencyLimiter::new(0);
        let _a = limiter.acquire("a").unwrap();
        let _b = limiter.acquire("b").unwrap();
        let _c = limiter.acquire("c").unwrap();
        assert_eq!(limiter.in_flight(), 0); // disabled ⇒ not counted
    }

    #[test]
    fn deadline_trips_on_zero_budget() {
        let caps = Caps {
            soft_timeout: Duration::from_secs(0),
            hard_timeout: Duration::from_secs(0),
            ..Caps::default()
        };
        let d = Deadline::start(&caps);
        std::thread::sleep(Duration::from_millis(2));
        let err = d.check("t", false).unwrap_err();
        assert_eq!(err.code, crate::errors::TIMEOUT);
    }

    #[test]
    fn deadline_ok_within_budget() {
        let d = Deadline::start(&Caps::default());
        assert!(d.check("t", true).is_ok());
    }
}
