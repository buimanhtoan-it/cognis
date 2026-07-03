//! `cargo xtask` — build / distribution automation (Task 10.1).
//!
//! The shipped product (design G2) is **one static `cognis` binary per
//! platform**: SQLite is bundled (`rusqlite` `bundled` feature compiles SQLite
//! with FTS5 into the binary — no system SQLite, Requirement 8.2), and the
//! binary dispatches busybox-style to the CLI / mcpd / indexd surfaces.
//!
//! `xtask dist` is the local half of the per-platform release matrix:
//!
//! ```text
//! cargo xtask dist                            # build the host target, stage it
//! cargo xtask dist --target <triple>          # build a specific target
//! cargo xtask dist --use-cross                # build via `cross` (Linux cross)
//! cargo xtask dist --features onnx-download   # self-contained ONNX runtime
//! ```
//!
//! It builds `--release -p cognis`, copies the artifact to `dist/` under a
//! platform-tagged name, computes its SHA-256, and writes a `<name>.sha256`
//! sidecar (public checksum — the trust posture the design calls for alongside
//! CI signing/provenance). CI runs the identical build per matrix target; this
//! command keeps `release` reproducible on a developer machine.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
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
}

#[derive(Parser)]
struct DistArgs {
    /// Target triple to build for (default: the host triple). When set with
    /// `--use-cross` the Linux cross targets build without a local toolchain.
    #[arg(long)]
    target: Option<String>,
    /// Use `cross` instead of `cargo` (Linux cross-compile in CI).
    #[arg(long)]
    use_cross: bool,
    /// Extra cargo features for the `cognis` binary (e.g. `onnx-download` for a
    /// self-contained ONNX runtime). Comma/space separated.
    #[arg(long)]
    features: Option<String>,
    /// Output directory for staged artifacts.
    #[arg(long, default_value = "dist")]
    out: PathBuf,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Dist(args) => dist(args),
    }
}

fn dist(args: DistArgs) -> Result<()> {
    let target = match args.target {
        Some(t) => t,
        None => host_triple().context("resolving host target triple")?,
    };

    // Build only the multi-call binary; the three standalone bins (fallback B)
    // come from the same crates and are not needed for the shipped artifact.
    let tool = if args.use_cross { "cross" } else { "cargo" };
    let mut cmd = Command::new(tool);
    cmd.args(["build", "--release", "-p", "cognis", "--bin", "cognis"]);
    cmd.args(["--target", &target]);
    if let Some(feats) = &args.features {
        cmd.args(["--features", feats]);
    }
    eprintln!("[xtask] {tool} build --release -p cognis --target {target}");
    let status = cmd.status().with_context(|| format!("spawning {tool}"))?;
    if !status.success() {
        bail!("{tool} build failed for target {target}");
    }

    // Locate the produced binary. With an explicit --target cargo nests the
    // output under target/<triple>/release/.
    let exe = exe_name(&target);
    let built = workspace_root()?
        .join("target")
        .join(&target)
        .join("release")
        .join(&exe);
    if !built.exists() {
        bail!("expected binary not found: {}", built.display());
    }

    fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;
    let staged_name = staged_name(&target);
    let staged = args.out.join(&staged_name);
    fs::copy(&built, &staged)
        .with_context(|| format!("copying {} -> {}", built.display(), staged.display()))?;

    let digest = sha256_file(&staged)?;
    let sidecar = args.out.join(format!("{staged_name}.sha256"));
    // `<sha256>  <filename>` — the format `sha256sum -c` consumes.
    fs::write(&sidecar, format!("{digest}  {staged_name}\n"))
        .with_context(|| format!("writing {}", sidecar.display()))?;

    let bytes = fs::metadata(&staged)?.len();
    eprintln!("[xtask] staged {} ({bytes} bytes)", staged.display());
    eprintln!("[xtask] sha256  {digest}");
    Ok(())
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
