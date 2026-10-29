//! `owner-status.json` — superset of today's `indexd-status.json` (task 13.4).
//!
//! Every existing `IndexStatusReport` key is preserved so old readers keep
//! working; the owner-only keys are additive (Requirement 2.6, 18.5, 20.7).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cognis_core::Capabilities;
use serde::Serialize;

/// Filename under `.cognis/` (Requirement 2.6).
pub const OWNER_STATUS_FILE: &str = "owner-status.json";

/// Status document written by the workspace owner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerStatus {
    pub pid: u32,
    pub active: bool,
    pub phase: String,
    pub message: String,
    pub progress_percent: Option<f64>,
    pub pending_count: usize,
    pub pending_files: Vec<String>,
    pub inflight_count: usize,
    pub inflight_files: Vec<String>,
    pub recent_files: Vec<String>,
    pub updated_at: f64,
    pub last_error: Option<String>,
    pub source_generation: i64,
    pub semantic_generation: i64,
    pub pending_vector_count: i64,
    pub watcher_queue_depth: usize,
    pub model_resident: bool,
    pub fingerprint_match: bool,
    pub capabilities: Capabilities,
    pub canonical_workspace: String,
}

impl OwnerStatus {
    pub fn now_secs() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// `.cognis/owner-status.json` under a repository root.
pub fn owner_status_path(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root
        .as_ref()
        .join(cognis_core::config::CONFIG_DIR_NAME)
        .join(OWNER_STATUS_FILE)
}

/// Write the status snapshot atomically (tmp + rename), retrying the rename
/// so a concurrent IDE reader on Windows cannot fail the write.
pub fn write_owner_status(path: &Path, status: &OwnerStatus) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(status)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text.as_bytes())?;

    let mut last_err = None;
    for attempt in 0..10 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_err = Some(error);
                std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("owner-status rename failed")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_index_status_keys_and_additive_owner_keys() {
        let status = OwnerStatus {
            pid: 1234,
            active: true,
            phase: "incremental".into(),
            message: "indexing".into(),
            progress_percent: Some(65.0),
            pending_count: 3,
            pending_files: vec!["a.rs".into()],
            inflight_count: 1,
            inflight_files: vec!["b.rs".into()],
            recent_files: vec!["c.rs".into()],
            updated_at: 1730000000.0,
            last_error: None,
            source_generation: 42,
            semantic_generation: 41,
            pending_vector_count: 17,
            watcher_queue_depth: 0,
            model_resident: true,
            fingerprint_match: true,
            capabilities: Capabilities {
                semantic_backend: true,
                accelerated_vector_search: false,
            },
            canonical_workspace: "wire-key".into(),
        };
        let value = serde_json::to_value(&status).unwrap();
        for key in [
            "pid",
            "active",
            "phase",
            "message",
            "progressPercent",
            "pendingCount",
            "pendingFiles",
            "inflightCount",
            "inflightFiles",
            "recentFiles",
            "updatedAt",
            "lastError",
            "sourceGeneration",
            "semanticGeneration",
            "pendingVectorCount",
            "watcherQueueDepth",
            "modelResident",
            "fingerprintMatch",
            "capabilities",
            "canonicalWorkspace",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["sourceGeneration"], 42);
        assert_eq!(value["watcherQueueDepth"], 0);
        assert_eq!(value["modelResident"], true);
        assert_eq!(value["canonicalWorkspace"], "wire-key");
        assert_eq!(value["capabilities"]["semantic_backend"], true);
    }
}
