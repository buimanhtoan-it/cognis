//! `cargo xtask` — build / distribution automation (Task 10.1).
//!
//! The shipped product (design G2) is **one static `cognis` binary per
//! platform**: SQLite is bundled (`rusqlite` `bundled` feature compiles SQLite
//! with FTS5 into the binary — no system SQLite, Requirement 8.2), and the
//! binary dispatches busybox-style to the CLI / mcpd / indexd surfaces.
//!
//! `xtask dist` stages an artifact locally, while `xtask verify-release` is the
//! release gate for Task 14.1:
//!
//! ```text
//! cargo xtask dist                                       # build + stage host artifact
//! cargo xtask verify-release --features release-full     # build/stage + verify the shipped build
//! cargo xtask verify-release --target <triple>           # verify a specific target
//! cargo xtask verify-release --use-cross --target <t>    # Linux cross-compile gate
//! ```
//!
//! Both commands build `--release -p cognis`, copy the artifact to `dist/`
//! under a platform-tagged name, compute its SHA-256, and write a
//! `<name>.sha256` sidecar. `verify-release` goes further: it fails if checksum
//! publication is wrong, if the produced binary reports either release capability
//! absent, or if the handshake payload reports a `contract_version` different
//! from `cognis_core::CONTRACT_VERSION`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use cognis_core::CONTRACT_VERSION;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(name = "xtask", about = "cognis build/dist automation")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the single `cognis` binary in --release and stage it under dist/.
    Dist(DistArgs),
    /// Build/stage the release artifact and verify its checksum + reported capabilities.
    VerifyRelease(VerifyReleaseArgs),
}

#[derive(Args, Debug, Clone)]
struct DistArgs {
    /// Target triple to build for (default: the host triple). When set with
    /// `--use-cross` the Linux cross targets build without a local toolchain.
    #[arg(long)]
    target: Option<String>,
    /// Use `cross` instead of `cargo` (Linux cross-compile in CI).
    #[arg(long)]
    use_cross: bool,
    /// Extra cargo features for the `cognis` binary (for the shipped artifact,
    /// use `release-full`). Comma/space separated.
    #[arg(long)]
    features: Option<String>,
    /// Output directory for staged artifacts.
    #[arg(long, default_value = "dist")]
    out: PathBuf,
}

#[derive(Args, Debug, Clone)]
struct VerifyReleaseArgs {
    #[command(flatten)]
    dist: DistArgs,
    /// Reuse an already staged artifact instead of rebuilding it first.
    #[arg(long, default_value_t = false)]
    skip_build: bool,
}

#[derive(Debug, Clone)]
struct StagedArtifact {
    target: String,
    staged_name: String,
    binary_path: PathBuf,
    sidecar_path: PathBuf,
    digest: String,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Dist(args) => dist_command(args),
        Cmd::VerifyRelease(args) => verify_release_command(args),
    }
}

fn dist_command(args: DistArgs) -> Result<()> {
    let artifact = stage_dist_artifact(&args)?;
    print_artifact_summary(&artifact)
}

fn verify_release_command(args: VerifyReleaseArgs) -> Result<()> {
    let artifact = if args.skip_build {
        locate_staged_artifact(&args.dist)?
    } else {
        stage_dist_artifact(&args.dist)?
    };
    print_artifact_summary(&artifact)?;
    verify_checksum_publication(&artifact)?;
    verify_release_binary_reports_truthful_capabilities(&artifact.binary_path)?;
    eprintln!(
        "[xtask] release verification passed for {}",
        artifact.binary_path.display()
    );
    Ok(())
}

fn stage_dist_artifact(args: &DistArgs) -> Result<StagedArtifact> {
    let target = resolve_target(args.target.as_deref())?;
    build_release_binary(&target, args)?;
    let built = built_binary_path(&target)?;
    if !built.exists() {
        bail!("expected binary not found: {}", built.display());
    }
    stage_built_binary(&target, &built, &args.out)
}

fn locate_staged_artifact(args: &DistArgs) -> Result<StagedArtifact> {
    let target = resolve_target(args.target.as_deref())?;
    let staged_name = staged_name(&target);
    let binary_path = args.out.join(&staged_name);
    let sidecar_path = args.out.join(format!("{staged_name}.sha256"));
    if !binary_path.exists() {
        bail!("staged binary not found: {}", binary_path.display());
    }
    if !sidecar_path.exists() {
        bail!("checksum sidecar not found: {}", sidecar_path.display());
    }
    Ok(StagedArtifact {
        target,
        staged_name,
        digest: sha256_file(&binary_path)?,
        binary_path,
        sidecar_path,
    })
}

fn build_release_binary(target: &str, args: &DistArgs) -> Result<()> {
    // Build only the multi-call binary; the three standalone bins (fallback B)
    // come from the same crates and are not needed for the shipped artifact.
    let tool = if args.use_cross { "cross" } else { "cargo" };
    let mut cmd = Command::new(tool);
    cmd.args(["build", "--release", "-p", "cognis", "--bin", "cognis"]);
    cmd.args(["--target", target]);
    if let Some(features) = &args.features {
        cmd.args(["--features", features]);
    }
    eprintln!("[xtask] {tool} build --release -p cognis --target {target}");
    let status = cmd.status().with_context(|| format!("spawning {tool}"))?;
    if !status.success() {
        bail!("{tool} build failed for target {target}");
    }
    Ok(())
}

fn built_binary_path(target: &str) -> Result<PathBuf> {
    Ok(workspace_root()?
        .join("target")
        .join(target)
        .join("release")
        .join(exe_name(target)))
}

fn stage_built_binary(target: &str, built: &Path, out: &Path) -> Result<StagedArtifact> {
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let staged_name = staged_name(target);
    let binary_path = out.join(&staged_name);
    fs::copy(built, &binary_path)
        .with_context(|| format!("copying {} -> {}", built.display(), binary_path.display()))?;

    let digest = sha256_file(&binary_path)?;
    let sidecar_path = out.join(format!("{staged_name}.sha256"));
    // `<sha256>  <filename>` — the format `sha256sum -c` consumes.
    fs::write(&sidecar_path, format!("{digest}  {staged_name}\n"))
        .with_context(|| format!("writing {}", sidecar_path.display()))?;

    Ok(StagedArtifact {
        target: target.to_string(),
        staged_name,
        binary_path,
        sidecar_path,
        digest,
    })
}

fn print_artifact_summary(artifact: &StagedArtifact) -> Result<()> {
    let bytes = fs::metadata(&artifact.binary_path)
        .with_context(|| format!("stat {}", artifact.binary_path.display()))?
        .len();
    eprintln!(
        "[xtask] staged {} ({bytes} bytes)",
        artifact.binary_path.display()
    );
    eprintln!("[xtask] target  {}", artifact.target);
    eprintln!("[xtask] sha256  {}", artifact.digest);
    Ok(())
}

fn verify_checksum_publication(artifact: &StagedArtifact) -> Result<()> {
    let published = fs::read_to_string(&artifact.sidecar_path)
        .with_context(|| format!("reading {}", artifact.sidecar_path.display()))?;
    let (published_digest, published_name) = parse_sha256_sidecar(&published)?;
    if published_name != artifact.staged_name {
        bail!(
            "checksum sidecar names `{published_name}`, expected `{}`",
            artifact.staged_name
        );
    }
    if published_digest != artifact.digest {
        bail!(
            "checksum sidecar digest mismatch for {}: published {}, actual {}",
            artifact.staged_name,
            published_digest,
            artifact.digest
        );
    }
    Ok(())
}

fn parse_sha256_sidecar(text: &str) -> Result<(String, String)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("checksum sidecar is empty");
    }
    let mut parts = trimmed.split_whitespace();
    let digest = parts.next().context("checksum sidecar missing digest")?;
    let filename = parts.next().context("checksum sidecar missing filename")?;
    if parts.next().is_some() {
        bail!("checksum sidecar must contain exactly one digest and filename");
    }
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("checksum sidecar digest is not a 64-char hex string: {digest}");
    }
    Ok((digest.to_string(), filename.to_string()))
}

fn verify_release_binary_reports_truthful_capabilities(binary: &Path) -> Result<()> {
    std::env::remove_var("COGNIS_SQLITE_VEC_PATH");
    let handshake = run_json_command(binary, &["cli", "handshake"], false)?;
    let handshake_caps = verify_handshake_payload(&handshake)?;

    let repo = fresh_temp_repo()?;
    let repo_str = repo.to_string_lossy().into_owned();
    let health = run_json_command(
        binary,
        &["cli", "--repo-root", repo_str.as_str(), "health", "--json"],
        true,
    )?;
    let health_caps = capabilities_from_surface(&health, "health")?;
    let _ = fs::remove_dir_all(&repo);

    if handshake_caps != health_caps {
        bail!(
            "handshake/health capability mismatch: handshake={:?}, health={:?}",
            handshake_caps,
            health_caps
        );
    }
    Ok(())
}

/// Fail the release when the handshake `contract_version` does not match the
/// compiled contract, or when either release capability is absent
/// (Requirement 6.6, 6.7).
fn verify_handshake_payload(handshake: &Value) -> Result<(bool, bool)> {
    if handshake["contract_version"].as_u64() != Some(CONTRACT_VERSION as u64) {
        bail!(
            "handshake contract_version mismatch: expected {}, got {}",
            CONTRACT_VERSION,
            handshake["contract_version"]
        );
    }
    capabilities_from_surface(handshake, "handshake")
}

fn capabilities_from_surface(surface: &Value, label: &str) -> Result<(bool, bool)> {
    let caps = surface
        .get("capabilities")
        .and_then(Value::as_object)
        .with_context(|| format!("{label} payload is missing a capabilities object"))?;
    let semantic_backend = caps
        .get("semantic_backend")
        .and_then(Value::as_bool)
        .with_context(|| format!("{label} capabilities.semantic_backend is missing"))?;
    let accelerated_vector_search = caps
        .get("accelerated_vector_search")
        .and_then(Value::as_bool)
        .with_context(|| format!("{label} capabilities.accelerated_vector_search is missing"))?;
    if !semantic_backend || !accelerated_vector_search {
        bail!(
            "{label} reports release capability absent: semantic_backend={semantic_backend}, accelerated_vector_search={accelerated_vector_search}"
        );
    }
    Ok((semantic_backend, accelerated_vector_search))
}

fn run_json_command(binary: &Path, args: &[&str], allow_failure: bool) -> Result<Value> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("running {} {:?}", binary.display(), args))?;
    let stdout = String::from_utf8(output.stdout).context("command stdout was not utf-8")?;
    let stderr = String::from_utf8(output.stderr).context("command stderr was not utf-8")?;
    if !output.status.success() && !allow_failure {
        bail!(
            "command {} {:?} failed with {}: {}",
            binary.display(),
            args,
            output.status,
            stderr.trim()
        );
    }
    serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "parsing JSON from {} {:?} (status={}, stderr={})",
            binary.display(),
            args,
            output.status,
            stderr.trim()
        )
    })
}

fn fresh_temp_repo() -> Result<PathBuf> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cognis-release-verify-{nonce}"));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn resolve_target(explicit: Option<&str>) -> Result<String> {
    match explicit {
        Some(target) => Ok(target.to_string()),
        None => host_triple().context("resolving host target triple"),
    }
}

/// Platform-tagged artifact name, e.g. `cognis-x86_64-unknown-linux-gnu` or
/// `cognis-x86_64-pc-windows-msvc.exe`.
fn staged_name(target: &str) -> String {
    if target.contains("windows") {
        format!("cognis-{target}.exe")
    } else {
        format!("cognis-{target}")
    }
}

fn exe_name(target: &str) -> String {
    if target.contains("windows") {
        "cognis.exe".to_string()
    } else {
        "cognis".to_string()
    }
}

/// The workspace root = the directory two levels up from this file's crate.
/// `CARGO_MANIFEST_DIR` points at xtask/, whose parent is the workspace root.
fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest has no parent (workspace root)")
}

/// Resolve the host target triple from `rustc -vV` (the `host:` line).
fn host_triple() -> Result<String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("running rustc -vV")?;
    if !out.status.success() {
        bail!("rustc -vV failed");
    }
    let text = String::from_utf8(out.stdout).context("rustc -vV output not utf-8")?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("host: ") {
            return Ok(rest.trim().to_string());
        }
    }
    bail!("no `host:` line in rustc -vV output")
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_core::{handshake_payload, handshake_payload_with_capabilities, Capabilities};
    use serde_json::json;

    fn full_release_capabilities() -> Capabilities {
        Capabilities {
            semantic_backend: true,
            accelerated_vector_search: true,
        }
    }

    fn handshake_json(caps: Capabilities) -> Value {
        serde_json::to_value(handshake_payload_with_capabilities(caps)).unwrap()
    }

    fn temp_stage_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "cognis-xtask-smoke-{}-{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn staged_artifact_with_sidecar(
        bytes: &[u8],
        sidecar: Option<&str>,
    ) -> (PathBuf, StagedArtifact) {
        let dir = temp_stage_dir();
        let staged_name = "cognis-test-target".to_string();
        let binary_path = dir.join(&staged_name);
        fs::write(&binary_path, bytes).unwrap();
        let digest = sha256_file(&binary_path).unwrap();
        let sidecar_path = dir.join(format!("{staged_name}.sha256"));
        let sidecar_text = sidecar
            .map(str::to_string)
            .unwrap_or_else(|| format!("{digest}  {staged_name}\n"));
        fs::write(&sidecar_path, sidecar_text).unwrap();
        (
            dir,
            StagedArtifact {
                target: "test-target".into(),
                staged_name,
                binary_path,
                sidecar_path,
                digest,
            },
        )
    }

    #[test]
    fn parse_sha256_sidecar_round_trips() {
        let parsed = parse_sha256_sidecar(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  cognis-x86_64-unknown-linux-gnu\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            (
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
                "cognis-x86_64-unknown-linux-gnu".to_string(),
            )
        );
    }

    #[test]
    fn release_handshake_reports_both_capabilities_present() {
        let payload = handshake_json(full_release_capabilities());
        assert_eq!(payload["capabilities"]["semantic_backend"], true);
        assert_eq!(payload["capabilities"]["accelerated_vector_search"], true);
        assert_eq!(verify_handshake_payload(&payload).unwrap(), (true, true));
    }

    #[test]
    fn release_verification_rejects_missing_semantic_backend() {
        let err = capabilities_from_surface(
            &json!({
                "capabilities": {
                    "semantic_backend": false,
                    "accelerated_vector_search": true
                }
            }),
            "handshake",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("release capability absent"), "{err}");
        assert!(err.contains("semantic_backend=false"), "{err}");
    }

    #[test]
    fn release_verification_rejects_missing_accelerated_vector_search() {
        let err = capabilities_from_surface(
            &json!({
                "capabilities": {
                    "semantic_backend": true,
                    "accelerated_vector_search": false
                }
            }),
            "handshake",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("release capability absent"), "{err}");
        assert!(err.contains("accelerated_vector_search=false"), "{err}");
    }

    #[test]
    fn release_verification_rejects_contract_version_mismatch() {
        let mut payload = handshake_json(full_release_capabilities());
        payload["contract_version"] = json!(CONTRACT_VERSION as u64 + 1);
        let err = verify_handshake_payload(&payload).unwrap_err().to_string();
        assert!(err.contains("contract_version mismatch"), "{err}");
    }

    #[test]
    fn checksum_publication_accepts_matching_sidecar() {
        let (dir, artifact) = staged_artifact_with_sidecar(b"cognis-release-bytes", None);
        verify_checksum_publication(&artifact).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn checksum_publication_rejects_digest_mismatch() {
        let wrong = format!("{}  cognis-test-target\n", "0".repeat(64));
        let (dir, artifact) = staged_artifact_with_sidecar(b"cognis-release-bytes", Some(&wrong));
        let err = verify_checksum_publication(&artifact)
            .unwrap_err()
            .to_string();
        assert!(err.contains("digest mismatch"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn checksum_publication_rejects_filename_mismatch() {
        let (dir, artifact) = staged_artifact_with_sidecar(b"cognis-release-bytes", None);
        let wrong = format!("{}  other-name\n", artifact.digest);
        fs::write(&artifact.sidecar_path, wrong).unwrap();
        let err = verify_checksum_publication(&artifact)
            .unwrap_err()
            .to_string();
        assert!(err.contains("checksum sidecar names"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn developer_handshake_fails_the_release_capability_gate() {
        let payload = serde_json::to_value(handshake_payload()).unwrap();
        assert_eq!(payload["capabilities"]["semantic_backend"], false);
        assert_eq!(payload["capabilities"]["accelerated_vector_search"], false);
        let err = verify_handshake_payload(&payload).unwrap_err().to_string();
        assert!(err.contains("release capability absent"), "{err}");
    }

    #[test]
    fn developer_cognis_package_stays_offline_and_release_full_enables_both() {
        let toml = fs::read_to_string(
            workspace_root()
                .unwrap()
                .join("bins")
                .join("cognis")
                .join("Cargo.toml"),
        )
        .unwrap();
        assert!(
            toml.contains("default = []"),
            "developer build must keep default features empty:\n{toml}"
        );
        assert!(
            toml.contains("release-full = [")
                && toml.contains("onnx-download")
                && toml.contains("cognis-store/sqlite-vec"),
            "release-full must enable onnx-download and sqlite-vec:\n{toml}"
        );
        assert!(
            !toml.contains("default = [\"release-full\"]")
                && !toml.contains("default = [\"onnx")
                && !toml.contains("default = [\"sqlite-vec"),
            "ONNX / sqlite-vec must not be default features:\n{toml}"
        );
    }

    #[test]
    fn staged_names_match_platform_convention() {
        assert_eq!(
            staged_name("x86_64-pc-windows-msvc"),
            "cognis-x86_64-pc-windows-msvc.exe"
        );
        assert_eq!(
            staged_name("x86_64-unknown-linux-gnu"),
            "cognis-x86_64-unknown-linux-gnu"
        );
    }
}
