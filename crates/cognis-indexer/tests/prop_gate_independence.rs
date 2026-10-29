//! Property test for artifact/code admission gate independence (Task 1.4).
//!
//! Feature: non-code-artifact-coverage, Property 2: Artifact and code admission
//! gates are independent
//!
//! **Validates: Requirements 1.6**
//!
//! For any configuration toggling the artifact gate (`config.artifact.enabled`)
//! and the set of enabled code languages (`config.languages.enabled`), code
//! admission depends only on the code-language gate and artifact admission
//! depends only on the artifact gate: enabling artifacts disables no code
//! language, and disabling a code language disables no artifact.
//!
//! The property is driven through the public [`IndexerPipeline`] walk surface.
//! A temp repo is populated with a mix of code files (`.py`, `.go`) and artifact
//! files (`.md`, `.yaml`); each admitted file yields at least one symbol (code
//! via the language extractors, artifacts via the textual fallback of
//! `parse_source`), so the set of distinct `file_path`s written to the DB is a
//! faithful observation of the admitted set. We then index the *same* tree under
//! four configs — two independent language sets crossed with the artifact gate
//! off/on — and assert the admitted set **factors**: the code slice is invariant
//! under the artifact gate, and the artifact slice is invariant under the code
//! language set.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cognis_core::Config;
use cognis_indexer::IndexerPipeline;
use cognis_store::Database;
use proptest::prelude::*;

/// The four file categories the generator can place in the repo. `Py`/`Go` are
/// Code_Files (gated by `languages.enabled`); `Md`/`Yaml` are Artifact_Files
/// (gated by `artifact.enabled`).
#[derive(Debug, Clone, Copy)]
enum Cat {
    Py,
    Go,
    Md,
    Yaml,
}

impl Cat {
    fn ext(self) -> &'static str {
        match self {
            Cat::Py => "py",
            Cat::Go => "go",
            Cat::Md => "md",
            Cat::Yaml => "yaml",
        }
    }

    /// Non-empty, per-type content that reliably yields at least one symbol.
    fn content(self, i: usize) -> String {
        match self {
            Cat::Py => format!("def func_{i}():\n    return {i}\n"),
            Cat::Go => format!("package main\n\nfunc Func{i}() int {{\n    return {i}\n}}\n"),
            Cat::Md => format!("# Heading {i}\n\nSome documentation body {i}.\n"),
            Cat::Yaml => format!("key_{i}: value_{i}\nother_{i}: {i}\n"),
        }
    }
}

fn cat_strategy() -> impl Strategy<Value = Cat> {
    prop_oneof![Just(Cat::Py), Just(Cat::Go), Just(Cat::Md), Just(Cat::Yaml),]
}

/// A repo layout plus two code-language configurations to cross with the
/// artifact gate. Language sets are subsets of {python, go}.
#[derive(Debug, Clone)]
struct Scenario {
    files: Vec<Cat>,
    langs_a: (bool, bool), // (python_enabled, go_enabled)
    langs_b: (bool, bool),
}

fn scenario_strategy() -> impl Strategy<Value = Scenario> {
    (
        prop::collection::vec(cat_strategy(), 0..8),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(files, pa, ga, pb, gb)| Scenario {
            files,
            langs_a: (pa, ga),
            langs_b: (pb, gb),
        })
}

/// Process-global monotonic counter for unique temp paths, robust to coarse OS
/// clocks that would otherwise collide time-based names.
static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn next_unique() -> u64 {
    UNIQUE.fetch_add(1, Ordering::Relaxed)
}

/// Build a fresh unique temp directory for one proptest case.
fn make_repo(files: &[Cat]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cognis-gate-indep-{}-{}",
        std::process::id(),
        next_unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (i, cat) in files.iter().enumerate() {
        let name = format!("f{i}.{}", cat.ext());
        std::fs::write(dir.join(name), cat.content(i)).unwrap();
    }
    dir
}

/// Turn (python_enabled, go_enabled) into a `languages.enabled` list.
fn langs_list((py, go): (bool, bool)) -> Vec<String> {
    let mut v = Vec::new();
    if py {
        v.push("python".to_string());
    }
    if go {
        v.push("go".to_string());
    }
    v
}

/// Index `repo` with the given gate settings and return the admitted set as the
/// distinct repo-relative `file_path`s persisted to the DB.
///
/// Each call uses a **unique on-disk DB path**. The store caches SQLite
/// connections per-thread keyed by path, and every `":memory:"` open shares one
/// database on a given thread — which would leak symbols across the four
/// configs and across proptest cases. A distinct file path per call guarantees a
/// fresh, isolated database; the connection is closed and the file removed after
/// reading.
fn admitted(
    repo: &std::path::Path,
    langs: (bool, bool),
    artifact_enabled: bool,
) -> BTreeSet<String> {
    let mut cfg = Config::default();
    cfg.languages.enabled = langs_list(langs);
    cfg.artifact.enabled = artifact_enabled;

    let db_path = std::env::temp_dir().join(format!(
        "cognis-gate-indep-db-{}-{}.db",
        std::process::id(),
        next_unique()
    ));

    let db = Database::open(&db_path).expect("open db");
    let mut pipe = IndexerPipeline::new(db.clone(), cfg);
    pipe.index_repo(repo, true).expect("index repo");
    let set: BTreeSet<String> = db
        .list_symbols()
        .expect("list symbols")
        .into_iter()
        .map(|s| s.file_path)
        .collect();

    // Release the connection (Windows holds the file open otherwise) and clean
    // up the DB + WAL sidecar files.
    db.close_thread_connection();
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));

    set
}

fn is_code(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".go")
}

fn is_artifact(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".yaml")
}

fn code_slice(set: &BTreeSet<String>) -> BTreeSet<String> {
    set.iter().filter(|p| is_code(p)).cloned().collect()
}

fn artifact_slice(set: &BTreeSet<String>) -> BTreeSet<String> {
    set.iter().filter(|p| is_artifact(p)).cloned().collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 2: Artifact and code admission gates are independent
    #[test]
    fn gates_factor_independently(scenario in scenario_strategy()) {
        let repo = make_repo(&scenario.files);

        // Same tree, four configs: two language sets × artifact gate off/on.
        let a_off = admitted(&repo, scenario.langs_a, false);
        let a_on = admitted(&repo, scenario.langs_a, true);
        let b_off = admitted(&repo, scenario.langs_b, false);
        let b_on = admitted(&repo, scenario.langs_b, true);

        // Best-effort cleanup; assertion failures still report first.
        let _ = std::fs::remove_dir_all(&repo);

        // Code admission is independent of the artifact gate: toggling
        // `artifact.enabled` never changes which code files are admitted, for
        // either language set.
        prop_assert_eq!(
            code_slice(&a_off),
            code_slice(&a_on),
            "code admission changed when only the artifact gate toggled (langs_a)"
        );
        prop_assert_eq!(
            code_slice(&b_off),
            code_slice(&b_on),
            "code admission changed when only the artifact gate toggled (langs_b)"
        );

        // Artifact admission is independent of the code language set: changing
        // `languages.enabled` never changes which artifact files are admitted,
        // with the artifact gate held fixed (off, then on).
        prop_assert_eq!(
            artifact_slice(&a_off),
            artifact_slice(&b_off),
            "artifact admission changed when only the code language set changed (gate off)"
        );
        prop_assert_eq!(
            artifact_slice(&a_on),
            artifact_slice(&b_on),
            "artifact admission changed when only the code language set changed (gate on)"
        );

        // Sanity anchors that keep the factoring non-vacuous: with the artifact
        // gate off, no artifact file is ever admitted; with it on, every
        // artifact file in the tree is admitted regardless of the language set.
        prop_assert!(
            artifact_slice(&a_off).is_empty(),
            "artifact files admitted despite the artifact gate being off"
        );
        let artifact_count = scenario.files.iter().filter(|c| matches!(c, Cat::Md | Cat::Yaml)).count();
        prop_assert_eq!(
            artifact_slice(&a_on).len(),
            artifact_count,
            "not all artifact files were admitted with the artifact gate on"
        );
    }
}
