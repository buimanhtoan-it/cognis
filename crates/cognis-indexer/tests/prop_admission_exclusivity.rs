//! Property-based test for admission routing exclusivity (Task 1.3).
//!
//! Feature: non-code-artifact-coverage, Property 1: Artifact admission routing
//! is exclusive and code-preserving
//!
//! Validates: Requirements 1.1, 1.4, 1.5, 1.8
//!
//! ## The property
//!
//! *For any* generated file tree, every file is admitted through **at most
//! one** of the Code_File path or the artifact admission path; and the set of
//! admitted Code_Files (with artifacts enabled) is exactly equal to the set
//! admitted before this feature (artifacts disabled), so no code file is added,
//! dropped, or double-counted.
//!
//! ## How it is driven
//!
//! The test builds a real temp directory tree of code / artifact / unknown
//! files (with mixed-case extensions and, occasionally, deploy/CI descriptor
//! patterns that may collide with code file names), then runs the genuine
//! walker admission logic through the crate's `admitted_rel_paths` accessor
//! twice for identical inputs — once with the artifact gate enabled and once
//! disabled (which reproduces pre-feature Code_File-only admission). The two
//! admitted sets are checked against the exclusivity and code-preservation
//! invariants. The oracle only classifies *code* files (a code extension with
//! the language enabled is unambiguously code no matter what the artifact gate
//! or descriptor patterns do), which is exactly the quantity the property
//! constrains.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cognis_core::Config;
use cognis_indexer::admitted_rel_paths;
use proptest::prelude::*;

/// Extensions in `LANG_BY_EXT` whose language is enabled by default. Mixed case
/// is included to exercise the case-insensitive `to_ascii_lowercase` match in
/// `detect_language`.
const CODE_EXTS: &[&str] = &["py", "go", "rs", "ts", "java", "cs", "PY", "Go", "Rs", "TS"];

/// Extensions in `ARTIFACT_BY_EXT`. Mixed case exercises the case-insensitive
/// match in `detect_artifact`.
const ARTIFACT_EXTS: &[&str] = &[
    "md", "yaml", "yml", "toml", "html", "htm", "sql", "YAML", "Md", "SQL",
];

/// Extensions in neither table: admitted by neither path (absent a descriptor
/// name match).
const UNKNOWN_EXTS: &[&str] = &["xyz", "bin", "dat", "log", "txt", "json"];

/// Non-ignored directory prefixes (none appear in the default `repo.ignore`
/// set), so admission — not directory pruning — is what this property probes.
const DIRS: &[&str] = &["", "a", "a/b", "sub"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    Artifact,
    Unknown,
}

/// One generated file: which directory it lives in, its stem, and the extension
/// (chosen from the pool matching its kind).
#[derive(Debug, Clone)]
struct FileSpec {
    kind: Kind,
    dir_pick: usize,
    stem: String,
    ext_pick: usize,
}

fn file_spec_strategy() -> impl Strategy<Value = FileSpec> {
    (0u8..3, 0usize..DIRS.len(), "[a-z]{1,6}", 0usize..10).prop_map(
        |(k, dir_pick, stem, ext_pick)| FileSpec {
            kind: match k {
                0 => Kind::Code,
                1 => Kind::Artifact,
                _ => Kind::Unknown,
            },
            dir_pick,
            stem,
            ext_pick,
        },
    )
}

/// A fresh, process-and-time-and-counter unique temp repo root so concurrent
/// test binaries and successive proptest cases never collide.
fn unique_repo() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "cognis-admission-excl-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        n
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pick the extension for a spec from the pool matching its kind.
fn ext_for(spec: &FileSpec) -> &'static str {
    match spec.kind {
        Kind::Code => CODE_EXTS[spec.ext_pick % CODE_EXTS.len()],
        Kind::Artifact => ARTIFACT_EXTS[spec.ext_pick % ARTIFACT_EXTS.len()],
        Kind::Unknown => UNKNOWN_EXTS[spec.ext_pick % UNKNOWN_EXTS.len()],
    }
}

proptest! {
    // Minimum 100 iterations per the spec (each case materializes a real temp
    // tree and runs the walker twice), one test for Property 1.
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: non-code-artifact-coverage, Property 1: Artifact admission
    // routing is exclusive and code-preserving
    #[test]
    fn admission_is_exclusive_and_code_preserving(
        specs in prop::collection::vec(file_spec_strategy(), 1..12),
        descriptors in prop::collection::vec("[a-z]{1,3}", 0..3),
    ) {
        let repo = unique_repo();

        // Materialize the tree. Each file is prefixed with its index so no two
        // specs ever collide on the same relative path (writes are unique).
        // `code_expected` is the oracle: a code-extension file whose language is
        // enabled is admitted as code regardless of the artifact gate or any
        // descriptor match, so it is the exact quantity the property pins.
        let mut code_expected: BTreeSet<String> = BTreeSet::new();
        let mut artifact_expected: BTreeSet<String> = BTreeSet::new();

        for (i, spec) in specs.iter().enumerate() {
            let ext = ext_for(spec);
            let dir = DIRS[spec.dir_pick];
            let file_name = format!("f{i}_{}.{ext}", spec.stem);
            let rel = if dir.is_empty() {
                file_name.clone()
            } else {
                format!("{dir}/{file_name}")
            };

            let abs = repo.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&abs, b"x").unwrap();

            match spec.kind {
                Kind::Code => {
                    code_expected.insert(rel);
                }
                Kind::Artifact => {
                    artifact_expected.insert(rel);
                }
                Kind::Unknown => {}
            }
        }

        // Two configs identical except for the artifact gate. Descriptors are
        // supplied to both (they are inert when the gate is closed), so the only
        // difference is `artifact.enabled`.
        let mut cfg_on = Config::default();
        cfg_on.artifact.enabled = true;
        cfg_on.artifact.ci_descriptors = descriptors.clone();

        let mut cfg_off = Config::default();
        cfg_off.artifact.enabled = false;
        cfg_off.artifact.ci_descriptors = descriptors;

        let admitted_on_vec = admitted_rel_paths(&repo, &cfg_on);
        let admitted_off_vec = admitted_rel_paths(&repo, &cfg_off);

        let on: BTreeSet<String> = admitted_on_vec.iter().cloned().collect();
        let off: BTreeSet<String> = admitted_off_vec.iter().cloned().collect();

        // --- Exclusivity (Req 1.1, 1.4): each file admitted through at most one
        // path, so no path appears twice in a single walk. ---
        prop_assert_eq!(
            admitted_on_vec.len(),
            on.len(),
            "artifacts-enabled walk admitted a file more than once (double-counted): {:?}",
            admitted_on_vec
        );
        prop_assert_eq!(
            admitted_off_vec.len(),
            off.len(),
            "artifacts-disabled walk admitted a file more than once: {:?}",
            admitted_off_vec
        );

        // --- Pre-feature admission (Req 1.8): with the artifact gate closed the
        // walker admits exactly the Code_File set and nothing else. ---
        prop_assert_eq!(
            &off,
            &code_expected,
            "artifacts-disabled admission must equal exactly the Code_File set"
        );

        // --- Code preservation (Req 1.5): every code file is admitted with the
        // gate open, and the code-file subset admitted with the gate open is
        // exactly the set admitted with it closed — no code file added or
        // dropped by enabling artifacts. ---
        prop_assert!(
            code_expected.is_subset(&on),
            "enabling artifacts dropped a code file from admission: expected {:?} within {:?}",
            code_expected,
            on
        );
        let on_code: BTreeSet<String> = on.intersection(&code_expected).cloned().collect();
        prop_assert_eq!(
            &on_code,
            &off,
            "code files admitted with artifacts enabled must match those admitted when disabled"
        );

        // --- Additivity / exclusivity (Req 1.1, 1.4): enabling artifacts only
        // *adds* non-code files; every newly admitted path is a non-code
        // artifact, never a duplicate or reclassification of a code file. ---
        prop_assert!(
            off.is_subset(&on),
            "artifacts-enabled admission must be a superset of artifacts-disabled admission"
        );
        for p in on.difference(&off) {
            prop_assert!(
                !code_expected.contains(p),
                "a file newly admitted only when artifacts are enabled must not be a code file: {p}"
            );
        }

        // Every artifact-extension file is admitted when the gate is open
        // (detect_language misses, the artifact arm catches it).
        prop_assert!(
            artifact_expected.is_subset(&on),
            "an artifact-extension file was not admitted with artifacts enabled: expected {:?} within {:?}",
            artifact_expected,
            on
        );

        let _ = std::fs::remove_dir_all(&repo);
    }
}
