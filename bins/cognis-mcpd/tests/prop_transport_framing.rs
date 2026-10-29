//! Property 20: Transport framing is preserved.
//!
//! For any sequence of well-formed JSON-RPC messages — including batched
//! writes, partial lines, and large payloads — the newline-delimited stdio
//! surface round-trips them unchanged through the Thin_Adapter to the owner
//! and back.
//!
//! **Validates: Requirements 17.5, 22.4, 17.3**

use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cognis_core::MCP_TOOLS;
use cognis_mcpd::proxy::{serve_proxy, ProxyTarget};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
enum WireMessage {
    Request {
        id: u32,
        method: String,
        blob: String,
    },
    Batch(Vec<(u32, String, String)>),
}

fn arb_method() -> impl Strategy<Value = String> {
    prop::sample::select(
        MCP_TOOLS
            .iter()
            .copied()
            .chain(["initialize", "tools/list"])
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
}

fn arb_blob() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-z0-9]{1,64}",
        (64usize..513).prop_map(|n| "L".repeat(n)),
        (512usize..2049).prop_map(|n| "G".repeat(n)),
    ]
}

fn arb_request() -> impl Strategy<Value = (u32, String, String)> {
    (1u32..500, arb_method(), arb_blob())
}

fn arb_message() -> impl Strategy<Value = WireMessage> {
    prop_oneof![
        3 => arb_request().prop_map(|(id, method, blob)| WireMessage::Request { id, method, blob }),
        1 => prop::collection::vec(arb_request(), 2..5).prop_map(WireMessage::Batch),
    ]
}

fn encode(message: &WireMessage) -> String {
    match message {
        WireMessage::Request { id, method, blob } => json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": { "payload": blob },
        })
        .to_string(),
        WireMessage::Batch(items) => Value::Array(
            items
                .iter()
                .map(|(id, method, blob)| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": method,
                        "params": { "payload": blob },
                    })
                })
                .collect(),
        )
        .to_string(),
    }
}

/// Reader that yields at most `chunk` bytes per `read`, so a message newline
/// may arrive across several reads (partial lines).
struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.max(1).min(buf.len());
        self.inner.read(&mut buf[..n])
    }
}

fn handle_owner_conn(stream: TcpStream, received: &Mutex<Vec<String>>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let text = String::from_utf8(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    received.lock().expect("received lock").push(text.clone());
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let mut writer = stream;
    writer.write_all(response.as_bytes())?;
    writer.flush()?;
    Ok(())
}

type EchoOwnerHandle = (
    ProxyTarget,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicBool>,
    thread::JoinHandle<()>,
);

fn spawn_echo_owner() -> EchoOwnerHandle {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind framing owner");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let port = listener.local_addr().expect("addr").port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let received_thread = Arc::clone(&received);
    let stop_thread = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = handle_owner_conn(stream, &received_thread);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    let target = ProxyTarget::parse(&format!("http://127.0.0.1:{port}/mcp")).expect("target");
    (target, received, stop, handle)
}

fn round_trip(messages: Vec<WireMessage>, chunk: usize) -> Result<(), TestCaseError> {
    let encoded: Vec<String> = messages.iter().map(encode).collect();
    for line in &encoded {
        prop_assert!(
            serde_json::from_str::<Value>(line).is_ok(),
            "generator must emit well-formed JSON-RPC"
        );
        prop_assert!(
            !line.contains('\n'),
            "stdio frames are one compact JSON value per line"
        );
    }

    let mut stdin = encoded.join("\n");
    stdin.push('\n');

    let (target, received, stop, owner) = spawn_echo_owner();
    let mut reader = BufReader::new(ChunkedReader {
        inner: Cursor::new(stdin.into_bytes()),
        chunk,
    });
    let mut stdout = Vec::new();
    serve_proxy(&mut reader, &mut stdout, &target).expect("thin adapter serve_proxy");
    stop.store(true, Ordering::SeqCst);
    let _ = owner.join();

    let got_owner = received.lock().expect("received").clone();
    prop_assert_eq!(
        got_owner,
        encoded.clone(),
        "owner must receive each JSON-RPC frame unchanged"
    );

    let got_stdout: Vec<String> = String::from_utf8(stdout)
        .expect("utf8 stdout")
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect();
    prop_assert_eq!(
        got_stdout,
        encoded,
        "newline-delimited stdout must round-trip the owner response unchanged"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 20: Transport framing is preserved.
    /// **Validates: Requirements 17.5, 22.4, 17.3**
    #[test]
    fn jsonrpc_stdio_round_trips_through_thin_adapter(
        messages in prop::collection::vec(arb_message(), 1..6),
        chunk in 1usize..65,
    ) {
        round_trip(messages, chunk)?;
    }
}
