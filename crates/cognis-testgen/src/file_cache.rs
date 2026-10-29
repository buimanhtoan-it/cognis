//! `arb_file_cache_state` — `File_Cache` states the `Change_Planner` must
//! handle.
//!
//! Design: "caches that are current, stale, ahead of the tree, hash-matching
//! with absent symbols, and with failed parse statuses". Feeds P10.
//!
//! The interesting entry is [`FileCacheKind::HashMatchesSymbolsAbsent`]. The
//! planner's skip rule (Requirement 8.2, 8.8) is a **three**-part conjunction —
//! hash equal *and* parse status successful *and* symbols present — and a
//! generator that only produced matching/non-matching hashes would never catch
//! a two-part implementation. This generator produces the state where the hash
//! matches but the derived rows are gone, so the planner must re-index anyway
//! ("the 'symbols absent' clause is what makes a hash-matching but empty
//! database self-heal").
//!
//! Generation is *relative to a [`SourceTree`]*, so the resulting cache is a
//! coherent story about that tree rather than free-floating rows.

use proptest::prelude::*;

use cognis_core::ParseStatus;

use crate::source_tree::{arb_source_tree, SourceTree};

/// What kind of state one cache entry is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCacheKind {
    /// Hash matches the tree, parse succeeded, symbols present. The planner
    /// must skip this path.
    Current,
    /// Hash differs from the tree's current bytes. Must be re-indexed.
    Stale,
    /// A `file` row whose path is absent from the admitted tree. `Reconcile`
    /// must emit deletes for it (Requirement 8.4) — this is also the missed
    /// delete repair path (Requirement 12.6).
    AheadOfTree,
    /// Hash matches, parse succeeded, but the file's symbol rows are absent.
    /// Must be re-indexed despite the matching hash (Requirement 8.8).
    HashMatchesSymbolsAbsent,
    /// Hash matches but the recorded parse status is a failure. Must be
    /// re-attempted (Requirement 8.2, 8.7).
    ParseFailed,
    /// No cache row at all for a path present in the tree. Must be indexed
    /// (Requirement 8.5).
    Absent,
}

/// One generated cache entry.
///
/// [`FileCacheKind::Absent`] entries carry `row: None`; every other kind carries
/// a row. Keeping the kind alongside the row is what lets a property assert the
/// planner's decision *per entry* instead of only over the batch as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheEntry {
    pub path: String,
    pub kind: FileCacheKind,
    /// `(content_hash, parse_status, symbols_present)` as the `file` row records
    /// them. `None` for [`FileCacheKind::Absent`].
    pub row: Option<FileCacheRowState>,
}

/// The three fields the planner's skip rule reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheRowState {
    pub content_hash: String,
    pub parse_status: ParseStatus,
    /// Whether the database actually holds symbol rows for this path (or the
    /// path was legitimately recorded as symbol-less).
    pub symbols_present: bool,
}

impl FileCacheEntry {
    /// Whether the planner is required to include this path in the batch.
    ///
    /// This is the design's rule stated once, so a property can compare the
    /// planner's decision against it rather than re-deriving it per test.
    pub fn must_be_reindexed(&self) -> bool {
        match self.kind {
            FileCacheKind::Current => false,
            FileCacheKind::Stale
            | FileCacheKind::AheadOfTree
            | FileCacheKind::HashMatchesSymbolsAbsent
            | FileCacheKind::ParseFailed
            | FileCacheKind::Absent => true,
        }
    }

    /// Whether this entry must produce *deletes* rather than a re-index.
    pub fn must_be_deleted(&self) -> bool {
        self.kind == FileCacheKind::AheadOfTree
    }
}

/// A whole generated cache, paired with the tree it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCacheState {
    pub tree: SourceTree,
    pub entries: Vec<FileCacheEntry>,
}

impl FileCacheState {
    /// Paths the planner must include in its batch.
    pub fn expected_batch_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.must_be_reindexed())
            .map(|e| e.path.clone())
            .collect();
        paths.sort();
        paths
    }

    /// Paths the planner must emit deletes for under `Reconcile`.
    pub fn expected_delete_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.must_be_deleted())
            .map(|e| e.path.clone())
            .collect();
        paths.sort();
        paths
    }

    /// Paths the planner must skip.
    pub fn expected_skipped_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .entries
            .iter()
            .filter(|e| !e.must_be_reindexed())
            .map(|e| e.path.clone())
            .collect();
        paths.sort();
        paths
    }

    /// The entry for a path, if any.
    pub fn entry(&self, path: &str) -> Option<&FileCacheEntry> {
        self.entries.iter().find(|e| e.path == path)
    }
}

/// `arb_file_cache_state` — a cache over a freshly generated tree.
pub fn arb_file_cache_state() -> impl Strategy<Value = FileCacheState> {
    arb_source_tree().prop_flat_map(arb_file_cache_state_over)
}

/// `arb_file_cache_state` over a caller-supplied tree, for properties that need
/// the tree and the cache to be generated separately (P10 pairs it with
/// `arb_source_tree`).
pub fn arb_file_cache_state_over(tree: SourceTree) -> impl Strategy<Value = FileCacheState> {
    let paths = tree.paths();
    let in_tree_count = paths.len();
    (
        // One kind per in-tree path.
        prop::collection::vec(arb_in_tree_kind(), in_tree_count),
        // 0..=2 ahead-of-tree rows: paths the cache remembers that the tree no
        // longer has.
        prop::collection::vec(0usize..8, 0..3),
    )
        .prop_map(move |(kinds, ghosts)| {
            let mut entries: Vec<FileCacheEntry> = Vec::new();

            for (path, kind) in paths.iter().zip(kinds.iter().copied()) {
                let real_hash = tree
                    .file(path)
                    .map(|f| f.content_hash())
                    .unwrap_or_default();
                let row = match kind {
                    FileCacheKind::Absent => None,
                    FileCacheKind::Current => Some(FileCacheRowState {
                        content_hash: real_hash,
                        parse_status: ParseStatus::Ok,
                        symbols_present: true,
                    }),
                    FileCacheKind::Stale => Some(FileCacheRowState {
                        // Deliberately not the tree's hash.
                        content_hash: format!("stale{}", &real_hash[..real_hash.len().min(58)]),
                        parse_status: ParseStatus::Ok,
                        symbols_present: true,
                    }),
                    FileCacheKind::HashMatchesSymbolsAbsent => Some(FileCacheRowState {
                        content_hash: real_hash,
                        parse_status: ParseStatus::Ok,
                        // The trap: hash agrees, derived rows are gone.
                        symbols_present: false,
                    }),
                    FileCacheKind::ParseFailed => Some(FileCacheRowState {
                        content_hash: real_hash,
                        parse_status: ParseStatus::Failed,
                        symbols_present: false,
                    }),
                    FileCacheKind::AheadOfTree => unreachable!("not an in-tree kind"),
                };
                entries.push(FileCacheEntry {
                    path: path.clone(),
                    kind,
                    row,
                });
            }

            for ghost in &ghosts {
                let path = format!("src/removed_{ghost}.py");
                if entries.iter().any(|e| e.path == path) {
                    continue;
                }
                entries.push(FileCacheEntry {
                    path,
                    kind: FileCacheKind::AheadOfTree,
                    row: Some(FileCacheRowState {
                        content_hash: format!("{ghost:064x}"),
                        parse_status: ParseStatus::Ok,
                        symbols_present: true,
                    }),
                });
            }

            entries.sort_by(|a, b| a.path.cmp(&b.path));
            FileCacheState {
                tree: tree.clone(),
                entries,
            }
        })
}

/// Kinds that describe a path the tree still has.
///
/// `Current` is weighted heaviest because a realistic cache is mostly current —
/// that is what makes the incremental path incremental, and a property that
/// never skips anything would not exercise the skip rule at all.
fn arb_in_tree_kind() -> impl Strategy<Value = FileCacheKind> {
    prop_oneof![
        4 => Just(FileCacheKind::Current),
        2 => Just(FileCacheKind::Stale),
        2 => Just(FileCacheKind::HashMatchesSymbolsAbsent),
        1 => Just(FileCacheKind::ParseFailed),
        2 => Just(FileCacheKind::Absent),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Every in-tree path gets exactly one entry, and every ahead-of-tree
        /// entry names a path the tree does not have.
        #[test]
        fn cache_covers_the_tree(state in arb_file_cache_state()) {
            let tree_paths: BTreeSet<String> = state.tree.paths().into_iter().collect();
            for path in &tree_paths {
                let entry = state.entry(path);
                prop_assert!(entry.is_some(), "no cache entry for {path}");
                prop_assert_ne!(entry.unwrap().kind, FileCacheKind::AheadOfTree);
            }
            for entry in &state.entries {
                if entry.kind == FileCacheKind::AheadOfTree {
                    prop_assert!(!tree_paths.contains(&entry.path));
                }
            }
        }

        /// Only a fully current entry may be skipped: the skip rule really is a
        /// three-part conjunction.
        #[test]
        fn only_current_entries_are_skippable(state in arb_file_cache_state()) {
            for entry in &state.entries {
                if !entry.must_be_reindexed() {
                    let row = entry.row.as_ref().expect("current entries have a row");
                    let real = state
                        .tree
                        .file(&entry.path)
                        .expect("current entries are in the tree")
                        .content_hash();
                    prop_assert_eq!(&row.content_hash, &real);
                    prop_assert_eq!(row.parse_status, ParseStatus::Ok);
                    prop_assert!(row.symbols_present);
                }
            }
        }

        /// The hash-matching-symbols-absent trap really does match on hash — a
        /// two-part skip rule would wrongly skip it.
        #[test]
        fn symbols_absent_trap_matches_on_hash(state in arb_file_cache_state()) {
            for entry in &state.entries {
                if entry.kind == FileCacheKind::HashMatchesSymbolsAbsent {
                    let row = entry.row.as_ref().unwrap();
                    let real = state.tree.file(&entry.path).unwrap().content_hash();
                    prop_assert_eq!(&row.content_hash, &real);
                    prop_assert!(!row.symbols_present);
                    prop_assert!(entry.must_be_reindexed());
                }
            }
        }

        /// The three expected-path partitions are disjoint and total.
        #[test]
        fn partitions_are_disjoint_and_total(state in arb_file_cache_state()) {
            let batch: BTreeSet<String> = state.expected_batch_paths().into_iter().collect();
            let skipped: BTreeSet<String> = state.expected_skipped_paths().into_iter().collect();
            prop_assert!(batch.is_disjoint(&skipped));
            prop_assert_eq!(batch.len() + skipped.len(), state.entries.len());
            for path in state.expected_delete_paths() {
                prop_assert!(batch.contains(&path));
            }
        }
    }
}
