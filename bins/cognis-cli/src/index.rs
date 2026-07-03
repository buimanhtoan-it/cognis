//! `cognis-cli index` — indexer entry point.
//!
//! `--clear` removes stored index artifacts under `.cognis/` (filesystem only).
//! Otherwise the native `cognis-indexer` pipeline runs a cold/incremental pass
//! over the repo (parse → resolve → enrich/scrub → write, plus embeddings when
//! an embedder backend is compiled in), so `bootstrap` and `index` produce a
//! real, queryable UCKG. The daemon (`cognis-indexd`) shares the same pipeline
//! for the live watch loop.

use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;

use cognis_core::config::CONFIG_DIR_NAME;
use cognis_core::Config;

use crate::IndexArgs;

/// Outcome of an `index` invocation (structured for `bootstrap --json`).
#[derive(Debug, Clone, Serialize)]
pub struct IndexOutcome {
    /// `cleared` | `done` | `failed`.
    pub status: String,
    pub message: String,
    /// Artifact names removed by `--clear` (empty otherwise).
    #[serde(default)]
    pub cleared: Vec<String>,
    /// Symbols persisted this run (0 for `--clear` / on failure).
    #[serde(default)]
    pub symbols_indexed: usize,
    /// Edges persisted this run.
    #[serde(default)]
    pub edges_resolved: usize,
}

pub fn cmd_index(repo_root: &Path, args: &IndexArgs) -> cognis_core::Result<ExitCode> {
    let outcome = index_outcome(repo_root, args);
    println!("{}", outcome.message);
    // A failed pipeline run is a non-zero exit so scripts/extension can react.
    Ok(if outcome.status == "failed" {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The UCKG path for `repo_root` (`COGNIS_DB_PATH` override, else default).
fn db_path_for(repo_root: &Path) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COGNIS_DB_PATH") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    repo_root.join(CONFIG_DIR_NAME).join("uckg.db")
}

/// Compute the index outcome (shared with `bootstrap`).
///
/// Runs the real `cognis-indexer` pipeline unless `--clear` is set. The
/// embedder is built best-effort from config (`embedder.backend`): unavailable
/// backends (e.g. `onnx` not compiled in, `--skip-embeddings`) degrade to
/// lexical/structural indexing rather than failing the run.
pub fn index_outcome(repo_root: &Path, args: &IndexArgs) -> IndexOutcome {
    let target = args.path.clone().unwrap_or_else(|| repo_root.to_path_buf());

    if args.clear {
        let cleared = clear_index_artifacts(&target);
        return IndexOutcome {
            status: "cleared".to_string(),
            message: if cleared.is_empty() {
                "no index artifacts to clear".to_string()
            } else {
                format!("cleared index artifacts: {}", cleared.join(", "))
            },
            cleared,
            symbols_indexed: 0,
            edges_resolved: 0,
        };
    }

    let mode = if args.full {
        "full (cold)"
    } else {
        "incremental"
    };
    let db_path = db_path_for(&target);
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let config = Config::load(&target).unwrap_or_default();

    // `--skip-embeddings` indexes with **no embedder** (fast; no model, and no
    // vectors written) — distinct from a stub embedder, which would persist
    // zero-vectors and falsely report a populated semantic index. Otherwise the
    // embedder is built best-effort from config (`embedder.backend`).
    let opened = if args.skip_embeddings {
        cognis_store::Database::open(&db_path)
            .map(|db| cognis_indexer::IndexerPipeline::new(db, config))
    } else {
        cognis_indexer::IndexerPipeline::open(&db_path, config)
    };
    let mut pipeline = match opened {
        Ok(p) => p,
        Err(e) => {
            return IndexOutcome {
                status: "failed".to_string(),
                message: format!("index ({mode}) failed to open UCKG: {e}"),
                cleared: Vec::new(),
                symbols_indexed: 0,
                edges_resolved: 0,
            };
        }
    };

    match pipeline.index_repo(&target, args.full) {
        Ok(stats) => IndexOutcome {
            status: "done".to_string(),
            message: format!(
                "indexed {} ({mode}): {} symbols, {} edges across {} files",
                target.display(),
                stats.symbols_indexed,
                stats.edges_resolved,
                stats.files_processed
            ),
            cleared: Vec::new(),
            symbols_indexed: stats.symbols_indexed,
            edges_resolved: stats.edges_resolved,
        },
        Err(e) => IndexOutcome {
            status: "failed".to_string(),
            message: format!("index ({mode}) failed: {e}"),
            cleared: Vec::new(),
            symbols_indexed: 0,
            edges_resolved: 0,
        },
    }
}

/// Delete stored index artifacts under `.cognis/` (best-effort). Preserves
/// `config.yaml` and the config revision so user settings survive a reset.
/// Mirrors the Python `_clear_index_artifacts` target set.
fn clear_index_artifacts(repo_root: &Path) -> Vec<String> {
    let cognis_dir = repo_root.join(CONFIG_DIR_NAME);
    let db = if let Ok(p) = std::env::var("COGNIS_DB_PATH") {
        if p.trim().is_empty() {
            cognis_dir.join("uckg.db")
        } else {
            std::path::PathBuf::from(p)
        }
    } else {
        cognis_dir.join("uckg.db")
    };

    let status = cognis_dir.join("indexd-status.json");
    let files: Vec<std::path::PathBuf> = vec![
        db.clone(),
        db.with_extension("db-wal"),
        db.with_extension("db-shm"),
        db.with_extension("db-journal"),
        status.clone(),
        status.with_extension("json.tmp"),
    ];
    let dirs: Vec<std::path::PathBuf> = vec![cognis_dir.join("capsule_cache")];

    let mut removed = Vec::new();
    for f in files {
        if f.exists() && std::fs::remove_file(&f).is_ok() {
            if let Some(name) = f.file_name() {
                removed.push(name.to_string_lossy().into_owned());
            }
        }
    }
    for d in dirs {
        if d.exists() && std::fs::remove_dir_all(&d).is_ok() {
            if let Some(name) = d.file_name() {
                removed.push(name.to_string_lossy().into_owned());
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_removes_db_and_caches_preserves_config() {
        // Isolate from any ambient COGNIS_DB_PATH so the default-path resolution
        // under test is deterministic regardless of the caller's environment.
        std::env::remove_var("COGNIS_DB_PATH");
        let repo = std::env::temp_dir().join(format!(
            "cognis-idx-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cognis = repo.join(".cognis");
        std::fs::create_dir_all(cognis.join("capsule_cache")).unwrap();
        std::fs::write(cognis.join("uckg.db"), b"db").unwrap();
        std::fs::write(cognis.join("config.yaml"), "x: 1\n").unwrap();
        std::fs::write(cognis.join("indexd-status.json"), "{}").unwrap();

        let args = IndexArgs {
            path: Some(repo.clone()),
            full: false,
            clear: true,
            skip_embeddings: false,
        };
        let outcome = index_outcome(&repo, &args);
        assert_eq!(outcome.status, "cleared");
        assert!(!cognis.join("uckg.db").exists());
        assert!(!cognis.join("capsule_cache").exists());
        assert!(!cognis.join("indexd-status.json").exists());
        // config preserved.
        assert!(cognis.join("config.yaml").exists());

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn index_runs_the_real_pipeline_and_reports_counts() {
        std::env::remove_var("COGNIS_DB_PATH");
        let repo = std::env::temp_dir().join(format!(
            "cognis-idx-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("a.py"),
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        )
        .unwrap();

        let args = IndexArgs {
            path: Some(repo.clone()),
            full: true,
            clear: false,
            skip_embeddings: true, // stub embedder → no model needed offline
        };
        let outcome = index_outcome(&repo, &args);
        assert_eq!(outcome.status, "done", "message: {}", outcome.message);
        assert!(outcome.symbols_indexed >= 2, "expected symbols indexed");

        // The UCKG now has the symbols (a real, queryable index).
        let db = repo.join(".cognis").join("uckg.db");
        let database = cognis_store::Database::open(db.to_string_lossy().as_ref()).unwrap();
        assert!(database.count("symbol").unwrap() >= 2);

        std::fs::remove_dir_all(&repo).ok();
    }
}
