//! `cognis-cli health` — sanity checks over config / DB / index readiness.
//!
//! Rust mirror of the Python `_build_health_report` subcheck set, scoped to the
//! surfaces the Rust engine already owns (`cognis-core` config, `cognis-store`
//! UCKG). Each subcheck returns `ok` / `warn` / `fail`; the overall status is
//! `fail > warn > ok`. `warn` is used where a fresh repo merely needs
//! `cognis-cli init`/`index` (actionable, not a hard error) so the extension's
//! auto-manage can distinguish "needs setup" from "broken".

use std::path::Path;

use serde::Serialize;

use cognis_core::config::{CONFIG_DIR_NAME, CONFIG_FILE_NAME};
use cognis_core::Config;

/// One subcheck verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Warn,
    Fail,
}

impl HealthStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HealthStatus::Ok => "ok",
            HealthStatus::Warn => "warn",
            HealthStatus::Fail => "fail",
        }
    }
}

/// A single subcheck payload.
#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub status: HealthStatus,
    pub message: String,
}

impl HealthCheck {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Ok,
            message: message.into(),
        }
    }
    fn warn(message: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Warn,
            message: message.into(),
        }
    }
    fn fail(message: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Fail,
            message: message.into(),
        }
    }
}

/// Top-level health report. `checks` is an ordered list of `(name, check)` so
/// the JSON output is stable for snapshot/extension consumers; it serializes as
/// a JSON **object** keyed by check name (the shape `types.ts`'
/// `HealthReport.checks: Record<string, …>` and `tests/e2e/contracts/health.json`
/// pin), not as an array of pairs.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub runtime_version: String,
    #[serde(serialize_with = "serialize_checks_as_map")]
    pub checks: Vec<(String, HealthCheck)>,
    pub overall: HealthStatus,
}

/// Serialize the ordered `(name, check)` list as a JSON object keyed by name.
fn serialize_checks_as_map<S>(
    checks: &[(String, HealthCheck)],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(checks.len()))?;
    for (name, check) in checks {
        map.serialize_entry(name, check)?;
    }
    map.end()
}

/// Run all sanity checks and assemble a report (pure; safe on any path).
pub fn build_health_report(repo_root: &Path) -> HealthReport {
    let checks = vec![
        ("config".to_string(), check_config(repo_root)),
        ("db".to_string(), check_db(repo_root)),
        ("index".to_string(), check_index(repo_root)),
        ("vector".to_string(), check_vector(repo_root)),
    ];
    let overall = aggregate(&checks);
    HealthReport {
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        checks,
        overall,
    }
}

fn aggregate(checks: &[(String, HealthCheck)]) -> HealthStatus {
    let mut overall = HealthStatus::Ok;
    for (_, check) in checks {
        match check.status {
            HealthStatus::Fail => return HealthStatus::Fail,
            HealthStatus::Warn => overall = HealthStatus::Warn,
            HealthStatus::Ok => {}
        }
    }
    overall
}

fn config_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME)
}

fn db_path(repo_root: &Path) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COGNIS_DB_PATH") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    repo_root.join(CONFIG_DIR_NAME).join("uckg.db")
}

fn check_config(repo_root: &Path) -> HealthCheck {
    let cfg_path = config_path(repo_root);
    if !cfg_path.exists() {
        return HealthCheck::warn(format!(
            "{} not present; using built-in defaults (run `cognis-cli init`)",
            cfg_path.display()
        ));
    }
    match Config::load(repo_root) {
        Ok(cfg) => HealthCheck::ok(format!(
            "{} loaded (embedder={}/{}, languages=[{}])",
            cfg_path.display(),
            cfg.embedder.backend,
            cfg.embedder.model,
            cfg.languages.enabled.join(",")
        )),
        Err(err) => HealthCheck::fail(format!("failed to load {}: {err}", cfg_path.display())),
    }
}

fn check_db(repo_root: &Path) -> HealthCheck {
    let db = db_path(repo_root);
    if db.exists() {
        // Best-effort writability probe via the read-only metadata.
        match std::fs::metadata(&db) {
            Ok(meta) if meta.permissions().readonly() => {
                HealthCheck::fail(format!("{} present but read-only", db.display()))
            }
            Ok(_) => HealthCheck::ok(format!("{} present and writable", db.display())),
            Err(err) => HealthCheck::fail(format!("cannot stat {}: {err}", db.display())),
        }
    } else if let Some(parent) = db.parent() {
        if parent.exists() {
            HealthCheck::warn(format!(
                "{} not present; parent exists (run `cognis-cli init`)",
                db.display()
            ))
        } else {
            HealthCheck::warn(format!(
                "{} not present; run `cognis-cli init` to create it",
                db.display()
            ))
        }
    } else {
        HealthCheck::warn(format!("{} not present", db.display()))
    }
}

fn check_index(repo_root: &Path) -> HealthCheck {
    let db = db_path(repo_root);
    if !db.exists() {
        return HealthCheck::fail(format!(
            "{} not present — run `cognis-cli init` then index the repo",
            db.display()
        ));
    }
    // The DB exists: open it (migrations are idempotent on a Python-built DB)
    // and count symbols. An empty index is not ready to serve MCP traffic.
    let path = db.to_string_lossy().into_owned();
    match cognis_store::Database::open(&path) {
        Ok(database) => match database.count("symbol") {
            Ok(0) => HealthCheck::fail(format!(
                "{} has 0 symbols — index the repo before serving",
                db.display()
            )),
            Ok(n) => HealthCheck::ok(format!("{n} symbols indexed in UCKG")),
            Err(err) => HealthCheck::fail(format!("cannot read symbol count: {err}")),
        },
        Err(err) => HealthCheck::fail(format!("cannot open {}: {err}", db.display())),
    }
}

fn check_vector(repo_root: &Path) -> HealthCheck {
    let db = db_path(repo_root);
    if !db.exists() {
        return HealthCheck::warn("vector check skipped (no database)".to_string());
    }
    let path = db.to_string_lossy().into_owned();
    match cognis_store::Database::open(&path).and_then(|d| d.vec_symbol_ids()) {
        Ok(ids) if ids.is_empty() => {
            HealthCheck::warn("no vectors stored yet (semantic search degraded)".to_string())
        }
        Ok(ids) => HealthCheck::ok(format!("{} symbol vectors present", ids.len())),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("vec0") {
                // A legacy sqlite-vec `vec0` index this build can't read. It
                // self-heals to the built-in vector format on the next index
                // pass, so point the user at that instead of a cryptic error.
                HealthCheck::warn(
                    "legacy vector index from another build; run Rebuild Index \
                     (after Install Backend) to migrate it and enable semantic search"
                        .to_string(),
                )
            } else {
                HealthCheck::warn(format!("vector check failed: {err}"))
            }
        }
    }
}
