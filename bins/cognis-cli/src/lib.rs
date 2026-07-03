//! cognis-cli — `clap` operator CLI (Task 7.3).
//!
//! Subcommands (the operator surface the extension drives): `init`, `index`,
//! `bootstrap`, `health`, `eval`, plus the contract-critical `handshake` the
//! extension reads at startup. The command
//! tree is `clap`-derive; each command delegates to a pure helper
//! ([`init`], [`build_health_report`], …) so the behaviour is unit-testable
//! without spawning a process.
//!
//! The entry points ([`run`] / [`run_from`]) return a [`std::process::ExitCode`]
//! and are the seam the single multi-call `cognis` binary dispatches into
//! (busybox-style, design "Single-binary distribution"). The native indexer
//! (Task 8) and eval harness (Task 9) are still landing, so `index`/`eval`
//! report their pending status rather than fabricating results — `init`,
//! `health` and `handshake` are fully wired against `cognis-core` /
//! `cognis-store`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use cognis_core::config::{CONFIG_DIR_NAME, CONFIG_FILE_NAME, CONFIG_REVISION};
use cognis_core::{handshake_payload, Config};

mod health;
mod index;
mod report;

pub use health::{build_health_report, HealthCheck, HealthReport, HealthStatus};

/// Operator entry point for cognis (Rust engine).
#[derive(Debug, Parser)]
#[command(
    name = "cognis-cli",
    version,
    about = "cognis code intelligence CLI",
    propagate_version = true
)]
pub struct Cli {
    /// Repo root that holds the `.cognis/` directory (default: cwd).
    #[arg(long, global = true, value_name = "DIR")]
    pub repo_root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Materialize the `.cognis/` runtime layout (config, caches, audit log, eval seeds).
    Init(InitArgs),
    /// Run the indexer pipeline (cold or incremental).
    Index(IndexArgs),
    /// One-shot setup: `init` → cold `index` → `health`.
    Bootstrap(BootstrapArgs),
    /// Sanity-check config, DB, and index readiness.
    Health(JsonFlag),
    /// Report the resolved `.cognis/` layout as JSON (extension contract).
    Paths,
    /// Report the setup prerequisite checklist as JSON (extension contract).
    Doctor,
    /// Emit the editor mcp.json payload for this repo as JSON.
    McpConfig(McpConfigArgs),
    /// Run the golden-set eval harness.
    Eval(JsonFlag),
    /// Emit the extension ↔ backend handshake payload (JSON).
    Handshake,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite existing config.yaml and golden.jsonl (other artifacts preserved).
    #[arg(long)]
    pub force: bool,
    /// Suppress human-readable output (used by `bootstrap --json`).
    #[arg(long, hide = true)]
    pub quiet: bool,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Repo path to index (default: repo root / cwd).
    pub path: Option<PathBuf>,
    /// Force a full (cold) rebuild rather than incremental.
    #[arg(long)]
    pub full: bool,
    /// Clear stored index artifacts (DB + caches) and exit.
    #[arg(long)]
    pub clear: bool,
    /// Index without embeddings (faster; no model download).
    #[arg(long)]
    pub skip_embeddings: bool,
}

#[derive(Debug, Args)]
pub struct BootstrapArgs {
    /// Repo path to set up (default: repo root / cwd).
    pub path: Option<PathBuf>,
    /// Overwrite config.yaml when running init.
    #[arg(long)]
    pub force: bool,
    /// Index without embeddings (faster; no model download).
    #[arg(long)]
    pub skip_embeddings: bool,
    /// Emit structured JSON (phases + health) instead of human output.
    #[arg(long = "json")]
    pub as_json: bool,
}

/// Reusable `--json` flag for read-only report commands.
#[derive(Debug, Args)]
pub struct JsonFlag {
    /// Emit machine-readable JSON instead of human-readable output.
    #[arg(long = "json")]
    pub as_json: bool,
}

/// Args for `mcp-config` (mirrors the flags the extension passes).
#[derive(Debug, Args)]
pub struct McpConfigArgs {
    /// Target MCP host (cursor / vscode / claude).
    #[arg(long, default_value = "cursor")]
    pub host: String,
    /// Server-entry name to emit (default: `cognis`).
    #[arg(long = "server-name")]
    pub server_name: Option<String>,
    /// Emit only the minimal env (`COGNIS_DB_PATH`).
    #[arg(long = "minimal-env")]
    pub minimal_env: bool,
    /// Accepted for compatibility; JSON is always emitted.
    #[arg(long = "json", hide = true)]
    pub as_json: bool,
}

// ---------------------------------------------------------------------------
// Entry points (the seam the multi-call `cognis` binary dispatches into)
// ---------------------------------------------------------------------------

/// Parse `std::env::args_os()` and run. Used by the standalone `cognis-cli` bin.
pub fn run() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => execute(cli),
        Err(err) => clap_exit(err),
    }
}

/// Parse an explicit argv (`args[0]` is the program name) and run. Used by the
/// multi-call `cognis` dispatcher.
pub fn run_from<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => execute(cli),
        Err(err) => clap_exit(err),
    }
}

/// Print a clap parse error/`--help`/`--version` to the right stream and map it
/// to an exit code (matching clap's own convention).
fn clap_exit(err: clap::Error) -> ExitCode {
    let _ = err.print();
    if err.use_stderr() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Run a fully-parsed [`Cli`].
pub fn execute(cli: Cli) -> ExitCode {
    let repo_root = cli
        .repo_root
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let result = match cli.command {
        Command::Init(args) => cmd_init(&repo_root, &args),
        Command::Index(args) => index::cmd_index(&repo_root, &args),
        Command::Bootstrap(args) => cmd_bootstrap(&repo_root, &args),
        Command::Health(flag) => cmd_health(&repo_root, flag.as_json),
        Command::Paths => cmd_paths(&repo_root),
        Command::Doctor => cmd_doctor(&repo_root),
        Command::McpConfig(args) => cmd_mcp_config(&repo_root, &args),
        Command::Eval(flag) => cmd_eval(&repo_root, flag.as_json),
        Command::Handshake => cmd_handshake(),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("cognis-cli: {err}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

/// Result of materializing the `.cognis/` layout — the artifacts touched.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct InitReport {
    pub cognis_dir: String,
    /// Human-readable "action path" lines (e.g. `wrote .cognis/config.yaml`).
    pub actions: Vec<String>,
}

/// Materialize `<repo_root>/.cognis/` (config, capsule cache, audit log, eval
/// seed). Pure helper behind `cognis-cli init` — produces the canonical
/// `.cognis/` artifact set the engine and extension expect.
pub fn init(repo_root: &Path, force: bool) -> cognis_core::Result<InitReport> {
    let cognis_dir = repo_root.join(CONFIG_DIR_NAME);
    let mut report = InitReport {
        cognis_dir: cognis_dir.display().to_string(),
        actions: Vec::new(),
    };

    fs_mkdir(&cognis_dir)?;
    report
        .actions
        .push(format!("ensured {}", cognis_dir.display()));

    // 1. config.yaml — Config::default().to_yaml() (task 2.2 contract).
    let cfg_path = cognis_dir.join(CONFIG_FILE_NAME);
    if cfg_path.exists() && !force {
        report.actions.push(format!(
            "exists  {} (preserved; pass --force)",
            cfg_path.display()
        ));
    } else {
        let yaml = Config::default().to_yaml()?;
        fs_write(&cfg_path, &yaml)?;
        report
            .actions
            .push(format!("wrote   {}", cfg_path.display()));
    }
    // config revision marker (mirrors write_config_revision).
    let revision_path = cognis_dir.join("config.revision");
    fs_write(&revision_path, &format!("{CONFIG_REVISION}\n"))?;

    // 2. capsule_cache/.
    let capsule_cache = cognis_dir.join("capsule_cache");
    fs_mkdir(&capsule_cache)?;
    report
        .actions
        .push(format!("ensured {}", capsule_cache.display()));

    // 3. audit log — touch at the path declared in security.audit_log.
    let cfg = Config::load(repo_root)?;
    let audit_path = resolve_under_repo(repo_root, &cfg.security.audit_log);
    if let Some(parent) = audit_path.parent() {
        fs_mkdir(parent)?;
    }
    if !audit_path.exists() {
        fs_write(&audit_path, "")?;
        report
            .actions
            .push(format!("touched {}", audit_path.display()));
    } else {
        report
            .actions
            .push(format!("exists  {}", audit_path.display()));
    }

    // 4. eval/golden.jsonl placeholder.
    let eval_path = resolve_under_repo(repo_root, &cfg.eval.golden_set);
    if let Some(parent) = eval_path.parent() {
        fs_mkdir(parent)?;
    }
    if eval_path.exists() && !force {
        report
            .actions
            .push(format!("exists  {} (preserved)", eval_path.display()));
    } else {
        fs_write(&eval_path, "")?;
        report
            .actions
            .push(format!("wrote   {}", eval_path.display()));
    }

    Ok(report)
}

fn cmd_init(repo_root: &Path, args: &InitArgs) -> cognis_core::Result<ExitCode> {
    let report = init(repo_root, args.force)?;
    if !args.quiet {
        for line in &report.actions {
            println!("  {line}");
        }
        println!("\ncognis initialized at {}", report.cognis_dir);
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

fn cmd_health(repo_root: &Path, as_json: bool) -> cognis_core::Result<ExitCode> {
    let report = build_health_report(repo_root);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| cognis_core::CognisError::Config(e.to_string()))?
        );
    } else {
        println!("cognis health — overall: {}", report.overall.as_str());
        for (name, check) in &report.checks {
            println!("  [{}] {}: {}", check.status.as_str(), name, check.message);
        }
    }
    // fail → non-zero so scripts/extension auto-manage can react.
    Ok(match report.overall {
        HealthStatus::Fail => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    })
}

// ---------------------------------------------------------------------------
// paths / doctor / mcp-config — extension JSON contract surfaces
// ---------------------------------------------------------------------------

/// Serialize `value` to compact JSON on stdout (the shape the extension reads).
fn print_json<T: Serialize>(value: &T) -> cognis_core::Result<ExitCode> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|e| cognis_core::CognisError::Config(e.to_string()))?
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_paths(repo_root: &Path) -> cognis_core::Result<ExitCode> {
    print_json(&report::build_paths(repo_root))
}

fn cmd_doctor(repo_root: &Path) -> cognis_core::Result<ExitCode> {
    print_json(&report::build_doctor(repo_root))
}

fn cmd_mcp_config(repo_root: &Path, args: &McpConfigArgs) -> cognis_core::Result<ExitCode> {
    let opts = report::McpConfigOptions {
        host: args.host.clone(),
        server_name: args.server_name.clone(),
        minimal_env: args.minimal_env,
    };
    print_json(&report::build_mcp_config(repo_root, &opts))
}

// ---------------------------------------------------------------------------
// bootstrap
// ---------------------------------------------------------------------------

fn cmd_bootstrap(repo_root: &Path, args: &BootstrapArgs) -> cognis_core::Result<ExitCode> {
    let target = args.path.clone().unwrap_or_else(|| repo_root.to_path_buf());

    let init_report = init(&target, args.force)?;

    let index_args = IndexArgs {
        path: Some(target.clone()),
        full: true,
        clear: false,
        skip_embeddings: args.skip_embeddings,
    };
    let index_outcome = index::index_outcome(&target, &index_args);

    let health = build_health_report(&target);

    if args.as_json {
        // The extension's `setupWorkspace` parses this as `BootstrapPayload`:
        // command / runtime_version / repo_root / index_path / db_path /
        // skip_embeddings / paths / phases / health / overall / exit_code.
        let paths = report::build_paths(&target);
        let payload = report::BootstrapPayload {
            command: "bootstrap".to_string(),
            runtime_version: env!("CARGO_PKG_VERSION").to_string(),
            repo_root: target.to_string_lossy().into_owned(),
            index_path: target.to_string_lossy().into_owned(),
            db_path: paths.db_path.clone(),
            skip_embeddings: args.skip_embeddings,
            paths,
            phases: vec![
                report::BootstrapPhase {
                    name: "init".to_string(),
                    status: "ok".to_string(),
                },
                report::BootstrapPhase {
                    name: "index".to_string(),
                    status: index_outcome.status.clone(),
                },
                report::BootstrapPhase {
                    name: "health".to_string(),
                    status: health.overall.as_str().to_string(),
                },
            ],
            overall: health.overall.as_str().to_string(),
            exit_code: 0,
            health,
        };
        print_json(&payload)?;
    } else {
        println!("bootstrap: init");
        for line in &init_report.actions {
            println!("  {line}");
        }
        println!("bootstrap: index\n  {}", index_outcome.message);
        println!("bootstrap: health — overall {}", health.overall.as_str());
        for (name, check) in &health.checks {
            println!("  [{}] {}: {}", check.status.as_str(), name, check.message);
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// eval
// ---------------------------------------------------------------------------

fn cmd_eval(repo_root: &Path, as_json: bool) -> cognis_core::Result<ExitCode> {
    let cfg = Config::load(repo_root)?;
    let golden = resolve_under_repo(repo_root, &cfg.eval.golden_set);
    let message = format!(
        "the native eval harness lands in Task 9 (cognis-eval). golden set: {}",
        golden.display()
    );
    if as_json {
        println!(
            "{}",
            serde_json::json!({ "status": "pending", "component": "cognis-eval", "message": message })
        );
    } else {
        println!("cognis-cli eval: {message}");
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// handshake
// ---------------------------------------------------------------------------

fn cmd_handshake() -> cognis_core::Result<ExitCode> {
    let payload = handshake_payload();
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|e| cognis_core::CognisError::Config(e.to_string()))?
    );
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// small fs / path helpers (typed CognisError, no panics)
// ---------------------------------------------------------------------------

pub(crate) fn resolve_under_repo(repo_root: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        repo_root.join(candidate)
    }
}

fn fs_mkdir(path: &Path) -> cognis_core::Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|e| cognis_core::CognisError::Config(format!("mkdir {}: {e}", path.display())))
}

fn fs_write(path: &Path, contents: &str) -> cognis_core::Result<()> {
    std::fs::write(path, contents)
        .map_err(|e| cognis_core::CognisError::Config(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cognis-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_materializes_cognis_layout() {
        let repo = tmp_repo();
        let report = init(&repo, false).unwrap();
        assert!(repo.join(".cognis").is_dir());
        assert!(repo.join(".cognis/config.yaml").is_file());
        assert!(repo.join(".cognis/capsule_cache").is_dir());
        assert!(repo.join(".cognis/audit.log").is_file());
        assert!(repo.join(".cognis/eval/golden.jsonl").is_file());
        assert!(repo.join(".cognis/config.revision").is_file());
        assert!(!report.actions.is_empty());

        // config.yaml round-trips to the default Config.
        let text = std::fs::read_to_string(repo.join(".cognis/config.yaml")).unwrap();
        assert_eq!(Config::from_yaml_str(&text).unwrap(), Config::default());

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn init_preserves_existing_config_without_force() {
        let repo = tmp_repo();
        init(&repo, false).unwrap();
        // Tamper with config; a second init without --force must preserve it.
        let cfg_path = repo.join(".cognis/config.yaml");
        std::fs::write(&cfg_path, "embedder:\n  dim: 768\n").unwrap();
        init(&repo, false).unwrap();
        let text = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(text.contains("768"), "config should be preserved: {text}");
        // With --force it is overwritten back to defaults.
        init(&repo, true).unwrap();
        let text = std::fs::read_to_string(&cfg_path).unwrap();
        assert_eq!(Config::from_yaml_str(&text).unwrap(), Config::default());
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn health_on_uninitialized_repo_is_not_ok() {
        std::env::remove_var("COGNIS_DB_PATH");
        let repo = tmp_repo();
        let report = build_health_report(&repo);
        // No DB yet → index check fails (not ready to serve).
        assert_eq!(report.overall, HealthStatus::Fail);
        assert!(report.checks.iter().any(|(n, _)| n == "index"));
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn parse_subcommands() {
        for argv in [
            vec!["cognis-cli", "init"],
            vec!["cognis-cli", "init", "--force"],
            vec!["cognis-cli", "index", ".", "--full"],
            vec!["cognis-cli", "index", "--clear"],
            vec!["cognis-cli", "bootstrap", ".", "--json"],
            vec!["cognis-cli", "health", "--json"],
            vec!["cognis-cli", "eval"],
            vec!["cognis-cli", "handshake"],
            vec!["cognis-cli", "--repo-root", ".", "health"],
        ] {
            assert!(
                Cli::try_parse_from(argv.clone()).is_ok(),
                "failed to parse {argv:?}"
            );
        }
    }
}
