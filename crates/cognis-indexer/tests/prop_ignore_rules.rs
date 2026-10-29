// Feature: non-code-artifact-coverage, Property 3: Directory ignore rules apply to artifacts
//
// Property 3 (from design.md):
//   For any generated tree, an artifact file located under an ignored directory
//   (`config.repo.ignore`, `.git`, `.cognis`) is never admitted, identically to
//   the Code_File path.
//
// Validates: Requirements 1.3
//
// The test drives the walker through the public
// `IndexerPipeline::new(...).walk_repo(...)` surface over a materialized temp
// tree, mirroring the inline `walker_skips_ignored_dirs_and_unsupported_exts`
// pattern in `pipeline.rs`. It places both artifact files (.md/.yaml/.sql/.html)
// and code files (.py/.go/.rs/.java) inside ignored directories and in admitted
// locations, then asserts the ignored-directory files are *never* admitted while
// the admitted-location files always are — proving artifacts obey the ignore
// rules identically to the Code_File path.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cognis_core::config::CONFIG_DIR_NAME;
use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;
use proptest::prelude::*;

/// Directory names that are always pruned: the default `config.repo.ignore`
/// set plus the always-ignored `.git` and `.cognis` (CONFIG_DIR_NAME) added by
/// `walk_repo`.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "target",
    "__pycache__",
    ".venv",
    "reference",
    CONFIG_DIR_NAME, // ".cognis"
];

/// Directory names that are *not* ignored, so files beneath them are admitted.
const ADMITTED_DIRS: &[&str] = &["src", "app", "docs", "pkg", "internal", "web"];

/// Artifact extensions admitted by the second admission path (Req 1.1).
const ARTIFACT_EXTS: &[&str] = &["md", "yaml", "sql", "html"];

/// Code extensions admitted by the Code_File path (all enabled by default).
const CODE_EXTS: &[&str] = &["py", "go", "rs", "java"];

/// A single planned file placement within the generated tree.
#[derive(Debug, Clone)]
struct Item {
    /// Whether the file lives under an ignored directory.
    ignored: bool,
    /// Index into `IGNORED_DIRS` (when `ignored`) or, when admitted,
    /// `ADMITTED_DIRS` — with the sentinel `ADMITTED_DIRS.len()` meaning "at the
    /// repo root, no subdirectory".
    dir_idx: usize,
    /// Add one extra nesting level so pruning is exercised deeper than a direct
    /// child (e.g. `node_modules/deep/f.md`, `src/deep/f.py`).
    nested: bool,
    /// The file extension.
    ext: &'static str,
}

fn item_strategy() -> impl Strategy<Value = Item> {
    // (ignored, raw ignored-dir index, raw admitted-dir choice, nested, ext choice)
    (
        any::<bool>(),
        0..IGNORED_DIRS.len(),
        0..=ADMITTED_DIRS.len(), // inclusive upper bound = root sentinel
        any::<bool>(),
        prop_oneof![
            (0..ARTIFACT_EXTS.len()).prop_map(|i| ARTIFACT_EXTS[i]),
            (0..CODE_EXTS.len()).prop_map(|i| CODE_EXTS[i]),
        ],
    )
        .prop_map(|(ignored, ig_idx, adm_idx, nested, ext)| Item {
            ignored,
            dir_idx: if ignored { ig_idx } else { adm_idx },
            nested,
            ext,
        })
}

fn unique_repo_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "cognis-prop-ignore-{}-{}-{}",
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// The relative directory path (from repo root) an item is materialized under,
/// or `None` for a root-level admitted file.
fn item_dir(item: &Item) -> Option<PathBuf> {
    let base = if item.ignored {
        Some(PathBuf::from(IGNORED_DIRS[item.dir_idx]))
    } else if item.dir_idx == ADMITTED_DIRS.len() {
        None
    } else {
        Some(PathBuf::from(ADMITTED_DIRS[item.dir_idx]))
    };
    match (base, item.nested) {
        (Some(d), true) => Some(d.join("deep")),
        (other, _) => other,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn artifacts_obey_directory_ignore_rules(items in prop::collection::vec(item_strategy(), 1..12)) {
        let repo = unique_repo_dir();
        std::fs::create_dir_all(&repo).unwrap();

        // Materialize the tree. File names are made globally unique via the
        // enumeration index so no two placements collide, and so we can assert
        // an exact admitted set.
        let mut expected_admitted: BTreeSet<PathBuf> = BTreeSet::new();
        for (idx, item) in items.iter().enumerate() {
            let file_name = format!("f{idx}.{}", item.ext);
            let rel = match item_dir(item) {
                Some(d) => d.join(&file_name),
                None => PathBuf::from(&file_name),
            };
            let abs = repo.join(&rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&abs, b"x = 1\n").unwrap();
            if !item.ignored {
                // Both artifact and code files in admitted locations are admitted
                // (artifacts enabled by default), so they belong in the expected set.
                expected_admitted.insert(rel);
            }
        }

        // Canonicalize the root so stripping the prefix off walked absolute
        // paths yields clean relative paths (walk_repo joins children onto the
        // exact root it is handed).
        let canonical_root = repo.canonicalize().unwrap_or_else(|_| repo.clone());
        let pipe = IndexerPipeline::new(
            Database::open(":memory:").expect("open in-memory db"),
            Config::default(),
        );
        let walked = pipe.walk_repo(&canonical_root);

        let ignored_set: BTreeSet<&str> = IGNORED_DIRS.iter().copied().collect();
        let mut walked_rel: BTreeSet<PathBuf> = BTreeSet::new();
        for abs in &walked {
            let rel = abs
                .strip_prefix(&canonical_root)
                .expect("walked path is under the repo root")
                .to_path_buf();

            // Core invariant (Req 1.3): no admitted path may pass through an
            // ignored directory component — this holds for artifacts and code
            // files alike, so the ignore rule is applied identically.
            for comp in rel.components() {
                let comp = comp.as_os_str().to_string_lossy();
                prop_assert!(
                    !ignored_set.contains(comp.as_ref()),
                    "admitted path {:?} traverses ignored directory {:?}",
                    rel,
                    comp
                );
            }
            walked_rel.insert(rel);
        }

        // The admitted set is exactly the files placed in admitted locations:
        // every ignored-directory file (artifact or code) was pruned, and no
        // admitted file was dropped for an unrelated reason.
        prop_assert_eq!(
            &walked_rel,
            &expected_admitted,
            "walked set must equal the admitted-location placements"
        );

        std::fs::remove_dir_all(&repo).ok();
    }
}
