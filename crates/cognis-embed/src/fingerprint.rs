//! Model fingerprint for session isolation (Requirement 2.12).
//!
//! Two ONNX/embedder sessions may be shared **only** when their fingerprints
//! match. The fingerprint is derived from:
//!
//! 1. **Immutable asset checksums** — the SHA-256 of each model asset
//!    (`model.onnx`, `tokenizer.json`, `pooling.json`). When a `.sha256`
//!    sidecar is present (the same values verified in
//!    `apps/cognis-vscode/src/model.ts`), that published digest is preferred;
//!    otherwise the file contents are hashed. Missing assets contribute a
//!    stable `missing` marker so a no-model process still has a well-defined
//!    fingerprint.
//! 2. **Backend id** — `cfg.embedder.backend` (`stub` / `local` / `onnx-local`).
//! 3. **Embedding dimension** — `cfg.embedder.dim`.
//! 4. **Config identity** — `cfg.embedder.model` (model id) + `batch_size`, so a
//!    config change that would alter embedding behaviour cannot reuse a
//!    session loaded under a different config.
//!
//! Session reuse is allowed **iff** fingerprints are equal
//! ([`ModelFingerprint::allows_session_reuse`]).
//!
//! Task 8.2 / Correctness Property 12; preservation 3.6.

use std::path::{Path, PathBuf};

use cognis_core::Config;
use sha2::{Digest, Sha256};

/// Files the `onnx-local` backend loads from a model asset directory.
/// Order is fixed so the fingerprint is stable across platforms.
pub const MODEL_ASSET_FILES: &[&str] = &["model.onnx", "tokenizer.json", "pooling.json"];

/// HTTP header a client presents to declare its model fingerprint.
pub const MODEL_FINGERPRINT_HEADER: &str = "X-Cognis-Model-Fingerprint";

/// Env var that can pin an explicit model-fingerprint override (tests / ops).
pub const MODEL_FINGERPRINT_ENV: &str = "COGNIS_MODEL_FINGERPRINT";

/// A model fingerprint: opaque hex digest of the identity material above.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelFingerprint {
    /// Lowercase hex SHA-256 of the canonical fingerprint material.
    pub digest: String,
}

impl ModelFingerprint {
    /// Build from an already-computed lowercase hex digest.
    pub fn from_digest(digest: impl Into<String>) -> Self {
        ModelFingerprint {
            digest: digest.into().trim().to_ascii_lowercase(),
        }
    }

    /// Derive the fingerprint for `cfg`, hashing assets under the resolved
    /// model directory (see [`resolve_model_dir_for_fingerprint`]).
    pub fn derive(cfg: &Config) -> Self {
        let dir = resolve_model_dir_for_fingerprint(&cfg.embedder.model);
        Self::derive_with_model_dir(cfg, &dir)
    }

    /// Derive with an explicit model-asset directory (tests + callers that
    /// already resolved `COGNIS_ONNX_MODEL_DIR`).
    pub fn derive_with_model_dir(cfg: &Config, model_dir: &Path) -> Self {
        let material = fingerprint_material(cfg, model_dir);
        let mut hasher = Sha256::new();
        hasher.update(material.as_bytes());
        ModelFingerprint {
            digest: hex_lower(&hasher.finalize()),
        }
    }

    /// Resolve from env override if set, else derive from config + model dir.
    pub fn from_env_or_derive(cfg: &Config) -> Self {
        match std::env::var(MODEL_FINGERPRINT_ENV) {
            Ok(v) if !v.trim().is_empty() => Self::from_digest(v),
            _ => Self::derive(cfg),
        }
    }

    /// Wire form (the digest itself).
    pub fn as_str(&self) -> &str {
        &self.digest
    }

    /// Session reuse is allowed **iff** fingerprints are equal.
    pub fn allows_session_reuse(&self, other: &ModelFingerprint) -> bool {
        !self.digest.is_empty() && !other.digest.is_empty() && self.digest == other.digest
    }
}

/// True when a session owned by `owner` may be reused by a client presenting
/// `presented`. Equal fingerprints only.
pub fn session_reuse_allowed(owner: &ModelFingerprint, presented: &ModelFingerprint) -> bool {
    owner.allows_session_reuse(presented)
}

/// Canonical fingerprint material (newline-separated `key=value` lines).
///
/// Exposed for tests that want to assert the component set without hashing.
pub fn fingerprint_material(cfg: &Config, model_dir: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "backend={}",
        normalize_backend(&cfg.embedder.backend)
    ));
    lines.push(format!("dim={}", cfg.embedder.dim));
    lines.push(format!("model={}", cfg.embedder.model.trim()));
    lines.push(format!("batch_size={}", cfg.embedder.batch_size));
    for asset in MODEL_ASSET_FILES {
        let digest = asset_checksum(model_dir, asset);
        lines.push(format!("asset.{asset}={digest}"));
    }
    lines.join("\n")
}

/// Resolve the model asset directory the same way the ONNX backend does, so
/// the fingerprint and the loaded session always look at the same files.
///
/// Precedence:
/// 1. `COGNIS_ONNX_MODEL_DIR`
/// 2. `assets/models/<model-leaf>` next to the executable
/// 3. `assets/models/<model-leaf>` relative to cwd
pub fn resolve_model_dir_for_fingerprint(model: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("COGNIS_ONNX_MODEL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let leaf = model.rsplit('/').next().unwrap_or(model);
    let rel = PathBuf::from("assets").join("models").join(leaf);
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let next_to_exe = exe_dir.join(&rel);
        if next_to_exe.exists() {
            return next_to_exe;
        }
    }
    rel
}

/// Prefer a published `.sha256` sidecar (same values `model.ts` verifies);
/// otherwise hash the file contents; missing files yield `missing`.
fn asset_checksum(model_dir: &Path, asset: &str) -> String {
    let sidecar = model_dir.join(format!("{asset}.sha256"));
    if let Ok(text) = std::fs::read_to_string(&sidecar) {
        if let Some(hex) = parse_sha256_sidecar(&text) {
            return hex;
        }
    }
    let path = model_dir.join(asset);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            hex_lower(&hasher.finalize())
        }
        Err(_) => "missing".to_string(),
    }
}

/// Parse a `sha256sum -c` sidecar or a bare 64-hex digest (mirrors
/// `parseSha256Sidecar` in the extension).
pub fn parse_sha256_sidecar(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    // `sha256sum` formats: `<hex>  <name>` or `<hex> *<name>`; also bare hex.
    let hex = line
        .split_whitespace()
        .next()
        .unwrap_or(line)
        .trim()
        .to_ascii_lowercase();
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex)
    } else {
        None
    }
}

/// Normalize backend aliases so `local` and `onnx-local` fingerprint the same
/// (they select the same native backend).
fn normalize_backend(backend: &str) -> &str {
    match backend.trim() {
        "local" | "onnx-local" => "onnx-local",
        other => other,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_model_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("cognis-fp-{}-{}", std::process::id(), nanos));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cfg(backend: &str, dim: u32, model: &str) -> Config {
        let mut c = Config::default();
        c.embedder.backend = backend.into();
        c.embedder.dim = dim;
        c.embedder.model = model.into();
        c
    }

    #[test]
    fn equal_inputs_yield_equal_fingerprints() {
        let dir = tmp_model_dir();
        fs::write(dir.join("model.onnx"), b"onnx-bytes").unwrap();
        fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        fs::write(
            dir.join("pooling.json"),
            b"{\"pooling_mode_cls_token\":true}",
        )
        .unwrap();
        let c = cfg("stub", 384, "BAAI/bge-small-en-v1.5");
        let a = ModelFingerprint::derive_with_model_dir(&c, &dir);
        let b = ModelFingerprint::derive_with_model_dir(&c, &dir);
        assert_eq!(a, b);
        assert!(a.allows_session_reuse(&b));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dimension_change_refuses_session_reuse() {
        let dir = tmp_model_dir();
        fs::write(dir.join("model.onnx"), b"onnx-bytes").unwrap();
        let a = ModelFingerprint::derive_with_model_dir(&cfg("stub", 384, "m"), &dir);
        let b = ModelFingerprint::derive_with_model_dir(&cfg("stub", 768, "m"), &dir);
        assert_ne!(a.digest, b.digest);
        assert!(!a.allows_session_reuse(&b));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn backend_change_refuses_session_reuse() {
        let dir = tmp_model_dir();
        let a = ModelFingerprint::derive_with_model_dir(&cfg("stub", 384, "m"), &dir);
        let b = ModelFingerprint::derive_with_model_dir(&cfg("onnx-local", 384, "m"), &dir);
        assert_ne!(a.digest, b.digest);
        assert!(!session_reuse_allowed(&a, &b));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn local_and_onnx_local_are_aliases() {
        let dir = tmp_model_dir();
        let a = ModelFingerprint::derive_with_model_dir(&cfg("local", 384, "m"), &dir);
        let b = ModelFingerprint::derive_with_model_dir(&cfg("onnx-local", 384, "m"), &dir);
        assert_eq!(a, b);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn asset_checksum_change_refuses_session_reuse() {
        let dir = tmp_model_dir();
        fs::write(dir.join("model.onnx"), b"v1").unwrap();
        let a = ModelFingerprint::derive_with_model_dir(&cfg("stub", 384, "m"), &dir);
        fs::write(dir.join("model.onnx"), b"v2").unwrap();
        let b = ModelFingerprint::derive_with_model_dir(&cfg("stub", 384, "m"), &dir);
        assert_ne!(a.digest, b.digest);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_checksum_is_preferred_over_file_hash() {
        let dir = tmp_model_dir();
        fs::write(dir.join("model.onnx"), b"actual-bytes").unwrap();
        let published = "a".repeat(64);
        fs::write(
            dir.join("model.onnx.sha256"),
            format!("{published}  model.onnx\n"),
        )
        .unwrap();
        let material = fingerprint_material(&cfg("stub", 384, "m"), &dir);
        assert!(
            material.contains(&format!("asset.model.onnx={published}")),
            "material should use sidecar digest: {material}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parse_sha256_sidecar_formats() {
        let hex = "b".repeat(64);
        assert_eq!(
            parse_sha256_sidecar(&format!("{hex}  name")),
            Some(hex.clone())
        );
        assert_eq!(
            parse_sha256_sidecar(&format!("{hex} *name")),
            Some(hex.clone())
        );
        assert_eq!(parse_sha256_sidecar(&hex), Some(hex));
        assert_eq!(parse_sha256_sidecar("not-a-checksum"), None);
    }
}
