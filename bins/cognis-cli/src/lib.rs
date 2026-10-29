//! cognis-cli — `clap` operator CLI.
//!
//! Subcommands (the operator surface the IDE extension and CLI users drive):
//! `init`, `index`, `bootstrap`, `health`, `eval`, plus the contract-critical
//! `handshake` payload read at startup. The command tree is `clap`-derive and
//! delegates to pure helpers ([`init`], [`build_health_report`], …) so every
//! operation is unit-testable without spawning a process.
//!
//! The entry points ([`run`] / [`run_from`]) return a [`std::process::ExitCode`]
//! and serve as the dispatch target for the single multi-call `cognis` binary.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use cognis_core::config::{CONFIG_DIR_NAME, CONFIG_FILE_NAME, CONFIG_REVISION};
use cognis_core::{handshake_payload_with_capabilities, Config, HandshakePayload};

mod health;
mod index;
mod report;

pub use health::{
    build_health_report, build_health_report_with_capabilities, resolve_running_capabilities,
    running_capabilities, HealthCheck, HealthReport, HealthStatus,
};

/// Build the startup handshake from the same running capability source as health.
pub fn build_handshake_payload() -> HandshakePayload {
    handshake_payload_with_capabilities(running_capabilities())
}

/// Operator entry point for cognis (Rust engine).
#[derive(Debug, Parser)]
#[command(
    name = "cognis-cli",
    version,
    about = "Cognis guardrail & code intelligence CLI",
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
    /// Emit deterministic database category digests for cross-process comparison.
    Digest(DigestArgs),
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

/// Args for `digest` (plain digest, or a diagnosable comparison — Requirement 24.2).
#[derive(Debug, Args)]
pub struct DigestArgs {
    /// Compare this repo's Workspace_Database against another database file or repo root.
    #[arg(long)]
    pub diff: Option<PathBuf>,
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
        Command::Digest(args) => cmd_digest(&repo_root, &args),
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

    // 1. config.yaml — Config::default().to_yaml().
    let cfg_path = cognis_dir.join(CONFIG_FILE_NAME);
    if cfg_path.exists() && !force {
        let upgraded = upgrade_legacy_allow_tools_if_needed(&cfg_path)?;
        if upgraded {
            report.actions.push(format!(
                "updated {} (migrated default allow_tools to current contract)",
                cfg_path.display()
            ));
        } else {
            report.actions.push(format!(
                "exists  {} (preserved; pass --force)",
                cfg_path.display()
            ));
        }
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
// handshake / digest
// ---------------------------------------------------------------------------

fn cmd_handshake() -> cognis_core::Result<ExitCode> {
    let payload = build_handshake_payload();
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|e| cognis_core::CognisError::Config(e.to_string()))?
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_digest(repo_root: &Path, args: &DigestArgs) -> cognis_core::Result<ExitCode> {
    let path = report::db_path_for(repo_root);
    if !Path::new(&path).exists() {
        return Err(cognis_core::CognisError::Store(format!(
            "database not found: {path}"
        )));
    }
    let database = cognis_store::Database::open(&path)?;
    if let Some(other) = &args.diff {
        let other_path = resolve_diff_db(other);
        if !other_path.exists() {
            database.close_thread_connection();
            return Err(cognis_core::CognisError::Store(format!(
                "diff database not found: {}",
                other_path.display()
            )));
        }
        let other_db = cognis_store::Database::open(other_path.to_string_lossy().as_ref())?;
        let left = cognis_store::database_digest(&database)?;
        let right = cognis_store::database_digest(&other_db)?;
        let rows = cognis_store::diff(&database, &other_db)?;
        let differing_categories: Vec<&str> = left.differing_categories(&right);
        let payload = serde_json::json!({
            "equal": differing_categories.is_empty(),
            "differing_categories": differing_categories,
            "rows": rows.iter().map(|row| serde_json::json!({
                "category": row.category,
                "only_left": row.only_left,
                "only_right": row.only_right,
            })).collect::<Vec<_>>(),
            "left": left,
            "right": right,
        });
        other_db.close_thread_connection();
        database.close_thread_connection();
        print_json(&payload)
    } else {
        let digest = cognis_store::database_digest(&database)?;
        database.close_thread_connection();
        print_json(&digest)
    }
}

fn resolve_diff_db(other: &Path) -> PathBuf {
    if other.is_file() {
        other.to_path_buf()
    } else {
        other.join(CONFIG_DIR_NAME).join("uckg.db")
    }
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

/// Additively upgrade an older auto-generated 8-tool `mcp.allow_tools` list so
/// existing workspaces automatically expose the 4 guardrail tools without
/// clobbering custom user settings or intentionally restricted security lists.
fn upgrade_legacy_allow_tools_if_needed(cfg_path: &Path) -> cognis_core::Result<bool> {
    const LEGACY_DEFAULT_EIGHT: &[&str] = &[
        "diffuse_context",
        "symbol_lookup",
        "symbol_search",
        "discover_symbols",
        "semantic_search",
        "resolve_symbols",
        "dependency_trace",
        "retrieve_context_capsule",
    ];
    let raw = match std::fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    if !raw.contains("allow_tools") {
        return Ok(false);
    }
    let mut cfg = match Config::from_yaml_str(&raw) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    let current: Vec<&str> = cfg.mcp.allow_tools.iter().map(String::as_str).collect();
    if current == LEGACY_DEFAULT_EIGHT {
        cfg.mcp.allow_tools = Config::default().mcp.allow_tools;
        let yaml = cfg.to_yaml()?;
        fs_write(cfg_path, &yaml)?;
        return Ok(true);
    }
    Ok(false)
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
    fn init_upgrades_legacy_eight_tool_allow_list_without_clobbering_custom_settings() {
        let repo = tmp_repo();
        init(&repo, false).unwrap();
        let cfg_path = repo.join(".cognis/config.yaml");
        let mut legacy = Config::default();
        legacy.embedder.dim = 768;
        legacy.mcp.allow_tools = vec![
            "diffuse_context".into(),
            "symbol_lookup".into(),
            "symbol_search".into(),
            "discover_symbols".into(),
            "semantic_search".into(),
            "resolve_symbols".into(),
            "dependency_trace".into(),
            "retrieve_context_capsule".into(),
        ];
        std::fs::write(&cfg_path, legacy.to_yaml().unwrap()).unwrap();

        init(&repo, false).unwrap();
        let upgraded = Config::from_yaml_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
        assert_eq!(upgraded.embedder.dim, 768);
        assert_eq!(upgraded.mcp.allow_tools.len(), 12);
        assert!(upgraded
            .mcp
            .allow_tools
            .iter()
            .any(|t| t == "analyze_patch_impact"));

        // Deliberately restricted allow_tools (e.g. single read-only tool) is NOT expanded.
        let mut restricted = Config::default();
        restricted.mcp.allow_tools = vec!["symbol_lookup".into()];
        std::fs::write(&cfg_path, restricted.to_yaml().unwrap()).unwrap();
        init(&repo, false).unwrap();
        let after = Config::from_yaml_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
        assert_eq!(after.mcp.allow_tools, vec!["symbol_lookup".to_string()]);

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
        let expected = resolve_running_capabilities();
        assert_eq!(
            report.capabilities.semantic_backend,
            expected.semantic_backend
        );
        assert_eq!(
            report.capabilities.accelerated_vector_search,
            expected.accelerated_vector_search
        );
        let vector = report
            .checks
            .iter()
            .find(|(name, _)| name == "vector")
            .map(|(_, check)| check.message.as_str())
            .unwrap();
        assert!(vector.contains("blob_linear_scan fallback"));
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn developer_build_reports_compiled_capabilities_without_onnx() {
        let caps = resolve_running_capabilities();
        assert_eq!(
            caps.semantic_backend,
            cfg!(any(feature = "onnx", feature = "onnx-download")),
            "semantic_backend must track compiled ONNX features only"
        );
        assert_eq!(
            caps.accelerated_vector_search,
            cfg!(feature = "sqlite-vec") && cognis_store::accelerated_vector_search_available(),
            "accelerated_vector_search must track compiled sqlite-vec + loadability"
        );
        #[cfg(not(any(feature = "onnx", feature = "onnx-download", feature = "sqlite-vec")))]
        {
            assert!(
                !caps.semantic_backend && !caps.accelerated_vector_search,
                "default developer build reports both capabilities absent"
            );
        }
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
            vec!["cognis-cli", "digest"],
            vec!["cognis-cli", "--repo-root", ".", "health"],
        ] {
            assert!(
                Cli::try_parse_from(argv.clone()).is_ok(),
                "failed to parse {argv:?}"
            );
        }
    }
}
