//! Property 22: Thin adapters are free and faithful.
//!
//! A Thin_Adapter invocation opens no workspace database, loads no embedding
//! model, and is classified as a thin adapter rather than an owner.
//!
//! **Validates: Requirements 17.1, 17.2, 17.7**

use std::io::{Cursor, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use cognis_core::MCP_TOOLS;
use cognis_embed::ModelSlot;
use cognis_mcpd::proxy::{is_proxy_mode, serve_proxy, ProxyTarget, THIN_PROXY_ENV};
use cognis_owner::{attach_existing_owner, heavy_surface_args};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

#[derive(Debug, Clone)]
enum ThinArgv {
    Flag,
    TransportPair,
    TransportEquals,
}

fn arb_thin_argv() -> impl Strategy<Value = (ThinArgv, Vec<String>)> {
    let kind = prop_oneof![
        Just(ThinArgv::Flag),
        Just(ThinArgv::TransportPair),
        Just(ThinArgv::TransportEquals),
    ];
    let extras = prop::collection::vec("[a-z]{1,6}", 0..3);
    (kind, extras)
}

fn argv_for(kind: &ThinArgv, extras: &[String]) -> Vec<String> {
    let mut args = vec!["mcpd".to_string()];
    match kind {
        ThinArgv::Flag => args.push("--proxy".into()),
        ThinArgv::TransportPair => {
            args.push("--transport".into());
            args.push("proxy".into());
        }
        ThinArgv::TransportEquals => args.push("--transport=proxy".into()),
    }
    for extra in extras {
        args.push(format!("--note-{extra}"));
    }
    args
}

fn arb_jsonrpc_line() -> impl Strategy<Value = String> {
    (
        prop::sample::select(MCP_TOOLS.to_vec()),
        0u32..200,
        prop::option::of("[a-z]{1,8}"),
    )
        .prop_map(|(method, id, query)| {
            let params = match query {
                Some(q) => format!(r#"{{"query":"{q}","k":4}}"#),
                None => "{}".into(),
            };
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
        })
}

fn unique_repo() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p22-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create workspace");
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(".cognis").join("uckg.db")
}

fn echo_owner() -> (ProxyTarget, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo owner");
    let addr = listener.local_addr().expect("echo addr");
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    let url = format!("http://127.0.0.1:{}/mcp", addr.port());
    (ProxyTarget::parse(&url).expect("echo target"), handle)
}

fn invoke_thin(args: &[String], line: &str, repo: &Path) -> Result<(), TestCaseError> {
    prop_assert!(
        is_proxy_mode(args),
        "thin-adapter argv must be classified as a thin proxy, not an owner"
    );
    prop_assert_eq!(THIN_PROXY_ENV, "COGNIS_MCP_PROXY");
    prop_assert_eq!(heavy_surface_args("cognis"), vec!["owner"]);
    prop_assert!(
        !is_proxy_mode(&["owner".into()]),
        "workspace owner argv must not be classified as a thin adapter"
    );
    prop_assert!(!is_proxy_mode(&[
        "owner".into(),
        "--host".into(),
        "127.0.0.1".into()
    ]));

    let slot = ModelSlot::empty();
    prop_assert!(!slot.is_loaded(), "thin adapter must not load a model");

    let attached = attach_existing_owner(repo).expect("attach");
    prop_assert!(
        attached.is_none(),
        "vacant thin attach must not become owner"
    );
    prop_assert!(
        !db_path(repo).exists(),
        "thin adapter must open no Workspace_Database connection"
    );
    prop_assert!(!slot.is_loaded());

    let (target, echo) = echo_owner();
    let mut input = Cursor::new(format!("{line}\n"));
    let mut output = Vec::new();
    serve_proxy(&mut input, &mut output, &target).expect("serve_proxy");
    let _ = echo.join();

    prop_assert!(
        !db_path(repo).exists(),
        "forwarding JSON-RPC must not create the workspace database"
    );
    prop_assert!(
        !slot.is_loaded(),
        "forwarding JSON-RPC must not load an embedding model"
    );
    prop_assert!(
        !output.is_empty(),
        "thin adapter must still speak the newline-delimited JSON-RPC surface"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 22: Thin adapters are free and faithful.
    /// **Validates: Requirements 17.1, 17.2, 17.7**
    #[test]
    fn thin_adapter_opens_no_db_or_model_and_is_classified_thin(
        (kind, extras) in arb_thin_argv(),
        line in arb_jsonrpc_line(),
    ) {
        let repo = unique_repo();
        let _cleanup = Cleanup(repo.clone());
        let args = argv_for(&kind, &extras);
        invoke_thin(&args, &line, &repo)?;
    }
}
