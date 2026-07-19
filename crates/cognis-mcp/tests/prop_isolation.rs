//! Property-based + unit tests for loopback, repository-identity, and
//! model-fingerprint isolation on shared MCP routes.
//!
//! Feature: mcp-process-ram-duplication
//! **Property 12: Bug Condition** — Loopback, repository-identity, and
//! model-fingerprint isolation
//!
//! **Validates: Requirements 2.12**
//!
//! _For any_ daemon/proxy/broker that accepts or routes work, the fixed system
//! binds only to loopback by default, authenticates each route with an
//! unguessable scoped credential where supported, canonicalizes and verifies
//! repository/DB identity per attachment, rejects cross-repository access,
//! derives a model fingerprint from immutable asset checksums plus
//! backend/dimension/config identity, and prohibits session reuse when
//! fingerprints differ.
//!
//! Coverage:
//! * Property-based (fingerprint isolation): for random asset/backend/dimension
//!   permutations, session reuse occurs **iff** fingerprints are equal.
//! * Unit: loopback-only default, credential-required, repo-identity +
//!   fingerprint rejection paths on the live HTTP transport.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cognis_core::{
    verify_repo_attachment, verify_repo_wire_key, Config, Hit, RepoIdentity, Result, Symbol,
    REPO_IDENTITY_HEADER,
};
use cognis_embed::{
    session_reuse_allowed, ModelFingerprint, MODEL_ASSET_FILES, MODEL_FINGERPRINT_HEADER,
};
use cognis_mcp::engine::RetrievalEngine;
use cognis_mcp::http::{
    bind, bind_with, is_loopback_host, serve_listener_with, BindOptions, HttpServeConfig,
    RouteCredential,
};
use cognis_mcp::server::McpServer;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Fingerprint input algebra (Property 12)
// ---------------------------------------------------------------------------

/// One complete fingerprint input: embedder config fields + per-asset payloads
/// (file body and optional published `.sha256` sidecar).
#[derive(Debug, Clone)]
struct FingerprintInput {
    backend: String,
    dim: u32,
    model: String,
    batch_size: u32,
    /// Parallel to `MODEL_ASSET_FILES`: `(file_bytes, optional_sidecar_hex)`.
    assets: Vec<(Vec<u8>, Option<String>)>,
}

impl FingerprintInput {
    fn to_config(&self) -> Config {
        let mut c = Config::default();
        c.embedder.backend = self.backend.clone();
        c.embedder.dim = self.dim;
        c.embedder.model = self.model.clone();
        c.embedder.batch_size = self.batch_size;
        c
    }

    /// Materialize assets under a unique temp model directory and return it.
    fn materialize(&self) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cognis-p12-{}-{}-{}", std::process::id(), nanos, n));
        let _ = fs::create_dir_all(&dir);
        for (i, asset) in MODEL_ASSET_FILES.iter().enumerate() {
            let (bytes, sidecar) = &self.assets[i];
            // Empty bytes mean "asset absent" so the fingerprint uses `missing`.
            if !bytes.is_empty() {
                fs::write(dir.join(asset), bytes).expect("write asset");
            }
            if let Some(hex) = sidecar {
                fs::write(
                    dir.join(format!("{asset}.sha256")),
                    format!("{hex}  {asset}\n"),
                )
                .expect("write sidecar");
            }
        }
        dir
    }

    fn fingerprint(&self) -> (ModelFingerprint, PathBuf) {
        let dir = self.materialize();
        let fp = ModelFingerprint::derive_with_model_dir(&self.to_config(), &dir);
        (fp, dir)
    }
}

/// Backends that appear in production + one free-form id so the generator
/// covers both alias-normalization (`local` ≡ `onnx-local`) and arbitrary ids.
fn arb_backend() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("stub".to_string()),
        Just("local".to_string()),
        Just("onnx-local".to_string()),
        Just("remote".to_string()),
        "[a-z]{1,8}".prop_map(|s| s),
    ]
}

fn arb_dim() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(32u32),
        Just(64),
        Just(128),
        Just(256),
        Just(384),
        Just(512),
        Just(768),
        Just(1024),
        1u32..2049,
    ]
}

fn arb_model() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("BAAI/bge-small-en-v1.5".to_string()),
        Just("m".to_string()),
        Just("org/model-v1".to_string()),
        "[A-Za-z0-9_./-]{1,24}".prop_map(|s| s),
    ]
}

fn arb_batch() -> impl Strategy<Value = u32> {
    prop_oneof![Just(1u32), Just(8), Just(16), Just(32), Just(64), 1u32..257]
}

/// Asset body: empty (= missing), short fixed, or small random bytes.
fn arb_asset_body() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(Vec::new()),
        Just(b"onnx-v1".to_vec()),
        Just(b"onnx-v2".to_vec()),
        Just(b"{}".to_vec()),
        proptest::collection::vec(any::<u8>(), 1..16),
    ]
}

/// Optional published 64-hex sidecar (or none).
fn arb_sidecar() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        Just(Some("a".repeat(64))),
        Just(Some("b".repeat(64))),
        Just(Some("0123456789abcdef".repeat(4))),
        // Random valid 64-hex so two inputs almost never collide by chance.
        proptest::string::string_regex("[0-9a-f]{64}")
            .unwrap()
            .prop_map(Some),
    ]
}

fn arb_fingerprint_input() -> impl Strategy<Value = FingerprintInput> {
    (
        arb_backend(),
        arb_dim(),
        arb_model(),
        arb_batch(),
        arb_asset_body(),
        arb_asset_body(),
        arb_asset_body(),
        arb_sidecar(),
        arb_sidecar(),
        arb_sidecar(),
    )
        .prop_map(
            |(backend, dim, model, batch_size, a0, a1, a2, s0, s1, s2)| FingerprintInput {
                backend,
                dim,
                model,
                batch_size,
                assets: vec![(a0, s0), (a1, s1), (a2, s2)],
            },
        )
}

// ---------------------------------------------------------------------------
// Property 12 — fingerprint isolation (PBT)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Feature: mcp-process-ram-duplication, Property 12: Bug Condition —
    // Loopback, repository-identity, and model-fingerprint isolation
    // **Validates: Requirements 2.12**
    //
    // For any two random asset/backend/dimension/config permutations:
    // session reuse is allowed if and only if the derived fingerprints are
    // equal (and non-empty).
    #[test]
    fn prop12_session_reuse_iff_fingerprints_equal(
        left in arb_fingerprint_input(),
        right in arb_fingerprint_input(),
    ) {
        let (fp_l, dir_l) = left.fingerprint();
        let (fp_r, dir_r) = right.fingerprint();

        let equal = fp_l.digest == fp_r.digest && !fp_l.digest.is_empty();
        let reuse = session_reuse_allowed(&fp_l, &fp_r);

        prop_assert_eq!(
            reuse,
            equal,
            "session reuse must equal fingerprint equality; left={} right={}",
            fp_l.as_str(),
            fp_r.as_str()
        );
        // Reflexive: a session may always reuse itself.
        prop_assert!(
            session_reuse_allowed(&fp_l, &fp_l),
            "fingerprint must allow reuse with itself: {}",
            fp_l.as_str()
        );
        prop_assert!(
            session_reuse_allowed(&fp_r, &fp_r),
            "fingerprint must allow reuse with itself: {}",
            fp_r.as_str()
        );
        // Empty digests never authorize reuse (defense in depth).
        let empty = ModelFingerprint::from_digest("");
        prop_assert!(!session_reuse_allowed(&fp_l, &empty));
        prop_assert!(!session_reuse_allowed(&empty, &fp_r));
        prop_assert!(!session_reuse_allowed(&empty, &empty));

        let _ = fs::remove_dir_all(dir_l);
        let _ = fs::remove_dir_all(dir_r);
    }

    // Feature: mcp-process-ram-duplication, Property 12
    // **Validates: Requirements 2.12**
    //
    // Identical inputs (same material) always produce equal fingerprints and
    // permit session reuse — determinism of the fingerprint function.
    #[test]
    fn prop12_identical_inputs_always_reuse(
        input in arb_fingerprint_input(),
    ) {
        let (a, dir_a) = input.fingerprint();
        let (b, dir_b) = input.fingerprint();
        prop_assert_eq!(&a.digest, &b.digest);
        prop_assert!(session_reuse_allowed(&a, &b));
        prop_assert!(a.allows_session_reuse(&b));
        let _ = fs::remove_dir_all(dir_a);
        let _ = fs::remove_dir_all(dir_b);
    }

    // Feature: mcp-process-ram-duplication, Property 12
    // **Validates: Requirements 2.12**
    //
    // Changing any *observed* identity component refuses session reuse —
    // except the documented `local` ≡ `onnx-local` backend alias and the
    // documented sidecar-over-body rule (when a `.sha256` sidecar is present,
    // the file body is not part of the fingerprint material).
    #[test]
    fn prop12_component_change_refuses_reuse_except_aliases(
        base in arb_fingerprint_input(),
        which in 0usize..8,
        flip_backend in arb_backend(),
        flip_dim in arb_dim(),
        flip_model in arb_model(),
        flip_batch in arb_batch(),
        flip_body in arb_asset_body(),
        flip_sidecar in arb_sidecar(),
    ) {
        let mut other = base.clone();
        match which {
            0 => other.backend = flip_backend,
            1 => other.dim = flip_dim,
            2 => other.model = flip_model,
            3 => other.batch_size = flip_batch,
            4 => other.assets[0].0 = flip_body,
            5 => other.assets[1].0 = flip_body,
            6 => other.assets[2].0 = flip_body,
            _ => {
                // Flip a sidecar on a random asset slot.
                let slot = which % 3;
                other.assets[slot].1 = flip_sidecar;
            }
        }

        let (fp_a, dir_a) = base.fingerprint();
        let (fp_b, dir_b) = other.fingerprint();
        let reuse = session_reuse_allowed(&fp_a, &fp_b);

        // Effective equality mirrors production fingerprint material:
        // * backend aliases local ≡ onnx-local
        // * when a sidecar is present for an asset, the body is ignored
        fn norm_backend(b: &str) -> &str {
            match b.trim() {
                "local" | "onnx-local" => "onnx-local",
                other => other,
            }
        }
        // Sidecar present → body is ignored (production prefers `.sha256`).
        // Sidecar absent + empty body → `missing`.
        // Sidecar absent + non-empty body → full body bytes matter.
        fn assets_effectively_equal(
            a: &[(Vec<u8>, Option<String>)],
            b: &[(Vec<u8>, Option<String>)],
        ) -> bool {
            a.iter().zip(b.iter()).all(|((ab, asid), (bb, bsid))| {
                match (asid, bsid) {
                    (Some(ah), Some(bh)) => ah == bh,
                    (Some(_), None) | (None, Some(_)) => false,
                    (None, None) => ab == bb, // both missing or both same body
                }
            })
        }
        let same_backend = norm_backend(&base.backend) == norm_backend(&other.backend);
        let same_cfg = base.dim == other.dim
            && base.model == other.model
            && base.batch_size == other.batch_size;
        let same_assets = assets_effectively_equal(&base.assets, &other.assets);
        let effectively_equal = same_backend && same_cfg && same_assets;

        if effectively_equal {
            prop_assert!(
                reuse,
                "effectively identical material must allow reuse; a={} b={}",
                fp_a.as_str(),
                fp_b.as_str()
            );
            prop_assert_eq!(&fp_a.digest, &fp_b.digest);
        } else {
            prop_assert!(
                !reuse,
                "differing effective material must refuse session reuse; a={} b={}",
                fp_a.as_str(),
                fp_b.as_str()
            );
            prop_assert_ne!(&fp_a.digest, &fp_b.digest);
        }

        let _ = fs::remove_dir_all(dir_a);
        let _ = fs::remove_dir_all(dir_b);
    }
}

// ---------------------------------------------------------------------------
// Live HTTP helpers for isolation unit tests
// ---------------------------------------------------------------------------

/// Minimal engine — isolation checks run before any tool dispatch.
struct EmptyEngine;

impl RetrievalEngine for EmptyEngine {
    fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
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

/// POST `/mcp` with optional Authorization / repo-key / model-fingerprint headers.
fn post_with_headers(
    port: u16,
    token: Option<&str>,
    repo_key: Option<&str>,
    fingerprint: Option<&str>,
) -> (u16, String) {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"symbol_search","arguments":{"query":"x","k":1}}}"#;
    let mut headers = String::from(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nConnection: close\r\n",
    );
    if let Some(t) = token {
        headers.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if let Some(k) = repo_key {
        headers.push_str(&format!("{REPO_IDENTITY_HEADER}: {k}\r\n"));
    }
    if let Some(fp) = fingerprint {
        headers.push_str(&format!("{MODEL_FINGERPRINT_HEADER}: {fp}\r\n"));
    }
    headers.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));

    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(_) => return (0, String::new()),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    if stream.write_all(headers.as_bytes()).is_err() {
        return (0, String::new());
    }
    stream.flush().ok();
    let mut buf = Vec::new();
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

fn spawn_server(cfg: HttpServeConfig) -> u16 {
    let listener = bind("127.0.0.1", 0).expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let server = McpServer::new(EmptyEngine);
    thread::spawn(move || {
        let _ = serve_listener_with(&server, listener, cfg);
    });
    // Give the acceptor a moment to start.
    thread::sleep(Duration::from_millis(40));
    port
}

fn tmp_repo(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p12-repo-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Unit: loopback-only default (Property 12 / Requirement 2.12)
// ---------------------------------------------------------------------------

/// Unit: known loopback hosts are accepted; wildcards and LAN/public IPs are not.
#[test]
fn unit_prop12_loopback_only_default() {
    // Accepted by default.
    assert!(is_loopback_host("127.0.0.1"));
    assert!(is_loopback_host("localhost"));
    assert!(is_loopback_host("::1"));

    // Rejected by default (non-loopback).
    assert!(!is_loopback_host("0.0.0.0"));
    assert!(!is_loopback_host("::"));
    assert!(!is_loopback_host("8.8.8.8"));
    assert!(!is_loopback_host("192.168.1.1"));
    assert!(!is_loopback_host("10.0.0.1"));
    assert!(!is_loopback_host(""));

    // bind_with rejects non-loopback under the default policy.
    let err = bind_with("0.0.0.0", 0, BindOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

    // Explicit opt-in is the only way to bind non-loopback.
    let listener = bind_with(
        "0.0.0.0",
        0,
        BindOptions {
            allow_non_loopback: true,
        },
    )
    .expect("opt-in non-loopback bind must succeed");
    drop(listener);

    // Default bind to loopback always works.
    let loopback = bind("127.0.0.1", 0).expect("loopback bind");
    drop(loopback);
}

// ---------------------------------------------------------------------------
// Unit: credential-required (Property 12 / Requirement 2.12)
// ---------------------------------------------------------------------------

/// Unit: every shared HTTP route requires the unguessable scoped credential.
#[test]
fn unit_prop12_credential_required() {
    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    // Sanity: generated credentials are long enough to be unguessable.
    assert!(
        token.len() >= 32,
        "generated route credential too short: {}",
        token.len()
    );

    let port = spawn_server(isolation_config(cred, None, None));

    // Missing credential → 401.
    let (status, body) = post_with_headers(port, None, None, None);
    assert_eq!(status, 401, "missing credential must 401; body={body}");
    assert!(
        body.contains("UNAUTHORIZED") || body.contains("route credential"),
        "body={body}"
    );

    // Wrong credential → 401.
    let (status, body) =
        post_with_headers(port, Some("definitely-not-the-right-token!!"), None, None);
    assert_eq!(status, 401, "wrong credential must 401; body={body}");

    // Correct credential → authorized (200 tool response).
    let (status, body) = post_with_headers(port, Some(&token), None, None);
    assert_eq!(status, 200, "valid credential must authorize; body={body}");
}

// ---------------------------------------------------------------------------
// Unit: repo-identity + fingerprint rejection paths (Property 12)
// ---------------------------------------------------------------------------

/// Unit: mismatched `X-Cognis-Repo-Key` is rejected with 403 CROSS_REPOSITORY.
#[test]
fn unit_prop12_repo_identity_rejection_path() {
    let owner_root = tmp_repo("owner");
    let other_root = tmp_repo("other");
    let owner = RepoIdentity::from_paths(&owner_root, RepoIdentity::default_db_path(&owner_root));
    let other = RepoIdentity::from_paths(&other_root, RepoIdentity::default_db_path(&other_root));
    assert!(!owner.same_as(&other));
    assert!(!verify_repo_attachment(&owner, &other).is_allowed());
    assert!(!verify_repo_wire_key(&owner, &other.wire_key()).is_allowed());
    assert!(verify_repo_wire_key(&owner, &owner.wire_key()).is_allowed());

    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    let owner_key = owner.wire_key();
    let other_key = other.wire_key();

    let port = spawn_server(isolation_config(cred, Some(owner), None));

    // Missing repo key → 403 cross-repository.
    let (status, body) = post_with_headers(port, Some(&token), None, None);
    assert_eq!(status, 403, "missing repo key must 403; body={body}");
    assert!(
        body.contains("CROSS_REPOSITORY") || body.contains("repo-identity-rejected"),
        "body={body}"
    );

    // Wrong repo key → 403.
    let (status, body) = post_with_headers(port, Some(&token), Some(&other_key), None);
    assert_eq!(status, 403, "wrong repo key must 403; body={body}");
    assert!(
        body.contains("CROSS_REPOSITORY") || body.contains("repo-identity-rejected"),
        "body={body}"
    );

    // Matching repo key → allowed (200).
    let (status, body) = post_with_headers(port, Some(&token), Some(&owner_key), None);
    assert_eq!(status, 200, "matching repo key must allow; body={body}");

    let _ = fs::remove_dir_all(owner_root);
    let _ = fs::remove_dir_all(other_root);
}

/// Unit: mismatched `X-Cognis-Model-Fingerprint` refuses session reuse (403).
#[test]
fn unit_prop12_fingerprint_rejection_path() {
    let owner_fp = ModelFingerprint::from_digest("a".repeat(64));
    let other_fp = ModelFingerprint::from_digest("b".repeat(64));
    assert!(!session_reuse_allowed(&owner_fp, &other_fp));
    assert!(session_reuse_allowed(&owner_fp, &owner_fp));

    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    let owner_digest = owner_fp.as_str().to_string();
    let other_digest = other_fp.as_str().to_string();

    let port = spawn_server(isolation_config(cred, None, Some(owner_fp)));

    // Missing fingerprint → 403 MODEL_FINGERPRINT_MISMATCH.
    let (status, body) = post_with_headers(port, Some(&token), None, None);
    assert_eq!(status, 403, "missing fingerprint must 403; body={body}");
    assert!(
        body.contains("MODEL_FINGERPRINT_MISMATCH") || body.contains("model-fingerprint-rejected"),
        "body={body}"
    );

    // Wrong fingerprint → 403.
    let (status, body) = post_with_headers(port, Some(&token), None, Some(&other_digest));
    assert_eq!(status, 403, "wrong fingerprint must 403; body={body}");
    assert!(
        body.contains("MODEL_FINGERPRINT_MISMATCH") || body.contains("model-fingerprint-rejected"),
        "body={body}"
    );

    // Matching fingerprint → allowed (200).
    let (status, body) = post_with_headers(port, Some(&token), None, Some(&owner_digest));
    assert_eq!(status, 200, "matching fingerprint must allow; body={body}");
}

/// Unit: both isolation headers required together when both are configured.
#[test]
fn unit_prop12_combined_repo_and_fingerprint_isolation() {
    let owner_root = tmp_repo("combo-owner");
    let other_root = tmp_repo("combo-other");
    let owner = RepoIdentity::from_paths(&owner_root, RepoIdentity::default_db_path(&owner_root));
    let other = RepoIdentity::from_paths(&other_root, RepoIdentity::default_db_path(&other_root));
    let owner_fp = ModelFingerprint::from_digest("c".repeat(64));
    let other_fp = ModelFingerprint::from_digest("d".repeat(64));

    let cred = RouteCredential::generate();
    let token = cred.as_str().to_string();
    let owner_key = owner.wire_key();
    let other_key = other.wire_key();
    let owner_digest = owner_fp.as_str().to_string();
    let other_digest = other_fp.as_str().to_string();

    let port = spawn_server(isolation_config(cred, Some(owner), Some(owner_fp)));

    // Wrong repo, correct fingerprint → still 403 (repo checked first).
    let (status, body) =
        post_with_headers(port, Some(&token), Some(&other_key), Some(&owner_digest));
    assert_eq!(status, 403, "wrong repo must 403; body={body}");
    assert!(body.contains("CROSS_REPOSITORY") || body.contains("repo-identity-rejected"));

    // Correct repo, wrong fingerprint → 403 fingerprint.
    let (status, body) =
        post_with_headers(port, Some(&token), Some(&owner_key), Some(&other_digest));
    assert_eq!(status, 403, "wrong fingerprint must 403; body={body}");
    assert!(
        body.contains("MODEL_FINGERPRINT_MISMATCH") || body.contains("model-fingerprint-rejected")
    );

    // Both correct → 200.
    let (status, body) =
        post_with_headers(port, Some(&token), Some(&owner_key), Some(&owner_digest));
    assert_eq!(
        status, 200,
        "matching isolation headers must allow; body={body}"
    );

    // Wrong credential still wins over isolation (401 before 403).
    let (status, body) = post_with_headers(
        port,
        Some("wrong-token-xxxxxxxx"),
        Some(&owner_key),
        Some(&owner_digest),
    );
    assert_eq!(status, 401, "wrong credential must 401 first; body={body}");

    let _ = fs::remove_dir_all(owner_root);
    let _ = fs::remove_dir_all(other_root);
}

// ---------------------------------------------------------------------------
// HttpServeConfig test helper
// ---------------------------------------------------------------------------

/// Small isolation-focused serve config for unit tests.
fn isolation_config(
    credential: RouteCredential,
    repo_identity: Option<RepoIdentity>,
    model_fingerprint: Option<ModelFingerprint>,
) -> HttpServeConfig {
    HttpServeConfig {
        worker_count: 1,
        queue_capacity: 2,
        request_timeout: Duration::from_secs(3),
        overload_retry_after_secs: 1,
        route_credential: Some(credential),
        repo_identity,
        model_fingerprint,
    }
}
