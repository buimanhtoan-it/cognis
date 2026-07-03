//! MCP error envelope + typed tool errors.
//!
//! Rust mirror of `apps/cognis-mcpd/cognis_mcpd/errors.py`. The **error
//! envelope** is the stable JSON shape the TypeScript extension's
//! `showErrorGuidance` reads (it inspects `result.error.code` / `.message` /
//! `.retryable`), so it is part of the invariant contract (Requirement 3.2,
//! Property 4 — P-CON-*):
//!
//! ```json
//! {"error": {"code": "...", "message": "...", "retryable": true}}
//! ```
//!
//! Every tool handler converts a failure into this envelope rather than letting
//! it propagate as a JSON-RPC protocol error — the call *succeeds* at the
//! protocol level and the payload carries the error, exactly as the Python
//! server (fastmcp) returned the dict (design § Error Handling).

use serde::Serialize;

/// Symbol could not be resolved.
pub const SYMBOL_NOT_FOUND: &str = "SYMBOL_NOT_FOUND";
/// The UCKG index is not yet readable (no DB / mid-build).
pub const INDEX_NOT_READY: &str = "INDEX_NOT_READY";
/// The tool exceeded its wall-time / concurrency budget.
pub const TIMEOUT: &str = "TIMEOUT";
/// The embedder / semantic layer is unavailable.
pub const EMBEDDER_UNAVAILABLE: &str = "EMBEDDER_UNAVAILABLE";
/// A caller-supplied argument was invalid.
pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
/// An unexpected internal failure.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// Error codes that default to `retryable = true` (mirrors Python
/// `_RETRYABLE_CODES`).
const RETRYABLE_CODES: [&str; 2] = [TIMEOUT, INDEX_NOT_READY];

/// A typed error raised inside a tool handler.
///
/// Handlers return `Result<Value, McpError>`; the dispatcher converts the
/// `Err` into the [`error_envelope`] shape so a tool never panics or escapes an
/// unhandled error (design § Error Handling, CP-10). `retryable` defaults from
/// the error code (timeout / index-not-ready are retryable) unless set
/// explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    /// Machine-readable code (one of the `*` constants in this module).
    pub code: String,
    /// Human-readable, path/secret-free message.
    pub message: String,
    /// Whether the caller should retry; `None` ⇒ derived from `code`.
    pub retryable: Option<bool>,
}

impl McpError {
    /// Build an error, deferring `retryable` to the code default.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        McpError {
            code: code.into(),
            message: message.into(),
            retryable: None,
        }
    }

    /// Build an error with an explicit `retryable` flag.
    pub fn with_retryable(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        McpError {
            code: code.into(),
            message: message.into(),
            retryable: Some(retryable),
        }
    }

    /// Convenience: `INVALID_ARGUMENT` (not retryable).
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        McpError::with_retryable(INVALID_ARGUMENT, message, false)
    }

    /// Convenience: `TIMEOUT` (retryable).
    pub fn timeout(message: impl Into<String>) -> Self {
        McpError::with_retryable(TIMEOUT, message, true)
    }

    /// Convenience: `SYMBOL_NOT_FOUND` (not retryable).
    pub fn not_found(message: impl Into<String>) -> Self {
        McpError::with_retryable(SYMBOL_NOT_FOUND, message, false)
    }

    /// Convenience: `INTERNAL_ERROR` (not retryable).
    pub fn internal(message: impl Into<String>) -> Self {
        McpError::with_retryable(INTERNAL_ERROR, message, false)
    }

    /// Convenience: `INDEX_NOT_READY` (retryable).
    pub fn index_not_ready(message: impl Into<String>) -> Self {
        McpError::with_retryable(INDEX_NOT_READY, message, true)
    }

    /// Resolve the effective `retryable` flag (code default when unset).
    pub fn is_retryable(&self) -> bool {
        self.retryable
            .unwrap_or_else(|| RETRYABLE_CODES.contains(&self.code.as_str()))
    }

    /// Render the stable error envelope `serde_json::Value`.
    pub fn to_envelope(&self) -> serde_json::Value {
        error_envelope(&self.code, &self.message, self.is_retryable())
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for McpError {}

/// The serialized inner body of the envelope (`error` field value).
#[derive(Debug, Clone, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    retryable: bool,
}

/// Build the standard MCP error envelope (mirrors `errors.py::error_envelope`).
///
/// The returned value is `{"error": {"code", "message", "retryable"}}` — the
/// exact shape the extension's `showErrorGuidance` consumes.
pub fn error_envelope(code: &str, message: &str, retryable: bool) -> serde_json::Value {
    serde_json::json!({
        "error": serde_json::to_value(ErrorBody {
            code,
            message,
            retryable,
        })
        .expect("error body always serializes"),
    })
}

/// Whether a `serde_json::Value` is an error envelope (`{"error": {...}}`).
pub fn is_error_envelope(value: &serde_json::Value) -> bool {
    value.get("error").is_some_and(|e| e.is_object())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_matches_contract() {
        let env = error_envelope(SYMBOL_NOT_FOUND, "nope", false);
        assert_eq!(env["error"]["code"], "SYMBOL_NOT_FOUND");
        assert_eq!(env["error"]["message"], "nope");
        assert_eq!(env["error"]["retryable"], false);
        // Exactly the three keys the extension reads, nothing else.
        let obj = env["error"].as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(is_error_envelope(&env));
    }

    #[test]
    fn retryable_defaults_from_code() {
        assert!(McpError::new(TIMEOUT, "t").is_retryable());
        assert!(McpError::new(INDEX_NOT_READY, "t").is_retryable());
        assert!(!McpError::new(SYMBOL_NOT_FOUND, "t").is_retryable());
        assert!(!McpError::new(INVALID_ARGUMENT, "t").is_retryable());
        assert!(!McpError::new(INTERNAL_ERROR, "t").is_retryable());
    }

    #[test]
    fn explicit_retryable_overrides_code_default() {
        // A normally-not-retryable code can be marked retryable.
        let e = McpError::with_retryable(INVALID_ARGUMENT, "x", true);
        assert!(e.is_retryable());
        assert_eq!(e.to_envelope()["error"]["retryable"], true);
    }

    #[test]
    fn non_error_value_is_not_envelope() {
        assert!(!is_error_envelope(&serde_json::json!({"symbols": []})));
        assert!(!is_error_envelope(&serde_json::json!([1, 2, 3])));
    }
}
