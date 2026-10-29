//! Property 21: Owner uniqueness and canonicalization.
//!
//! At most one process holds the owner lease; losers attach without opening the
//! database; releasing the holder frees the lease; any two spellings of the same
//! directory resolve to one identity while distinct directories stay distinct.
//!
//! **Validates: Requirements 16.1, 16.3, 16.4, 16.5**

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::thread;

use cognis_core::lease::{new_owner_record, write_lease_atomic, DEFAULT_LEASE_TTL};
use cognis_core::{RepoIdentity, SemanticWarmPolicy};
use cognis_owner::{become_owner_with_policy, owner_lease_path, OwnerSession};
use cognis_testgen::{arb_path_spelling, arb_path_spelling_pair, spell, PathSpelling};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

fn lasting_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cognis-p21-{tag}-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn identity_base() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| lasting_dir("same")).clone()
}

fn distinct_base() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| lasting_dir("other")).clone()
}

fn unique_repo() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cognis-p21-lease-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("create unique workspace");
    dir
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn usable_path(spelling: &PathSpelling) -> Option<PathBuf> {
    if !spelling.kind.is_local() || !spelling.is_materializable() {
        return None;
    }
    match spelling.materialize() {
        Ok(path) if path.exists() => Some(path),
        _ => None,
    }
}

fn identity_for(path: &Path) -> RepoIdentity {
    RepoIdentity::from_paths(path, RepoIdentity::default_db_path(path))
}

fn install_foreign_lease(repo: &Path) -> PathBuf {
    let identity = identity_for(repo);
    let canonical = PathBuf::from(&identity.root);
    let cognis = canonical.join(".cognis");
    fs::create_dir_all(&cognis).expect("create .cognis");
    // Short TTL / debounce so LeaseGuard and FileWatcher Drop join in ~100ms
    // instead of sleeping the production 10s heartbeat / 250ms debounce.
    fs::write(
        cognis.join("config.yaml"),
        "owner:\n  lease_ttl_s: 1\n  watcher_debounce_ms: 1\n",
    )
    .expect("write owner test config");
    let mut foreign = new_owner_record(DEFAULT_LEASE_TTL);
    foreign.pid = 1;
    foreign.process_start_id = "foreign-p21-owner".into();
    let path = owner_lease_path(&canonical);
    write_lease_atomic(&path, &foreign).expect("write foreign owner lease");
    path
}

enum WorkerOutcome {
    Attached { pid: u32 },
    Acquired,
}

fn try_own(path: &Path) -> cognis_core::Result<OwnerSession> {
    become_owner_with_policy(path, SemanticWarmPolicy::Lazy)
}

fn worker_become(path: PathBuf) -> std::result::Result<WorkerOutcome, String> {
    match try_own(&path) {
        Ok(OwnerSession::Attached(session)) => Ok(WorkerOutcome::Attached {
            pid: session.endpoint().pid,
        }),
        Ok(OwnerSession::Acquired(owned)) => {
            let _ = owned.release();
            Ok(WorkerOutcome::Acquired)
        }
        Err(err) => Err(err.to_string()),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 21: Owner uniqueness and canonicalization.
    /// **Validates: Requirements 16.1, 16.3, 16.4, 16.5**
    #[test]
    fn owner_uniqueness_and_path_canonicalization(
        (left, right) in arb_path_spelling_pair(identity_base()),
        distinct in arb_path_spelling(distinct_base()),
        workers in 2usize..5,
    ) {
        if let (Some(left_path), Some(right_path)) = (usable_path(&left), usable_path(&right)) {
            let left_id = identity_for(&left_path);
            let right_id = identity_for(&right_path);
            prop_assert_eq!(
                left_id.wire_key(),
                right_id.wire_key(),
                "spellings of one directory must share a Canonical_Workspace ({:?} vs {:?})",
                left.kind,
                right.kind
            );
            prop_assert_eq!(
                owner_lease_path(Path::new(&left_id.root)),
                owner_lease_path(Path::new(&right_id.root))
            );
        }

        if let (Some(same_path), Some(other_path)) =
            (usable_path(&left), usable_path(&distinct))
        {
            if left.base != distinct.base {
                prop_assert_ne!(
                    identity_for(&same_path).wire_key(),
                    identity_for(&other_path).wire_key(),
                    "distinct directories must keep distinct identities"
                );
            }
        }

        let repo = unique_repo();
        let _cleanup = Cleanup(repo.clone());
        let candidate = spell(&repo, left.kind);
        let Some(spelled) = usable_path(&candidate) else {
            return Ok(());
        };
        prop_assert_eq!(
            identity_for(&spelled).wire_key(),
            identity_for(&repo).wire_key(),
            "generated spelling must resolve to the unique workspace"
        );

        let lease = install_foreign_lease(&repo);
        let db_path = identity_for(&repo).db_path;
        let mut attached = 0usize;
        let mut opened = 0usize;
        let mut handles = Vec::new();
        for _ in 0..workers {
            let spelled = spelled.clone();
            handles.push(thread::spawn(move || worker_become(spelled)));
        }
        for handle in handles {
            match handle.join().expect("worker").expect("become_owner") {
                WorkerOutcome::Attached { pid } => {
                    attached += 1;
                    prop_assert_eq!(pid, 1);
                }
                WorkerOutcome::Acquired => opened += 1,
            }
        }
        prop_assert_eq!(opened, 0, "losers must attach without opening the database");
        prop_assert_eq!(attached, workers);
        prop_assert!(
            !Path::new(&db_path).exists(),
            "attach must not create the workspace database"
        );

        fs::remove_file(&lease).expect("remove foreign lease");
        let acquired = match try_own(&spelled).expect("acquire after holder gone") {
            OwnerSession::Acquired(owned) => owned,
            OwnerSession::Attached(_) => {
                return Err(TestCaseError::fail(
                    "termination of the holder must free the lease for a later acquire",
                ));
            }
        };
        prop_assert!(
            acquired.database().is_some(),
            "the new holder opens the database"
        );
        let lease_after = owner_lease_path(Path::new(&acquired.identity().root));
        prop_assert!(lease_after.exists());
        acquired.release().expect("release owner lease");
        prop_assert!(
            !lease_after.exists(),
            "release must free the lease so a later process can acquire it"
        );

        match try_own(&spelled).expect("re-acquire") {
            OwnerSession::Acquired(owned) => {
                prop_assert!(owned.database().is_some());
                drop(owned);
            }
            OwnerSession::Attached(_) => {
                return Err(TestCaseError::fail("a vacant workspace must be acquirable"));
            }
        }
    }
}
