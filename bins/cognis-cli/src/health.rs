//! `cognis-cli health` — sanity checks over config / DB / index readiness.
//!
//! Rust mirror of the Python `_build_health_report` subcheck set, scoped to the
//! surfaces the Rust engine already owns (`cognis-core` config, `cognis-store`
//! UCKG). Each subcheck returns `ok` / `warn` / `fail`; the overall status is
//! `fail > warn > ok`. `warn` is used where a fresh repo merely needs
//! `cognis-cli init`/`index` (actionable, not a hard error) so the extension's
//! auto-manage can distinguish "needs setup" from "broken".

use std::path::Path;
use std::sync::OnceLock;

use serde::Serialize;

use cognis_core::config::{CONFIG_DIR_NAME, CONFIG_FILE_NAME};
use cognis_core::{Capabilities, Config};
use cognis_store::VectorSearchBackend;

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
    pub capabilities: Capabilities,
    #[serde(serialize_with = "serialize_checks_as_map")]
    pub checks: Vec<(String, HealthCheck)>,
    pub overall: HealthStatus,
    /// Required SQLite indexes absent from the workspace database.
    ///
    /// Empty when every bounded-read index is present (or there is no database
    /// yet). The status surface maps a non-empty list to `Needs_Attention`
    /// naming the missing index (Requirement 9.7).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_indexes: Vec<String>,
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

static RUNNING_CAPABILITIES: OnceLock<Capabilities> = OnceLock::new();

/// Resolve the capabilities of this running binary from compiled features and
/// the runtime sqlite-vec loadability probe.
pub fn resolve_running_capabilities() -> Capabilities {
    Capabilities {
        semantic_backend: cfg!(any(feature = "onnx", feature = "onnx-download")),
        accelerated_vector_search: cognis_store::accelerated_vector_search_available(),
    }
}

/// Cached startup capabilities for this running process.
pub fn running_capabilities() -> Capabilities {
    *RUNNING_CAPABILITIES.get_or_init(resolve_running_capabilities)
}

/// Run all sanity checks using this running binary's resolved capabilities.
pub fn build_health_report(repo_root: &Path) -> HealthReport {
    build_health_report_with_capabilities(repo_root, running_capabilities())
}

/// Assemble a health report from a capability set resolved at binary startup.
pub fn build_health_report_with_capabilities(
    repo_root: &Path,
    capabilities: Capabilities,
) -> HealthReport {
    let vector = check_vector(repo_root);
    let (index, missing_indexes) = check_index(repo_root);
    let checks = vec![
        ("config".to_string(), check_config(repo_root)),
        ("db".to_string(), check_db(repo_root)),
        ("index".to_string(), index),
        ("vector".to_string(), vector),
    ];
    let overall = aggregate(&checks);
    HealthReport {
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities,
        checks,
        overall,
        missing_indexes,
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

fn check_index(repo_root: &Path) -> (HealthCheck, Vec<String>) {
    let db = db_path(repo_root);
    if !db.exists() {
        return (
            HealthCheck::fail(format!(
                "{} not present — run `cognis-cli init` then index the repo",
                db.display()
            )),
            Vec::new(),
        );
    }
    // The DB exists: open it (migrations are idempotent on a Python-built DB)
    // and refuse to treat it as ready when a required bounded-read index is
    // missing (Requirement 9.7) or the symbol table is empty.
    let path = db.to_string_lossy().into_owned();
    match cognis_store::Database::open(&path) {
        Ok(database) => {
            let result = match database.missing_required_indexes() {
                Ok(missing) if !missing.is_empty() => {
                    let names = missing.join(", ");
                    (
                        HealthCheck::fail(format!("missing required index: {names}")),
                        missing,
                    )
                }
                Ok(_) => match database.count("symbol") {
                    Ok(0) => (
                        HealthCheck::fail(format!(
                            "{} has 0 symbols — index the repo before serving",
                            db.display()
                        )),
                        Vec::new(),
                    ),
                    Ok(n) => (
                        HealthCheck::ok(format!("{n} symbols indexed in UCKG")),
                        Vec::new(),
                    ),
                    Err(err) => (
                        HealthCheck::fail(format!("cannot read symbol count: {err}")),
                        Vec::new(),
                    ),
                },
                Err(err) => (
                    HealthCheck::fail(format!("cannot inspect required indexes: {err}")),
                    Vec::new(),
                ),
            };
            database.close_thread_connection();
            result
        }
        Err(err) => (
            HealthCheck::fail(format!("cannot open {}: {err}", db.display())),
            Vec::new(),
        ),
    }
}

fn check_vector(repo_root: &Path) -> HealthCheck {
    let db = db_path(repo_root);
    if !db.exists() {
        return HealthCheck::warn(
            "vector check skipped (no database); vector search would use \
             blob_linear_scan fallback"
                .to_string(),
        );
    }

    let path = db.to_string_lossy().into_owned();
    let database = match cognis_store::Database::open(&path) {
        Ok(database) => database,
        Err(err) => return HealthCheck::warn(format!("vector check failed: {err}")),
    };
    let backend = database.vector_search_backend();
    let ids = database.vec_symbol_ids();

    let check = match (backend, ids) {
        (Ok(VectorSearchBackend::Accelerated), Ok(ids)) if ids.is_empty() => {
            HealthCheck::warn("accelerated vector search active; no vectors stored yet".to_string())
        }
        (Ok(VectorSearchBackend::Accelerated), Ok(ids)) => HealthCheck::ok(format!(
            "accelerated vector search active; {} symbol vectors present",
            ids.len()
        )),
        (Ok(VectorSearchBackend::BlobLinearScan), Ok(ids)) if ids.is_empty() => HealthCheck::warn(
            "blob_linear_scan fallback active; no vectors stored yet (semantic search degraded)"
                .to_string(),
        ),
        (Ok(VectorSearchBackend::BlobLinearScan), Ok(ids)) => HealthCheck::ok(format!(
            "{} symbol vectors present; vector search running in blob_linear_scan fallback mode",
            ids.len()
        )),
        (Err(err), _) | (_, Err(err)) => {
            let msg = err.to_string();
            if msg.contains("vec0") {
                HealthCheck::warn(
                    "legacy vector index from another build; run Rebuild Index \
                     (after Install Backend) to migrate it and enable semantic search"
                        .to_string(),
                )
            } else {
                HealthCheck::warn(format!("vector check failed: {err}"))
            }
        }
    };

    check
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_store::Database;

    #[test]
    fn missing_required_index_is_named_on_the_health_report() {
        std::env::remove_var("COGNIS_DB_PATH");
        let dir = std::env::temp_dir().join(format!(
            "cognis-health-missing-idx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".cognis")).unwrap();
        let db_path = dir.join(".cognis").join("uckg.db");
        let database = Database::open(db_path.to_str().unwrap()).unwrap();
        database
            .connect()
            .unwrap()
            .execute("DROP INDEX idx_symbol_name_lower", [])
            .unwrap();
        database.close_thread_connection();
        drop(database);

        let report = build_health_report(&dir);
        assert_eq!(
            report.missing_indexes,
            vec!["idx_symbol_name_lower".to_string()]
        );
        assert_eq!(report.overall, HealthStatus::Fail);
        let index = report
            .checks
            .iter()
            .find(|(name, _)| name == "index")
            .map(|(_, check)| check)
            .expect("index check");
        assert_eq!(index.status, HealthStatus::Fail);
        assert!(
            index.message.contains("idx_symbol_name_lower"),
            "index check must name the missing index: {}",
            index.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
