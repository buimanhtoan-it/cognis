//! JSON report shapes the VS Code extension consumes (`paths`, `doctor`,
//! `mcp-config`, and the `bootstrap` envelope).
//!
//! These are **cross-language contract surfaces**: their field names + value
//! types are pinned by `tests/e2e/contracts/*.json` and the extension's
//! `types.ts`. Keep them in lockstep — a dropped/renamed field silently breaks
//! the extension's setup flow. Each builder is pure over a repo root + config
//! so it is unit-testable without spawning a process.

use std::path::Path;

use serde::Serialize;

use cognis_core::config::{CONFIG_DIR_NAME, CONFIG_FILE_NAME};
use cognis_core::Config;

use crate::health::HealthReport;
use crate::resolve_under_repo;

/// The engine version reported across every payload (`runtime_version`).
fn runtime_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The active UCKG path: `COGNIS_DB_PATH` override, else `<repo>/.cognis/uckg.db`.
fn db_path_for(repo_root: &Path) -> String {
    if let Ok(p) = std::env::var("COGNIS_DB_PATH") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    repo_root
        .join(CONFIG_DIR_NAME)
        .join("uckg.db")
        .to_string_lossy()
        .into_owned()
}

/// The running binary's path (for `engine_binary`), or `"cognis"` if unknown.
fn self_exe() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cognis".to_string())
}

/// `cognis-cli paths` — the resolved `.cognis/` layout the extension reads.
/// The pure-Rust engine ships as one self-contained multi-call binary, so every
/// surface (`cli`/`mcpd`/`indexd`) is dispatched from `engine_binary` — the
/// path of the running executable.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspacePaths {
    pub repo_root: String,
    pub cognis_dir: String,
    pub config_path: String,
    pub db_path: String,
    pub indexd_status_path: String,
    pub audit_log_path: String,
    pub capsule_cache_dir: String,
    pub golden_set_path: String,
    pub runtime_version: String,
    pub engine_binary: String,
}

/// Build the [`WorkspacePaths`] for `repo_root` (config-aware for the audit-log
/// and golden-set locations; defaults when the config is absent).
pub fn build_paths(repo_root: &Path) -> WorkspacePaths {
    let cfg = Config::load(repo_root).unwrap_or_default();
    let cognis_dir = repo_root.join(CONFIG_DIR_NAME);
    WorkspacePaths {
        repo_root: repo_root.to_string_lossy().into_owned(),
        cognis_dir: cognis_dir.to_string_lossy().into_owned(),
        config_path: cognis_dir
            .join(CONFIG_FILE_NAME)
            .to_string_lossy()
            .into_owned(),
        db_path: db_path_for(repo_root),
        indexd_status_path: cognis_dir
            .join("indexd-status.json")
            .to_string_lossy()
            .into_owned(),
        audit_log_path: resolve_under_repo(repo_root, &cfg.security.audit_log)
            .to_string_lossy()
            .into_owned(),
        capsule_cache_dir: cognis_dir
            .join("capsule_cache")
            .to_string_lossy()
            .into_owned(),
        golden_set_path: resolve_under_repo(repo_root, &cfg.eval.golden_set)
            .to_string_lossy()
            .into_owned(),
        runtime_version: runtime_version(),
        engine_binary: self_exe(),
    }
}

/// One row of the `doctor` prerequisite checklist.
#[derive(Debug, Clone, Serialize)]
pub struct PrerequisiteItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub status: String,
    pub required: bool,
    pub install_target: String,
    pub detail: String,
}

/// `cognis-cli doctor` — the setup prerequisite checklist.
#[derive(Debug, Clone, Serialize)]
pub struct PrerequisiteReport {
    pub ready: bool,
    pub items: Vec<PrerequisiteItem>,
    /// Generic install target for all missing items, or "" when none. Empty for
    /// the single self-contained binary (satisfied by installing the engine).
    pub combined_install_target: String,
}

/// Build the `doctor` report. The pure-Rust engine ships as one self-contained
/// binary: if this command is running, the engine is present, so the required
/// "engine" item is satisfied. A second, optional item reflects whether the
/// semantic vector index is populated (informational — indexing satisfies it,
/// not a package install), so the panel can surface "semantic degraded".
pub fn build_doctor(repo_root: &Path) -> PrerequisiteReport {
    let engine = PrerequisiteItem {
        id: "engine".to_string(),
        label: "Cognis engine".to_string(),
        description: "The single self-contained cognis binary (SQLite bundled, ONNX assets local)."
            .to_string(),
        status: "ok".to_string(),
        required: true,
        install_target: String::new(),
        detail: format!("cognis {} (rust)", runtime_version()),
    };

    // Optional semantic-index item: ok when vectors are present, else "missing"
    // (satisfied by indexing the repo, not by installing anything).
    let vectors_present = {
        let db = db_path_for(repo_root);
        std::path::Path::new(&db).exists()
            && cognis_store::Database::open(&db)
                .and_then(|d| d.vec_row_count())
                .map(|n| n > 0)
                .unwrap_or(false)
    };
    let semantic = PrerequisiteItem {
        id: "semantic_index".to_string(),
        label: "Semantic index".to_string(),
        description: "Symbol embeddings for semantic search (built by indexing the repo)."
            .to_string(),
        status: if vectors_present { "ok" } else { "missing" }.to_string(),
        required: false,
        install_target: String::new(),
        detail: if vectors_present {
            "vectors present".to_string()
        } else {
            "no vectors yet — index the repo to enable semantic search".to_string()
        },
    };

    PrerequisiteReport {
        ready: true, // the only required item (engine) is satisfied by running.
        items: vec![engine, semantic],
        combined_install_target: String::new(),
    }
}

/// One MCP server block (`command` + `args` + `env`) inside the config payload.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerBlock {
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
}

/// The `{ mcpServers: { <name>: block } }` config document.
#[derive(Debug, Clone, Serialize)]
pub struct McpConfigDocument {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::BTreeMap<String, McpServerBlock>,
}

/// `cognis-cli mcp-config` — the editor mcp.json payload for a repo.
#[derive(Debug, Clone, Serialize)]
pub struct McpConfigPayload {
    pub host: String,
    pub format: String,
    pub repo_root: String,
    pub server_name: String,
    pub config: McpConfigDocument,
    pub config_paths: std::collections::BTreeMap<String, String>,
    pub env: std::collections::BTreeMap<String, String>,
}

/// Options for [`build_mcp_config`], mirroring the CLI flags the extension
/// passes (`--host`, `--server-name`, `--minimal-env`).
pub struct McpConfigOptions {
    pub host: String,
    pub server_name: Option<String>,
    pub minimal_env: bool,
}

/// Build the [`McpConfigPayload`]. The server block launches the running binary
/// on its `mcpd` surface (`<exe> mcpd`) with `COGNIS_DB_PATH` pinned to the
/// repo's UCKG — the extension may still rewrite `command`/`args` to the managed
/// binary, but the env (which `envMatchesRepo` depends on) is authoritative
/// here. `--minimal-env` keeps the env to just `COGNIS_DB_PATH`.
pub fn build_mcp_config(repo_root: &Path, opts: &McpConfigOptions) -> McpConfigPayload {
    let server_name = opts
        .server_name
        .clone()
        .unwrap_or_else(|| "cognis".to_string());
    let db = db_path_for(repo_root);

    let mut env = std::collections::BTreeMap::new();
    env.insert("COGNIS_DB_PATH".to_string(), db.clone());
    if !opts.minimal_env {
        // Room for future non-minimal env (timeouts etc.); minimal is the
        // extension's default request, so keep the superset conservative.
        env.insert(
            "COGNIS_INDEXD_STATUS_PATH".to_string(),
            repo_root
                .join(CONFIG_DIR_NAME)
                .join("indexd-status.json")
                .to_string_lossy()
                .into_owned(),
        );
    }

    // Launch command: the running binary on its mcpd surface. The extension may
    // still rewrite command/args to the managed binary path, but the env (which
    // envMatchesRepo depends on) is authoritative here.
    let block = McpServerBlock {
        command: self_exe(),
        args: vec!["mcpd".to_string()],
        env: env.clone(),
    };

    let mut servers = std::collections::BTreeMap::new();
    servers.insert(server_name.clone(), block);

    let mut config_paths = std::collections::BTreeMap::new();
    config_paths.insert(
        opts.host.clone(),
        repo_root
            .join(mcp_config_rel_for_host(&opts.host))
            .to_string_lossy()
            .into_owned(),
    );

    McpConfigPayload {
        host: opts.host.clone(),
        format: "mcpServers".to_string(),
        repo_root: repo_root.to_string_lossy().into_owned(),
        server_name,
        config: McpConfigDocument {
            mcp_servers: servers,
        },
        config_paths,
        env,
    }
}

/// Best-effort workspace mcp.json relative path per host (informational — the
/// extension computes the authoritative path itself).
fn mcp_config_rel_for_host(host: &str) -> String {
    match host {
        "cursor" => ".cursor/mcp.json".to_string(),
        "vscode" => ".vscode/mcp.json".to_string(),
        _ => ".mcp.json".to_string(),
    }
}

/// One phase of a `bootstrap` run (`init` / `index` / `health`).
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapPhase {
    pub name: String,
    pub status: String,
}

/// `cognis-cli bootstrap --json` — the full setup envelope the extension's
/// `setupWorkspace` parses.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapPayload {
    pub command: String,
    pub runtime_version: String,
    pub repo_root: String,
    pub index_path: String,
    pub db_path: String,
    pub skip_embeddings: bool,
    pub paths: WorkspacePaths,
    pub phases: Vec<BootstrapPhase>,
    pub health: HealthReport,
    pub overall: String,
    pub exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn tmp_repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cognis-cli-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `paths` JSON must carry every field the extension's WorkspacePaths reads
    /// (tests/e2e/contracts/paths.json).
    #[test]
    fn paths_payload_has_contract_keys() {
        let repo = tmp_repo();
        let v = serde_json::to_value(build_paths(&repo)).unwrap();
        for key in [
            "repo_root",
            "cognis_dir",
            "config_path",
            "db_path",
            "indexd_status_path",
            "audit_log_path",
            "capsule_cache_dir",
            "golden_set_path",
            "runtime_version",
            "engine_binary",
        ] {
            assert!(v.get(key).is_some(), "paths missing key {key}");
        }
        assert!(
            v["engine_binary"].is_string(),
            "engine_binary must be the running executable path"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    /// `doctor` reports the engine as ready (single-binary is present if running).
    #[test]
    fn doctor_payload_is_ready_with_items() {
        let repo = tmp_repo();
        let report = build_doctor(&repo);
        assert!(report.ready);
        assert!(!report.items.is_empty());
        let v = serde_json::to_value(&report).unwrap();
        for key in ["ready", "items", "combined_install_target"] {
            assert!(v.get(key).is_some(), "doctor missing key {key}");
        }
        assert!(v["items"][0].get("install_target").is_some());
        std::fs::remove_dir_all(&repo).ok();
    }

    /// `mcp-config` echoes the server name and carries COGNIS_DB_PATH in the
    /// server block env (envMatchesRepo depends on it).
    #[test]
    fn mcp_config_payload_has_db_env_and_server() {
        let repo = tmp_repo();
        let opts = McpConfigOptions {
            host: "cursor".to_string(),
            server_name: Some("cognis-demo".to_string()),
            minimal_env: true,
        };
        let payload = build_mcp_config(&repo, &opts);
        let v: Value = serde_json::to_value(&payload).unwrap();
        for key in [
            "host",
            "format",
            "repo_root",
            "server_name",
            "config",
            "config_paths",
            "env",
        ] {
            assert!(v.get(key).is_some(), "mcp-config missing key {key}");
        }
        assert_eq!(v["server_name"], "cognis-demo");
        let servers = &v["config"]["mcpServers"];
        let block = &servers["cognis-demo"];
        assert!(block.get("command").is_some());
        assert!(block["env"].get("COGNIS_DB_PATH").is_some());
        std::fs::remove_dir_all(&repo).ok();
    }
}
