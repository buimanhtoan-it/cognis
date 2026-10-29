//! `arb_transition_sequence` — sequences over the eleven `Transition_Matrix`
//! transitions.
//!
//! Requirements glossary: "**Transition_Matrix**: initial index, unchanged
//! control, caller edit, callee edit, delete, re-add, rename, caller branch
//! replacement, valid-to-empty, valid-to-invalid-UTF-8, and full-rescan missed
//! delete." Feeds properties P5, P6, P7.
//!
//! A transition is applied to a [`SourceTree`] with [`Transition::apply`], which
//! returns the new tree **and** the set of paths the incremental indexer is told
//! about. Those two outputs differ for exactly one variant:
//! [`Transition::FullRescanMissedDelete`] removes a file from the tree while
//! reporting *no* touched paths, which is the whole point of Requirement 12.6 —
//! the incremental path misses the delete and the subsequent full index must
//! still converge on the clean-rebuild digest.

use proptest::prelude::*;

use crate::source_tree::{FileBody, FunctionSpec, SourceFileSpec, SourceTree};

/// One transition from the `Transition_Matrix`.
///
/// Path/ordinal fields are stored as ordinals rather than paths so a generated
/// sequence stays applicable to whatever tree it is paired with, and so
/// `proptest` shrinking produces small ordinals rather than unrelated strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// 1. Index the tree as it stands. Always valid, changes nothing.
    InitialIndex,
    /// 2. The control: no source change at all.
    UnchangedControl,
    /// 3. Edit a *caller* — change the body of a function that calls something.
    CallerEdit { file: usize },
    /// 4. Edit a *callee* — change a function that others call.
    CalleeEdit { file: usize },
    /// 5. Remove a file, and report it.
    Delete { file: usize },
    /// 6. Re-add a previously deleted file with byte-identical content.
    ReAdd { file: usize },
    /// 7. Move a file to a new path, same content.
    Rename { file: usize },
    /// 8. Replace a caller's call list wholesale (the "caller branch
    ///    replacement" case: its outbound edges change completely).
    CallerBranchReplacement { file: usize, target: usize },
    /// 9. Truncate a file to zero bytes.
    ValidToEmpty { file: usize },
    /// 10. Replace a file's content with bytes that are not valid UTF-8.
    ValidToInvalidUtf8 { file: usize },
    /// 11. Remove a file **without reporting it** — the missed delete a later
    ///     full rescan has to repair (Requirement 12.6).
    FullRescanMissedDelete { file: usize },
}

/// What applying a transition did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    /// The tree after the transition.
    pub tree: SourceTree,
    /// Paths the incremental indexer is told about. Empty for
    /// [`Transition::FullRescanMissedDelete`] and
    /// [`Transition::UnchangedControl`].
    pub touched: Vec<String>,
    /// Whether this transition deliberately withheld a change from the
    /// incremental path.
    pub missed: bool,
}

impl Transition {
    /// Stable label, for test failure messages and for asserting matrix
    /// coverage.
    pub fn label(&self) -> &'static str {
        match self {
            Transition::InitialIndex => "initial_index",
            Transition::UnchangedControl => "unchanged_control",
            Transition::CallerEdit { .. } => "caller_edit",
            Transition::CalleeEdit { .. } => "callee_edit",
            Transition::Delete { .. } => "delete",
            Transition::ReAdd { .. } => "re_add",
            Transition::Rename { .. } => "rename",
            Transition::CallerBranchReplacement { .. } => "caller_branch_replacement",
            Transition::ValidToEmpty { .. } => "valid_to_empty",
            Transition::ValidToInvalidUtf8 { .. } => "valid_to_invalid_utf8",
            Transition::FullRescanMissedDelete { .. } => "full_rescan_missed_delete",
        }
    }

    /// Every label in the matrix, in requirements order. A property asserting
    /// matrix coverage compares against this.
    pub const LABELS: [&'static str; 11] = [
        "initial_index",
        "unchanged_control",
        "caller_edit",
        "callee_edit",
        "delete",
        "re_add",
        "rename",
        "caller_branch_replacement",
        "valid_to_empty",
        "valid_to_invalid_utf8",
        "full_rescan_missed_delete",
    ];

    /// Apply this transition to `tree`.
    ///
    /// Transitions that address a file by ordinal wrap the ordinal modulo the
    /// file count, so any generated sequence applies to any generated tree. A
    /// transition with nothing to act on (an empty tree, a re-add with no
    /// deletion history) degenerates to the unchanged control rather than
    /// failing — the sequence stays valid, which is what keeps shrinking useful.
    pub fn apply(&self, tree: &SourceTree, deleted: &[SourceFileSpec]) -> TransitionOutcome {
        let mut next = tree.clone();
        match self {
            Transition::InitialIndex | Transition::UnchangedControl => TransitionOutcome {
                tree: next,
                touched: Vec::new(),
                missed: false,
            },

            Transition::CallerEdit { file } | Transition::CalleeEdit { file } => {
                match pick(&next, *file) {
                    None => unchanged(next),
                    Some(index) => {
                        let path = next.files[index].rel_path.clone();
                        edit_body(&mut next.files[index], self.label());
                        TransitionOutcome {
                            tree: next,
                            touched: vec![path],
                            missed: false,
                        }
                    }
                }
            }

            Transition::Delete { file } => match pick(&next, *file) {
                None => unchanged(next),
                Some(index) => {
                    let removed = next.files.remove(index);
                    TransitionOutcome {
                        tree: next,
                        touched: vec![removed.rel_path],
                        missed: false,
                    }
                }
            },

            Transition::ReAdd { file } => {
                if deleted.is_empty() {
                    return unchanged(next);
                }
                // Byte-identical re-add: the same spec that was removed.
                let restored = deleted[*file % deleted.len()].clone();
                if next.file(&restored.rel_path).is_some() {
                    return unchanged(next);
                }
                let path = restored.rel_path.clone();
                next.files.push(restored);
                TransitionOutcome {
                    tree: next,
                    touched: vec![path],
                    missed: false,
                }
            }

            Transition::Rename { file } => match pick(&next, *file) {
                None => unchanged(next),
                Some(index) => {
                    let old = next.files[index].rel_path.clone();
                    let renamed = rename_path(&old);
                    if next.file(&renamed).is_some() {
                        return unchanged(next);
                    }
                    next.files[index].rel_path = renamed.clone();
                    TransitionOutcome {
                        tree: next,
                        touched: vec![old, renamed],
                        missed: false,
                    }
                }
            },

            Transition::CallerBranchReplacement { file, target } => {
                match pick(&next, *file) {
                    None => unchanged(next),
                    Some(index) => {
                        // Choose a call target from another file's declarations,
                        // so the replaced branch genuinely re-points the
                        // cross-file call graph.
                        let targets: Vec<String> = next
                            .files
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i != index)
                            .flat_map(|(_, f)| {
                                f.declared_names()
                                    .into_iter()
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .collect();
                        let path = next.files[index].rel_path.clone();
                        if let FileBody::Code(functions) = &mut next.files[index].body {
                            if functions.is_empty() {
                                return unchanged(next);
                            }
                            let replacement = if targets.is_empty() {
                                Vec::new()
                            } else {
                                vec![targets[*target % targets.len()].clone()]
                            };
                            for function in functions.iter_mut() {
                                function.calls = replacement.clone();
                            }
                        } else {
                            return unchanged(next);
                        }
                        TransitionOutcome {
                            tree: next,
                            touched: vec![path],
                            missed: false,
                        }
                    }
                }
            }

            Transition::ValidToEmpty { file } => match pick(&next, *file) {
                None => unchanged(next),
                Some(index) => {
                    let path = next.files[index].rel_path.clone();
                    next.files[index].body = FileBody::Empty;
                    TransitionOutcome {
                        tree: next,
                        touched: vec![path],
                        missed: false,
                    }
                }
            },

            Transition::ValidToInvalidUtf8 { file } => match pick(&next, *file) {
                None => unchanged(next),
                Some(index) => {
                    let path = next.files[index].rel_path.clone();
                    next.files[index].body = FileBody::InvalidUtf8(vec![0xFF, 0xFE, 0x00, 0xFF]);
                    TransitionOutcome {
                        tree: next,
                        touched: vec![path],
                        missed: false,
                    }
                }
            },

            Transition::FullRescanMissedDelete { file } => match pick(&next, *file) {
                None => unchanged(next),
                Some(index) => {
                    next.files.remove(index);
                    // Deliberately report nothing: the incremental path never
                    // learns about this deletion (Requirement 12.6).
                    TransitionOutcome {
                        tree: next,
                        touched: Vec::new(),
                        missed: true,
                    }
                }
            },
        }
    }
}

/// Apply a whole sequence, threading the deleted-file history so `ReAdd` has
/// something byte-identical to restore.
///
/// Returns the final tree, the per-step touched paths, and whether any step
/// withheld a change from the incremental path (so a test knows a full rescan
/// is required for convergence).
pub fn apply_sequence(
    tree: &SourceTree,
    transitions: &[Transition],
) -> (SourceTree, Vec<Vec<String>>, bool) {
    let mut current = tree.clone();
    let mut deleted: Vec<SourceFileSpec> = Vec::new();
    let mut touched_per_step = Vec::new();
    let mut any_missed = false;

    for transition in transitions {
        let before = current.clone();
        let outcome = transition.apply(&current, &deleted);
        // Anything present before and absent after joins the deletion history.
        for file in &before.files {
            if outcome.tree.file(&file.rel_path).is_none() {
                deleted.push(file.clone());
            }
        }
        any_missed |= outcome.missed;
        touched_per_step.push(outcome.touched);
        current = outcome.tree;
    }

    (current, touched_per_step, any_missed)
}

fn unchanged(tree: SourceTree) -> TransitionOutcome {
    TransitionOutcome {
        tree,
        touched: Vec::new(),
        missed: false,
    }
}

/// Resolve a file ordinal against the tree, `None` when the tree is empty.
fn pick(tree: &SourceTree, ordinal: usize) -> Option<usize> {
    if tree.files.is_empty() {
        None
    } else {
        Some(ordinal % tree.files.len())
    }
}

/// Mutate a file's body so its bytes — and therefore its `Content_Hash` —
/// change, while keeping it parseable.
fn edit_body(file: &mut SourceFileSpec, marker: &str) {
    match &mut file.body {
        FileBody::Code(functions) => {
            if let Some(first) = functions.first_mut() {
                first.calls.push(format!("edited_{marker}"));
            } else {
                functions.push(FunctionSpec {
                    name: format!("edited_{marker}"),
                    calls: Vec::new(),
                });
            }
        }
        // A degenerate body becomes parseable again: an edit to an empty or
        // unreadable file is still a real transition.
        body => {
            *body = FileBody::Code(vec![FunctionSpec {
                name: format!("edited_{marker}"),
                calls: Vec::new(),
            }]);
        }
    }
}

fn rename_path(old: &str) -> String {
    match old.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}_renamed.{ext}"),
        None => format!("{old}_renamed"),
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// A single transition, uniformly over all eleven matrix entries.
pub fn arb_transition() -> impl Strategy<Value = Transition> {
    prop_oneof![
        Just(Transition::InitialIndex),
        Just(Transition::UnchangedControl),
        (0usize..8).prop_map(|file| Transition::CallerEdit { file }),
        (0usize..8).prop_map(|file| Transition::CalleeEdit { file }),
        (0usize..8).prop_map(|file| Transition::Delete { file }),
        (0usize..8).prop_map(|file| Transition::ReAdd { file }),
        (0usize..8).prop_map(|file| Transition::Rename { file }),
        (0usize..8, 0usize..8)
            .prop_map(|(file, target)| Transition::CallerBranchReplacement { file, target }),
        (0usize..8).prop_map(|file| Transition::ValidToEmpty { file }),
        (0usize..8).prop_map(|file| Transition::ValidToInvalidUtf8 { file }),
        (0usize..8).prop_map(|file| Transition::FullRescanMissedDelete { file }),
    ]
}

/// Sequences of 1..=8 transitions drawn from the matrix.
///
/// `prop_oneof!` weights every variant equally, so the missed delete appears in
/// a meaningful fraction of sequences without a test having to force it. When a
/// property needs *guaranteed* coverage of all eleven, use
/// [`arb_transition_sequence_covering_all`].
pub fn arb_transition_sequence() -> impl Strategy<Value = Vec<Transition>> {
    prop::collection::vec(arb_transition(), 1..9)
}

/// A permutation of all eleven transitions, so one case exercises the entire
/// matrix — the shape Requirement 24.1 ("FOR every transition in the
/// Transition_Matrix") asks for.
pub fn arb_transition_sequence_covering_all() -> impl Strategy<Value = Vec<Transition>> {
    (
        prop::collection::vec(0usize..8, 11),
        prop::collection::vec(0usize..8, 11),
        Just(()),
    )
        .prop_flat_map(|(files, targets, ())| {
            let all = vec![
                Transition::InitialIndex,
                Transition::UnchangedControl,
                Transition::CallerEdit { file: files[2] },
                Transition::CalleeEdit { file: files[3] },
                Transition::Delete { file: files[4] },
                Transition::ReAdd { file: files[5] },
                Transition::Rename { file: files[6] },
                Transition::CallerBranchReplacement {
                    file: files[7],
                    target: targets[7],
                },
                Transition::ValidToEmpty { file: files[8] },
                Transition::ValidToInvalidUtf8 { file: files[9] },
                Transition::FullRescanMissedDelete { file: files[10] },
            ];
            Just(all).prop_shuffle()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_tree::{arb_source_tree, SourceFileSpec};
    use std::collections::BTreeSet;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Every transition applies to every tree without panicking, and each
        /// step's touched paths belong to that step's before- or after-tree.
        ///
        /// The check is necessarily *per step*, not against the initial and
        /// final trees: a rename followed by a delete legitimately touches an
        /// intermediate path that neither endpoint tree contains.
        #[test]
        fn transitions_apply_totally(
            tree in arb_source_tree(),
            transitions in arb_transition_sequence(),
        ) {
            let mut current = tree;
            let mut deleted: Vec<SourceFileSpec> = Vec::new();

            for transition in &transitions {
                let before: BTreeSet<String> = current.paths().into_iter().collect();
                let outcome = transition.apply(&current, &deleted);
                let after: BTreeSet<String> = outcome.tree.paths().into_iter().collect();

                for step in &outcome.touched {
                    prop_assert!(
                        before.contains(step) || after.contains(step),
                        "{} touched {step}, which is in neither its before nor its after tree",
                        transition.label(),
                    );
                }

                for file in &current.files {
                    if outcome.tree.file(&file.rel_path).is_none() {
                        deleted.push(file.clone());
                    }
                }
                current = outcome.tree;
            }
        }

        /// `apply_sequence` reports one touched-path list per transition, and
        /// only a missed-delete transition can raise the `missed` flag — the
        /// flag is what tells a property that a full rescan is required for
        /// convergence.
        #[test]
        fn apply_sequence_accounts_for_every_step(
            tree in arb_source_tree(),
            transitions in arb_transition_sequence(),
        ) {
            let (_final_tree, touched, missed) = apply_sequence(&tree, &transitions);
            prop_assert_eq!(touched.len(), transitions.len());
            if missed {
                prop_assert!(
                    transitions
                        .iter()
                        .any(|t| matches!(t, Transition::FullRescanMissedDelete { .. })),
                    "missed flag raised without a missed-delete transition",
                );
            }
        }

        /// The missed-delete transition removes a file while reporting nothing:
        /// exactly the Requirement 12.6 trap.
        #[test]
        fn missed_delete_withholds_its_path(tree in arb_source_tree(), file in 0usize..8) {
            let transition = Transition::FullRescanMissedDelete { file };
            let outcome = transition.apply(&tree, &[]);
            prop_assert!(outcome.touched.is_empty());
            prop_assert!(outcome.missed);
            prop_assert_eq!(outcome.tree.files.len(), tree.files.len() - 1);
        }

        /// The covering generator really does cover all eleven labels.
        #[test]
        fn covering_sequence_hits_every_label(
            transitions in arb_transition_sequence_covering_all(),
        ) {
            let labels: BTreeSet<&str> = transitions.iter().map(Transition::label).collect();
            prop_assert_eq!(labels.len(), Transition::LABELS.len());
            for label in Transition::LABELS {
                prop_assert!(labels.contains(label), "missing {label}");
            }
        }
    }
}
