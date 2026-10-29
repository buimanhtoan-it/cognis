//! JSON-RPC 2.0 message types + newline-delimited stdio framing.
//!
//! The MCP stdio transport exchanges JSON-RPC 2.0 messages as **newline-
//! delimited JSON** (one compact JSON object per line) over stdin/stdout. This
//! module provides the wire types ([`Request`], [`Response`], [`RpcError`]),
//! the standard error codes, and the [`read_message`] / [`write_message`]
//! framing helpers the server loop uses.
//!
//! Framing is deliberately transport-only: it knows nothing about MCP methods
//! or the tool contract (those live in [`crate::server`]). Parsing never
//! panics — a malformed line yields a `-32700 Parse error` the caller turns
//! into a response.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `-32700` — invalid JSON was received.
pub const PARSE_ERROR: i64 = -32700;
/// `-32600` — the JSON is not a valid Request object.
pub const INVALID_REQUEST: i64 = -32600;
/// `-32601` — the method does not exist / is not available.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// `-32602` — invalid method parameters.
pub const INVALID_PARAMS: i64 = -32602;
/// `-32603` — internal JSON-RPC error.
pub const INTERNAL_ERROR: i64 = -32603;

/// A decoded JSON-RPC 2.0 request or notification.
///
/// A message with no `id` is a *notification* (no response is sent). `params`
/// is optional and free-form; method dispatch validates its shape.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// Protocol marker; must be `"2.0"`.
    #[serde(default)]
    pub jsonrpc: String,
    /// Request id. Absent for notifications. May be a number or string.
    #[serde(default)]
    pub id: Option<Value>,
    /// Method name (e.g. `"tools/call"`).
    pub method: String,
    /// Optional parameters.
    #[serde(default)]
    pub params: Option<Value>,
}

impl Request {
    /// Whether this message is a notification (no `id` ⇒ no response).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcError {
    /// Numeric error code (see the `*_ERROR` / `*` constants).
    pub code: i64,
    /// Short human-readable description.
    pub message: String,
    /// Optional structured detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Build an error with no `data`.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A JSON-RPC 2.0 response (success xor error), always carrying its `id`.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// Echoes the request id (`null` when the request id was unreadable).
    pub id: Value,
    /// Present on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A success response carrying `result`.
    pub fn success(id: Value, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response carrying an [`RpcError`].
    pub fn error(id: Value, error: RpcError) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Read and parse one newline-delimited JSON-RPC message.
///
/// Returns:
/// * `Ok(Some(Ok(req)))`   — a well-formed request/notification,
/// * `Ok(Some(Err(rpc)))`  — a line that parsed as JSON-text but not as a
///   Request (`-32600`) or was not valid JSON at all (`-32700`),
/// * `Ok(None)`            — clean EOF (the peer closed stdin),
/// * `Err(_)`              — an underlying IO error.
///
/// Blank lines are skipped (some clients emit keep-alive newlines).
#[allow(clippy::type_complexity)]
pub fn read_message<R: BufRead>(
    reader: &mut R,
) -> io::Result<Option<std::result::Result<Request, RpcError>>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // skip keep-alive blank lines
        }
        return Ok(Some(parse_message(trimmed)));
    }
}

/// Parse a single trimmed JSON line into a [`Request`] or an [`RpcError`].
pub fn parse_message(line: &str) -> std::result::Result<Request, RpcError> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => match serde_json::from_value::<Request>(value) {
            Ok(req) => Ok(req),
            Err(e) => Err(RpcError::new(
                INVALID_REQUEST,
                format!("invalid Request object: {e}"),
            )),
        },
        Err(e) => Err(RpcError::new(PARSE_ERROR, format!("parse error: {e}"))),
    }
}

/// Serialize and write one response as a single newline-terminated line, then
/// flush so the peer sees it immediately.
pub fn write_message<W: Write>(writer: &mut W, response: &Response) -> io::Result<()> {
    let text = serde_json::to_string(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writer.write_all(text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_request_with_id() {
        let req = parse_message(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.id, Some(Value::from(1)));
        assert!(!req.is_notification());
    }

    #[test]
    fn parses_notification_without_id() {
        let req =
            parse_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(req.is_notification());
    }

    #[test]
    fn malformed_json_is_parse_error() {
        let err = parse_message("{not json").unwrap_err();
        assert_eq!(err.code, PARSE_ERROR);
    }

    #[test]
    fn missing_method_is_invalid_request() {
        let err = parse_message(r#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert_eq!(err.code, INVALID_REQUEST);
    }

    #[test]
    fn read_message_skips_blank_lines_then_eof() {
        let mut cur = Cursor::new("\n\n".as_bytes().to_vec());
        // Two blank lines then EOF ⇒ None.
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn read_message_reads_one_per_line() {
        let data = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
                    {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let mut cur = Cursor::new(data.as_bytes().to_vec());
        let first = read_message(&mut cur).unwrap().unwrap().unwrap();
        assert_eq!(first.id, Some(Value::from(1)));
        let second = read_message(&mut cur).unwrap().unwrap().unwrap();
        assert_eq!(second.method, "tools/list");
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn write_message_is_one_line_terminated() {
        let mut buf: Vec<u8> = Vec::new();
        let resp = Response::success(Value::from(7), serde_json::json!({"ok": true}));
        write_message(&mut buf, &resp).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1);
        let back: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(back["jsonrpc"], "2.0");
        assert_eq!(back["id"], 7);
        assert_eq!(back["result"]["ok"], true);
    }

    #[test]
    fn error_response_omits_result_field() {
        let resp = Response::error(Value::from(1), RpcError::new(METHOD_NOT_FOUND, "no"));
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v.get("result").is_none());
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
    }
}
