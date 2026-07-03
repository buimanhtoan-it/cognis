//! HTTP transport for the MCP server (Requirement 3, `mcp.http`).
//!
//! `cognis-mcpd --transport http --host <h> --port <p>` serves the same
//! read-only 8-tool JSON-RPC surface as the stdio transport, but over
//! HTTP/1.1 on a localhost port an editor connects to (the panel-managed
//! "Start MCP server" flow, `apps/cognis-vscode/src/mcpServer.ts`). The stdio
//! transport remains the default and the one the editor spawns/owns; this is
//! the standalone-server option.
//!
//! The endpoint is intentionally small and dependency-free (std `TcpListener`,
//! no async runtime): a client POSTs a JSON-RPC request (or a batch array) to
//! `/mcp` and receives the JSON-RPC response as `application/json`. It reuses
//! [`McpServer::handle`] for dispatch, so the wire contract (tool set, output
//! shapes, error envelope) is identical to stdio by construction — there is no
//! second implementation of the protocol to drift.
//!
//! Framing notes:
//! * One request/response per connection (`Connection: close`); MCP HTTP
//!   clients open a fresh POST per call, so this is sufficient and avoids
//!   keep-alive edge cases.
//! * `GET /mcp` (the streamable-http SSE upgrade) is answered `405` with an
//!   `Allow: POST` header — this server does not push server-initiated events;
//!   request/response tool calls (the traffic that matters) work.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};

use serde_json::Value;

use crate::engine::RetrievalEngine;
use crate::jsonrpc::{Request, Response, RpcError, INVALID_REQUEST};
use crate::server::McpServer;

/// Cap on the request body we will read, so a bogus `Content-Length` can't make
/// the server allocate unbounded memory.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Bind `host:port` and serve the MCP JSON-RPC surface over HTTP until the
/// listener errors unrecoverably. Blocks the calling thread (the daemon's main
/// loop). Per-connection failures are logged-and-skipped, never fatal.
pub fn serve_http<E: RetrievalEngine>(
    server: &McpServer<E>,
    host: &str,
    port: u16,
) -> std::io::Result<()> {
    let listener = bind(host, port)?;
    serve_listener(server, listener)
}

/// Serve the MCP JSON-RPC surface on an already-bound [`TcpListener`]. Split
/// from [`serve_http`] so a caller can bind first (reporting a port-in-use
/// error) and only then announce readiness before entering the serve loop.
pub fn serve_listener<E: RetrievalEngine>(
    server: &McpServer<E>,
    listener: TcpListener,
) -> std::io::Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // A single bad client connection must not take the server down.
                let _ = handle_connection(server, stream);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

/// Bind a `TcpListener`, resolving `host:port`. Split out so [`serve_http`] can
/// report a bind failure (port in use) distinctly from serve-loop errors.
pub fn bind(host: &str, port: u16) -> std::io::Result<TcpListener> {
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no socket address")
    })?;
    TcpListener::bind(addr)
}

/// Handle one HTTP request on `stream`: parse the request line + headers, read
/// the body for a POST, dispatch it, and write the response.
fn handle_connection<E: RetrievalEngine>(
    server: &McpServer<E>,
    stream: TcpStream,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // Request line: "<METHOD> <PATH> HTTP/1.1".
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(()); // client closed before sending anything
    }
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    // Headers until a blank line; we only need Content-Length.
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(value) = header
            .split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.trim())
        {
            content_length = value.parse::<usize>().unwrap_or(0).min(MAX_BODY_BYTES);
        }
    }

    if !method.eq_ignore_ascii_case("POST") {
        // Only POST carries JSON-RPC. GET (SSE upgrade) and others are declined.
        return write_http(
            &mut writer,
            "405 Method Not Allowed",
            "text/plain",
            b"MCP over HTTP: POST JSON-RPC to /mcp",
            &[("Allow", "POST")],
        );
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body_text = String::from_utf8_lossy(&body);

    match dispatch(server, &body_text) {
        Some(json) => write_http(
            &mut writer,
            "200 OK",
            "application/json",
            json.as_bytes(),
            &[],
        ),
        // A body of only notifications produces no response payload — 202.
        None => write_http(&mut writer, "202 Accepted", "application/json", b"", &[]),
    }
}

/// Dispatch a JSON-RPC request body (single object or a batch array) through the
/// server. Returns the serialized JSON response (object for a single request,
/// array for a batch), or `None` when there is nothing to send (all
/// notifications / empty batch).
fn dispatch<E: RetrievalEngine>(server: &McpServer<E>, body: &str) -> Option<String> {
    let value: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            // Not JSON at all: a single parse-error response with a null id.
            let resp = Response::error(
                Value::Null,
                RpcError::new(-32700, format!("parse error: {e}")),
            );
            return Some(serde_json::to_string(&resp).unwrap_or_default());
        }
    };

    match value {
        Value::Array(items) => {
            let mut responses: Vec<Response> = Vec::new();
            for item in items {
                if let Some(resp) = dispatch_one(server, item) {
                    responses.push(resp);
                }
            }
            if responses.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&responses).unwrap_or_default())
            }
        }
        other => dispatch_one(server, other).map(|r| serde_json::to_string(&r).unwrap_or_default()),
    }
}

/// Dispatch a single JSON value as a JSON-RPC request. `None` for a
/// notification (no id ⇒ no response).
fn dispatch_one<E: RetrievalEngine>(server: &McpServer<E>, value: Value) -> Option<Response> {
    match serde_json::from_value::<Request>(value) {
        Ok(req) => server.handle(req),
        Err(e) => Some(Response::error(
            Value::Null,
            RpcError::new(INVALID_REQUEST, format!("invalid Request object: {e}")),
        )),
    }
}

/// Write a minimal HTTP/1.1 response with `Connection: close`.
fn write_http(
    writer: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}
