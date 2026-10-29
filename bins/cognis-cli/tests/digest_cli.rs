use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cognis_store::{database_digest, Database};
use serde_json::Value;
use tempfile::TempDir;

struct DigestWorkspace {
    _dir: TempDir,
    repo_root: PathBuf,
    db_path: PathBuf,
}

fn digest_workspace() -> DigestWorkspace {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path().to_path_buf();
    let cognis_dir = repo_root.join(".cognis");
    fs::create_dir_all(&cognis_dir).expect("mkdir .cognis");
    let db_path = cognis_dir.join("uckg.db");
    DigestWorkspace {
        _dir: dir,
        repo_root,
        db_path,
    }
}

fn run_digest(repo_root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cognis-cli"))
        .arg("--repo-root")
        .arg(repo_root)
        .arg("digest")
        .env_remove("COGNIS_DB_PATH")
        .output()
        .expect("run cognis-cli digest")
}

#[test]
fn digest_command_matches_in_process_database_digest() {
    let ws = digest_workspace();
    let db = Database::open(&ws.db_path).expect("create database");
    let expected = serde_json::to_value(database_digest(&db).expect("database digest"))
        .expect("serialize expected digest");
    db.close_thread_connection();

    let output = run_digest(&ws.repo_root);
    assert!(
        output.status.success(),
        "digest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value = serde_json::from_slice(&output.stdout).expect("digest JSON");
    assert_eq!(actual, expected);
    assert_eq!(
        actual
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["combined", "edges", "files", "fts", "symbols", "vector_ids"]
    );
    for value in actual.as_object().unwrap().values() {
        assert!(value.as_str().is_some_and(
            |digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
        ));
    }
}

#[test]
fn digest_command_refuses_to_create_a_missing_database() {
    let ws = digest_workspace();
    let output = run_digest(&ws.repo_root);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("database not found"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!ws.db_path.exists(), "digest must not create a database");
}
