//! Property 11: Dependents are re-resolved with their target (task 7.5).
//!
//! For any generated symbol graph and any change set, every symbol whose
//! resolved edge targets a changed, added, deleted, or renamed symbol — or a
//! changed configuration key — has its file in the same `Publish_Batch`. A
//! deleted file leaves no symbol, FTS row, vector, outbound edge, queue row,
//! or cache row behind.
//!
//! Uses [`arb_source_tree`]. A deterministic YAML key + Python reader is
//! injected so configuration-key dependents (Requirement 10.2) are reachable
//! even when the generated tree is code-only.
//!
//! Validates: Requirements 10.1, 10.2, 10.3
//! Properties: P11

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cognis_core::Config;
use cognis_indexer::{ChangePlanner, PlanRequest};
use cognis_store::{Database, IndexPublisher};
use cognis_testgen::arb_source_tree;
use proptest::prelude::*;
use rusqlite::params;
use tempfile::TempDir;

const CONFIG_PATH: &str = "p11_settings.yaml";
const READER_PATH: &str = "p11_reader.py";
const CONFIG_KEY: &str = "p11_port";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    EditTarget,
    DeleteFile,
    RenameFile,
    EditConfig,
}

fn arb_change_kind() -> impl Strategy<Value = ChangeKind> {
    prop_oneof![
        3 => Just(ChangeKind::EditTarget),
        2 => Just(ChangeKind::DeleteFile),
        2 => Just(ChangeKind::RenameFile),
        2 => Just(ChangeKind::EditConfig),
    ]
}

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<Path>) -> Self {
        Self(Database::open(path).expect("open P11 database"))
    }

    fn get(&self) -> &Database {
        &self.0
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        self.0.close_thread_connection();
    }
}

fn inject_config_fixture(root: &Path) {
    fs::write(root.join(CONFIG_PATH), format!("{CONFIG_KEY}: 8080\n"))
        .expect("write config fixture");
    fs::write(
        root.join(READER_PATH),
        format!("def p11_read_port():\n    return os.getenv(\"{CONFIG_KEY}\")\n"),
    )
    .expect("write config reader");
}

fn on_disk(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}

fn publish(db: &Database, batch: &cognis_store::PublishBatch) {
    let mut writable = db.clone();
    writable.publish(batch).expect("publish P11 batch");
}

fn symbol_files(db: &Database) -> BTreeMap<String, String> {
    let conn = db.connect().expect("connect symbols");
    let mut stmt = conn
        .prepare("SELECT id, file_path FROM symbol")
        .expect("prepare symbol query");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query symbols")
        .collect::<rusqlite::Result<BTreeMap<String, String>>>()
        .expect("read symbols")
}

fn resolved_edges(db: &Database) -> Vec<(String, String)> {
    let conn = db.connect().expect("connect edges");
    let mut stmt = conn
        .prepare("SELECT src_id, dst_id FROM edge")
        .expect("prepare edge query");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query edges")
        .collect::<rusqlite::Result<Vec<(String, String)>>>()
        .expect("read edges")
}

/// Files that have a resolved edge targeting a symbol in `target_ids`.
fn dependent_files(
    edges: &[(String, String)],
    files: &BTreeMap<String, String>,
    target_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (src, dst) in edges {
        if target_ids.contains(dst) {
            if let Some(path) = files.get(src) {
                out.insert(path.clone());
            }
        }
    }
    out
}

fn ids_in_file(files: &BTreeMap<String, String>, path: &str) -> BTreeSet<String> {
    files
        .iter()
        .filter(|(_, file)| *file == path)
        .map(|(id, _)| id.clone())
        .collect()
}

fn covered_paths(batch: &cognis_store::PublishBatch) -> BTreeSet<String> {
    batch.covered_paths().into_iter().collect()
}

fn count_query(db: &Database, sql: &str, bind: &str) -> i64 {
    db.connect()
        .expect("connect leftover query")
        .query_row(sql, params![bind], |row| row.get(0))
        .expect("count leftover rows")
}

fn leftover_rows(db: &Database, path: &str, deleted_ids: &BTreeSet<String>) -> Vec<String> {
    let mut leftover = Vec::new();
    if count_query(db, "SELECT COUNT(*) FROM symbol WHERE file_path = ?1", path) > 0 {
        leftover.push("symbol".into());
    }
    if count_query(db, "SELECT COUNT(*) FROM file WHERE path = ?1", path) > 0 {
        leftover.push("file".into());
    }
    if count_query(
        db,
        "SELECT COUNT(*) FROM embedding_queue WHERE file_path = ?1",
        path,
    ) > 0
    {
        leftover.push("embedding_queue".into());
    }
    let conn = db.connect().expect("connect id leftovers");
    for id in deleted_ids {
        let fts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbol_fts WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if fts > 0 {
            leftover.push(format!("fts:{id}"));
        }
        let vecs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbol_vec WHERE symbol_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if vecs > 0 {
            leftover.push(format!("vector:{id}"));
        }
        let outbound: i64 = conn
            .query_row("SELECT COUNT(*) FROM edge WHERE src_id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        if outbound > 0 {
            leftover.push(format!("outbound:{id}"));
        }
        let queued: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embedding_queue WHERE symbol_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if queued > 0 {
            leftover.push(format!("queue:{id}"));
        }
    }
    leftover
}

fn append_byte_change(path: &Path) {
    let mut bytes = fs::read(path).unwrap_or_default();
    bytes.extend_from_slice(b"\n#p11\n");
    fs::write(path, bytes).expect("append byte change");
}

fn pick(items: &[String], ordinal: usize) -> Option<&str> {
    if items.is_empty() {
        None
    } else {
        Some(items[ordinal % items.len()].as_str())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property 11: inbound dependents of a changed target ride in the same
    /// batch; a deleted path retains no derived rows.
    #[test]
    fn dependents_are_re_resolved_with_their_target(
        tree in arb_source_tree(),
        kind in arb_change_kind(),
        ordinal in 0usize..32,
    ) {
        let root = TempDir::new().expect("P11 workspace");
        tree.materialize(root.path()).expect("materialize tree");
        inject_config_fixture(root.path());

        let testdb = TestDatabase::open(root.path().join("index.db"));
        let config = Config::default();
        let planner = ChangePlanner::new(testdb.get(), &config, root.path());
        let initial = planner.plan(PlanRequest::Full).expect("initial full index");
        publish(testdb.get(), &initial);

        let files = symbol_files(testdb.get());
        let edges = resolved_edges(testdb.get());
        let mut indexed_paths: Vec<String> = files.values().cloned().collect();
        indexed_paths.sort();
        indexed_paths.dedup();

        let dst_paths: Vec<String> = {
            let mut paths: Vec<String> = edges
                .iter()
                .filter_map(|(_, dst)| files.get(dst).cloned())
                .collect();
            paths.sort();
            paths.dedup();
            paths
        };

        let mut expected_dependents;
        let mut deleted_path: Option<String> = None;
        let mut deleted_ids = BTreeSet::new();
        let mut touched: Vec<PathBuf> = Vec::new();

        match kind {
            ChangeKind::EditConfig => {
                fs::write(
                    on_disk(root.path(), CONFIG_PATH),
                    format!("{CONFIG_KEY}: 9090\n"),
                )
                .expect("edit config key");
                let targets = ids_in_file(&files, CONFIG_PATH);
                expected_dependents = dependent_files(&edges, &files, &targets);
                touched.push(on_disk(root.path(), CONFIG_PATH));
            }
            ChangeKind::EditTarget => {
                let path = pick(&dst_paths, ordinal)
                    .or_else(|| pick(&indexed_paths, ordinal))
                    .unwrap_or(CONFIG_PATH);
                append_byte_change(&on_disk(root.path(), path));
                let targets = ids_in_file(&files, path);
                expected_dependents = dependent_files(&edges, &files, &targets);
                expected_dependents.insert(path.to_string());
                touched.push(on_disk(root.path(), path));
            }
            ChangeKind::DeleteFile => {
                let path = pick(&indexed_paths, ordinal).unwrap_or(READER_PATH);
                let abs = on_disk(root.path(), path);
                if abs.exists() {
                    fs::remove_file(&abs).expect("delete file");
                }
                deleted_ids = ids_in_file(&files, path);
                expected_dependents = dependent_files(&edges, &files, &deleted_ids);
                deleted_path = Some(path.to_string());
                touched.push(abs);
            }
            ChangeKind::RenameFile => {
                let path = pick(&indexed_paths, ordinal).unwrap_or(READER_PATH);
                let old = on_disk(root.path(), path);
                let renamed = match path.rsplit_once('.') {
                    Some((stem, ext)) => format!("{stem}_renamed.{ext}"),
                    None => format!("{path}_renamed"),
                };
                let new_abs = on_disk(root.path(), &renamed);
                if old.exists() && !new_abs.exists() {
                    if let Some(parent) = new_abs.parent() {
                        fs::create_dir_all(parent).ok();
                    }
                    fs::rename(&old, &new_abs).expect("rename file");
                    deleted_ids = ids_in_file(&files, path);
                    expected_dependents = dependent_files(&edges, &files, &deleted_ids);
                    deleted_path = Some(path.to_string());
                    touched.push(old);
                    touched.push(new_abs);
                } else {
                    append_byte_change(&old);
                    let targets = ids_in_file(&files, path);
                    expected_dependents = dependent_files(&edges, &files, &targets);
                    expected_dependents.insert(path.to_string());
                    touched.push(old);
                }
            }
        }

        let batch = planner
            .plan(PlanRequest::Touched(touched))
            .expect("plan change");
        let covered = covered_paths(&batch);
        for path in &expected_dependents {
            prop_assert!(
                covered.contains(path),
                "dependent file {path} must ride with its changed target ({kind:?})"
            );
        }

        if let Some(path) = deleted_path {
            publish(testdb.get(), &batch);
            let leftover = leftover_rows(testdb.get(), &path, &deleted_ids);
            prop_assert!(
                leftover.is_empty(),
                "deleted file {path} left rows behind: {leftover:?}"
            );
        }
    }
}
