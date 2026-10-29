//! Executable spike: compare one long-lived incremental UCKG with a clean rebuild.
//! Degenerate transitions (empty, invalid UTF-8, rename, re-add, missed delete)
//! must match a clean rebuild; file-cache rows exclude `parsed_at` so wall-clock
//! parse times do not masquerade as content drift.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;
use serde_json::{json, Value};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cognis-spike-digest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Digest {
    symbols: Vec<String>,
    edges: Vec<String>,
    fts_ids: Vec<String>,
    vector_ids: Vec<String>,
    files: Vec<String>,
}
impl Digest {
    fn read(db: &Database) -> Self {
        let symbols = db
            .list_symbols()
            .unwrap()
            .into_iter()
            .map(|s| {
                // Deliberately enumerate every canonical Symbol field except updated_at.
                serde_json::to_string(&json!({
                    "id": s.id, "kind": s.kind, "name": s.name,
                    "qualified_name": s.qualified_name, "language": s.language,
                    "module": s.module, "file_path": s.file_path,
                    "line_start": s.line_start, "line_end": s.line_end,
                    "signature": s.signature, "docstring": s.docstring,
                    "content_hash": s.content_hash, "body_excerpt": s.body_excerpt,
                    "semantic_summary": s.semantic_summary, "risk_score": s.risk_score,
                    "ambiguous": s.ambiguous, "untrusted_flags": s.untrusted_flags,
                }))
                .unwrap()
            })
            .collect();
        let edges = db
            .list_edges()
            .unwrap()
            .into_iter()
            .map(|e| {
                serde_json::to_string(&json!({
                    "src_id": e.src_id, "dst_id": e.dst_id, "kind": e.kind,
                    "confidence": e.confidence, "meta": canonical_json(e.meta),
                }))
                .unwrap()
            })
            .collect();
        let mut fts_ids = query_strings(db, "SELECT id FROM symbol_fts ORDER BY id");
        let mut vector_ids = db.vec_symbol_ids().unwrap();
        let files = query_strings(
            db,
            "SELECT path || '|' || language || '|' || size_bytes || '|' || content_hash || '|' || parse_status FROM file ORDER BY path",
        );
        fts_ids.sort();
        vector_ids.sort();
        Self {
            symbols,
            edges,
            fts_ids,
            vector_ids,
            files,
        }
    }

    fn counts(&self) -> Value {
        json!({
            "symbols": self.symbols.len(), "edges": self.edges.len(),
            "fts_ids": self.fts_ids.len(), "vector_ids": self.vector_ids.len(),
            "file_rows": self.files.len(),
        })
    }

    fn diff(&self, clean: &Self) -> Value {
        json!({
            "symbols": component_diff(&self.symbols, &clean.symbols),
            "edges": component_diff(&self.edges, &clean.edges),
            "fts_ids": component_diff(&self.fts_ids, &clean.fts_ids),
            "vector_ids": component_diff(&self.vector_ids, &clean.vector_ids),
            "file_rows": component_diff(&self.files, &clean.files),
        })
    }
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(k, v)| (k, canonical_json(v)))
                .collect::<std::collections::BTreeMap<_, _>>();
            serde_json::to_value(sorted).unwrap()
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

fn query_strings(db: &Database, sql: &str) -> Vec<String> {
    let conn = db.connect().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
}

fn component_diff(actual: &[String], clean: &[String]) -> Value {
    let a: BTreeSet<_> = actual.iter().cloned().collect();
    let b: BTreeSet<_> = clean.iter().cloned().collect();
    json!({
        "actual_count": actual.len(), "clean_count": clean.len(),
        "only_incremental": a.difference(&b).take(8).cloned().collect::<Vec<_>>(),
        "only_clean": b.difference(&a).take(8).cloned().collect::<Vec<_>>(),
    })
}
fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn initial_repo(repo: &Path) {
    write(
        &repo.join("caller.py"),
        "from callee import helper\n\ndef caller():\n    return helper()\n",
    );
    write(&repo.join("callee.py"), "def helper():\n    return 1\n");
}

fn clean_digest(repo: &Path, db_path: &Path) -> Digest {
    let db = Database::open(db_path).unwrap();
    let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
    pipe.index_repo(repo, true).unwrap();
    let digest = Digest::read(&db);
    db.close_thread_connection();
    digest
}

fn compare_step(
    step: &str,
    repo: &Path,
    inc_db: &Database,
    scratch: &Path,
    mismatches: &mut Vec<String>,
) -> bool {
    let clean_path = scratch.join(format!("clean-{step}.db"));
    let clean = clean_digest(repo, &clean_path);
    let incremental = Digest::read(inc_db);
    let equal = incremental == clean;
    if !equal {
        mismatches.push(step.to_string());
    }
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "tier": "EMPIRICALLY_SUPPORTED",
            "step": step,
            "equal": equal,
            "incremental_counts": incremental.counts(),
            "clean_counts": clean.counts(),
            "component_diff": incremental.diff(&clean),
        }))
        .unwrap()
    );
    equal
}

#[test]
fn long_lived_incremental_db_is_compared_with_clean_rebuild_at_every_step() {
    let scratch = Scratch::new();
    let repo = scratch.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    initial_repo(&repo);

    let db = Database::open(scratch.0.join("incremental.db")).unwrap();
    let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
    pipe.index_repo(&repo, true).unwrap();
    let mut mismatches = Vec::new();

    // Baseline after the initial cold build must equal a separate clean build.
    assert!(compare_step(
        "initial",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches
    ));

    // Control: re-indexing unchanged valid source must equal a clean build.
    pipe.index_changed_files(&[repo.join("caller.py")], &repo)
        .unwrap();
    assert!(compare_step(
        "control_unchanged",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches
    ));

    write(
        &repo.join("caller.py"),
        "from callee import helper\n\ndef caller():\n    return helper() + 1\n",
    );
    pipe.index_changed_files(&[repo.join("caller.py")], &repo)
        .unwrap();
    assert!(compare_step(
        "caller_edit",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches
    ));

    write(
        &repo.join("callee.py"),
        "def renamed_helper():\n    return 2\n",
    );
    pipe.index_changed_files(&[repo.join("callee.py")], &repo)
        .unwrap();
    let callee_equal = compare_step("callee_edit", &repo, &db, &scratch.0, &mut mismatches);

    std::fs::remove_file(repo.join("callee.py")).unwrap();
    pipe.index_batch(&repo, &[repo.join("callee.py")]).unwrap();
    let delete_equal = compare_step("delete", &repo, &db, &scratch.0, &mut mismatches);

    write(&repo.join("callee.py"), "def helper():\n    return 3\n");
    pipe.index_batch(&repo, &[repo.join("callee.py")]).unwrap();
    let readd_equal = compare_step("re_add", &repo, &db, &scratch.0, &mut mismatches);

    std::fs::rename(repo.join("callee.py"), repo.join("renamed.py")).unwrap();
    pipe.index_batch(&repo, &[repo.join("callee.py"), repo.join("renamed.py")])
        .unwrap();
    let rename_equal = compare_step("rename", &repo, &db, &scratch.0, &mut mismatches);

    write(
        &repo.join("caller.py"),
        "def branch_caller():\n    return 7\n",
    );
    pipe.index_changed_files(&[repo.join("caller.py")], &repo)
        .unwrap();
    let branch_equal = compare_step(
        "caller_branch_replacement",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches,
    );

    write(&repo.join("caller.py"), "");
    pipe.index_changed_files(&[repo.join("caller.py")], &repo)
        .unwrap();
    let empty_equal = compare_step("valid_to_empty", &repo, &db, &scratch.0, &mut mismatches);

    write(&repo.join("caller.py"), [0xff, 0xfe, 0x80]);
    pipe.index_changed_files(&[repo.join("caller.py")], &repo)
        .unwrap();
    let invalid_equal = compare_step(
        "valid_to_invalid_utf8",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches,
    );

    write(&repo.join("missed.py"), "def missed():\n    return 1\n");
    pipe.index_changed_files(&[repo.join("missed.py")], &repo)
        .unwrap();
    std::fs::remove_file(repo.join("missed.py")).unwrap();
    // Deliberately omit the delete event, then request the documented full scan.
    pipe.index_repo(&repo, true).unwrap();
    let full_equal = compare_step(
        "full_rescan_missed_delete",
        &repo,
        &db,
        &scratch.0,
        &mut mismatches,
    );

    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "tier": "EMPIRICALLY_SUPPORTED",
            "mismatch_steps": mismatches,
            "known_counterexamples_observed": {
                "callee_inbound_reresolution": !callee_equal,
                "delete_after_stale_inbound_edge": !delete_equal,
                "readd_inbound_reresolution": !readd_equal,
                "rename_after_stale_inbound_edge": !rename_equal,
                "caller_branch_replacement_after_rename": !branch_equal,
                "valid_to_empty_stale_symbols": !empty_equal,
                "valid_to_invalid_utf8_stale_symbols": !invalid_equal,
                "full_rescan_missed_delete": !full_equal,
            },
            "boundaries": "finite deterministic Python fixture; no crash-atomicity claim",
        }))
        .unwrap()
    );

    assert!(
        mismatches.is_empty(),
        "incremental digest diverged from clean rebuild at {mismatches:?}"
    );
    assert!(callee_equal);
    assert!(delete_equal);
    assert!(readd_equal);
    assert!(rename_equal);
    assert!(branch_equal);
    assert!(empty_equal);
    assert!(invalid_equal);
    assert!(full_equal);
    assert!(
        db.count("file").unwrap() > 0,
        "degenerate transitions retain file-cache rows"
    );
    db.close_thread_connection();
}
