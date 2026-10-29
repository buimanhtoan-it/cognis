//! Property 10: Cache-driven minimal batch (task 7.4).
//!
//! For any generated source tree and any `File_Cache` state, a `Reconcile`
//! `Publish_Batch` covers exactly the union of changed, added, and removed
//! paths under the three-part unchanged rule (hash equal **and** parse status
//! successful **and** symbols present). Every indexed file in the batch carries
//! a complete `file` row (path, language, size, hash, parse time, parse
//! status). A `Full` plan covers every admitted path regardless of hash
//! equality.
//!
//! Uses [`arb_source_tree`] and [`arb_file_cache_state_over`].
//!
//! Validates: Requirements 8.1, 8.2, 8.3, 8.4, 8.5, 8.6, 8.7, 8.8, 3.4, 4.10
//! Properties: P10

use std::collections::BTreeSet;
use std::path::Path;

use cognis_core::{Config, ParseStatus};
use cognis_indexer::{ChangePlanner, PlanRequest};
use cognis_store::{Database, FileCacheRow, IndexPublisher};
use cognis_testgen::{arb_file_cache_state_over, arb_source_tree, FileCacheState};
use proptest::prelude::*;
use rusqlite::params;
use tempfile::TempDir;

fn arb_p10_state() -> impl Strategy<Value = FileCacheState> {
    arb_source_tree().prop_flat_map(arb_file_cache_state_over)
}

struct TestDatabase(Database);

impl TestDatabase {
    fn open(path: impl AsRef<Path>) -> Self {
        Self(Database::open(path).expect("open P10 database"))
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

fn parse_status_db(status: ParseStatus) -> &'static str {
    match status {
        ParseStatus::Ok => "ok",
        ParseStatus::Partial => "partial",
        ParseStatus::Failed => "failed",
    }
}

fn language_for(state: &FileCacheState, path: &str) -> String {
    state
        .tree
        .file(path)
        .map(|file| file.language.to_string())
        .unwrap_or_else(|| "python".to_string())
}

/// Seed `file` (and, when the cache claims symbols are present, a sentinel
/// symbol row) so the planner's three-part skip rule sees the generated state.
///
/// Sentinel names are outside the generator's name pool so a later parse of a
/// changed file cannot pull a Current path in as an Affected_Dependent — P11
/// owns that expansion; P10 measures the cache classification alone.
fn seed_cache(db: &Database, state: &FileCacheState) {
    let conn = db.connect().expect("connect P10 database");
    for entry in &state.entries {
        let Some(row) = entry.row.as_ref() else {
            continue;
        };
        let size_bytes = state
            .tree
            .file(&entry.path)
            .map(|file| file.bytes().len() as i64)
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO file(path, language, size_bytes, content_hash, parsed_at, parse_status) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.path,
                language_for(state, &entry.path),
                size_bytes,
                row.content_hash,
                1_700_000_000_i64,
                parse_status_db(row.parse_status),
            ],
        )
        .expect("insert File_Cache row");
        if row.symbols_present && !must_cover_on_reconcile(state, &entry.path) {
            let id = format!("p10-sentinel:{}:@present", entry.path);
            conn.execute(
                "INSERT INTO symbol(\
                    id, kind, name, qualified_name, language, module, file_path, \
                    line_start, line_end, content_hash, risk_score, ambiguous, \
                    untrusted_flags, updated_at\
                 ) VALUES(?1, 'function', '__p10_present', '__p10_present', ?2, '', \
                          ?3, 1, 1, ?4, 0, 0, '[]', 1)",
                params![
                    id,
                    language_for(state, &entry.path),
                    entry.path,
                    row.content_hash,
                ],
            )
            .expect("insert sentinel symbol so Current rows are skippable");
        }
    }
}

fn is_legitimately_symbol_less(tree: &cognis_testgen::SourceTree, path: &str) -> bool {
    tree.file(path).is_some_and(|file| {
        std::str::from_utf8(&file.bytes()).is_ok_and(|source| source.trim().is_empty())
    })
}

/// Three-part unchanged rule (Requirement 8.2, 8.8) plus the empty-file
/// exception from task 7.1: a path whose bytes are UTF-8 whitespace is
/// legitimately symbol-less, so a matching hash + successful parse is enough
/// to skip it even when `symbols_present` is false.
fn must_cover_on_reconcile(state: &FileCacheState, path: &str) -> bool {
    let Some(entry) = state.entry(path) else {
        return true;
    };
    if entry.must_be_deleted() {
        return true;
    }
    let Some(file) = state.tree.file(path) else {
        return true;
    };
    let Some(row) = entry.row.as_ref() else {
        return true;
    };
    let hash_match = row.content_hash == file.content_hash();
    let parse_ok = row.parse_status == ParseStatus::Ok;
    let symbols_ok = row.symbols_present || is_legitimately_symbol_less(&state.tree, path);
    !(hash_match && parse_ok && symbols_ok)
}

fn expected_reconcile_paths(state: &FileCacheState) -> BTreeSet<String> {
    state
        .entries
        .iter()
        .filter(|entry| must_cover_on_reconcile(state, &entry.path))
        .map(|entry| entry.path.clone())
        .collect()
}

fn expected_skipped_paths(state: &FileCacheState) -> BTreeSet<String> {
    state
        .entries
        .iter()
        .filter(|entry| !must_cover_on_reconcile(state, &entry.path))
        .map(|entry| entry.path.clone())
        .collect()
}

fn sorted(paths: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = paths.into_iter().collect();
    out.sort();
    out.dedup();
    out
}

fn row_complete(row: &FileCacheRow) -> bool {
    !row.path.is_empty()
        && !row.language.is_empty()
        && !row.content_hash.is_empty()
        && row.parsed_at > 0
        && matches!(
            row.parse_status,
            ParseStatus::Ok | ParseStatus::Partial | ParseStatus::Failed
        )
}

fn file_table_row(db: &Database, path: &str) -> Option<(String, i64, String, i64, String)> {
    let conn = db.connect().expect("connect file-table read");
    conn.query_row(
        "SELECT language, size_bytes, content_hash, parsed_at, parse_status \
         FROM file WHERE path = ?1",
        [path],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .ok()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property 10: Reconcile covers exactly the cache-driven union; Full
    /// covers every admitted path; every indexed file carries a complete
    /// File_Cache row that the publisher actually writes.
    #[test]
    fn cache_driven_minimal_batch(state in arb_p10_state()) {
        let tree = &state.tree;
        let expected = expected_reconcile_paths(&state);
        let skipped = expected_skipped_paths(&state);
        let expected_deletes: BTreeSet<String> =
            state.expected_delete_paths().into_iter().collect();
        let admitted: BTreeSet<String> = tree.paths().into_iter().collect();

        let root = TempDir::new().expect("P10 workspace");
        tree.materialize(root.path()).expect("materialize source tree");
        let testdb = TestDatabase::open(root.path().join("index.db"));
        seed_cache(testdb.get(), &state);
        let config = Config::default();
        let planner = ChangePlanner::new(testdb.get(), &config, root.path());

        let reconcile = planner
            .plan(PlanRequest::Reconcile)
            .expect("reconcile plan");
        let covered: BTreeSet<String> = reconcile.covered_paths().into_iter().collect();

        prop_assert_eq!(
            &covered,
            &expected,
            "Reconcile must cover exactly changed ∪ added ∪ removed"
        );
        for path in &skipped {
            prop_assert!(
                !covered.contains(path),
                "Current path {path} must be skipped (hash ∧ parse_ok ∧ symbols)"
            );
        }
        prop_assert_eq!(
            sorted(reconcile.file_deletes.iter().cloned()),
            sorted(expected_deletes.iter().cloned()),
            "Reconcile must delete every File_Cache path absent from the tree"
        );
        for path in admitted.iter().filter(|p| !skipped.contains(*p) && !expected_deletes.contains(*p)) {
            prop_assert!(
                reconcile.file_rows.iter().any(|row| row.path == *path),
                "admitted path {path} with no skippable cache row must be indexed"
            );
        }
        for row in &reconcile.file_rows {
            prop_assert!(row_complete(row), "incomplete File_Cache row: {row:?}");
            if let Some(file) = tree.file(&row.path) {
                prop_assert_eq!(&row.content_hash, &file.content_hash());
                prop_assert_eq!(row.size_bytes, file.bytes().len() as i64);
            }
        }

        let full = planner.plan(PlanRequest::Full).expect("full plan");
        let full_indexed: BTreeSet<String> = full
            .file_rows
            .iter()
            .map(|row| row.path.clone())
            .collect();
        prop_assert_eq!(
            &full_indexed,
            &admitted,
            "Full must reparse every admitted path, ignoring hash equality"
        );
        for row in &full.file_rows {
            prop_assert!(row_complete(row), "incomplete Full File_Cache row: {row:?}");
        }
        prop_assert_eq!(
            sorted(full.file_deletes.iter().cloned()),
            sorted(expected_deletes.iter().cloned())
        );

        let mut writable = testdb.get().clone();
        writable
            .publish(&reconcile)
            .expect("publisher writes File_Cache rows for the batch");
        for row in &reconcile.file_rows {
            let stored = file_table_row(testdb.get(), &row.path);
            prop_assert!(
                stored.is_some(),
                "publisher must write a file row for {}",
                row.path
            );
            let (language, size_bytes, content_hash, parsed_at, parse_status) =
                stored.expect("file row");
            prop_assert!(!language.is_empty());
            prop_assert_eq!(size_bytes, row.size_bytes);
            prop_assert_eq!(&content_hash, &row.content_hash);
            prop_assert!(parsed_at > 0);
            prop_assert!(matches!(
                parse_status.as_str(),
                "ok" | "partial" | "failed"
            ));
        }
        for path in &reconcile.file_deletes {
            prop_assert!(
                file_table_row(testdb.get(), path).is_none(),
                "deleted path {path} must not retain a File_Cache row"
            );
        }
    }
}
