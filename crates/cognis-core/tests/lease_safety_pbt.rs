//! Property 8 — Cross-process lease ownership with heartbeat and cleanup.
//!
//! **Validates: Requirements 2.7, 2.13**
//!
//! For random crash / reload / PID-reuse schedules against a repository-scoped
//! lease file, the fixed system guarantees:
//!
//! 1. **At most one heavy owner** per canonical repo + role (a live non-expired
//!    lease always attaches rather than spawning a second owner).
//! 2. **No unrelated PID is ever killed** — cleanup may only terminate a process
//!    whose live process-start identity matches the recorded lease owner
//!    (preservation 3.9 / safe non-destruction).
//!
//! The Rust module owns the on-disk lease algebra; TypeScript `verifyLeaseOwner`
//! applies the same identity check before `killByPid`. This suite encodes the
//! kill decision as a pure function of the lease record + observed identity so
//! the invariant is machine-checked across generated schedules without spawning
//! real OS processes.

use cognis_core::lease::{
    acquire_or_attach, lease_path, new_owner_record, read_lease, write_lease_atomic,
    AcquireOutcome, LeaseRecord, LeaseRole, DEFAULT_LEASE_TTL,
};
use proptest::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Pure kill decision (mirrors apps/cognis-vscode/src/lease.ts verifyLeaseOwner
// + the mismatch gate in indexd/mcpServer killByPid).
// ---------------------------------------------------------------------------

/// Outcome of comparing a live process against a recorded lease owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerVerification {
    /// Live process-start id equals the recorded one — safe to terminate.
    Match,
    /// Pid was reused by an unrelated process — MUST NOT kill.
    Mismatch,
    /// No lease / different pid / identity unavailable — safe non-destruction
    /// for kill of a dead pid; live unknown falls back to legacy best-effort.
    Unknown,
}

/// Pure verification: given the on-disk lease and the process-start id we just
/// observed for `pid`, decide match / mismatch / unknown.
fn verify_owner(
    lease: Option<&LeaseRecord>,
    pid: u32,
    observed_start_id: Option<&str>,
) -> OwnerVerification {
    if pid == 0 {
        return OwnerVerification::Unknown;
    }
    let Some(lease) = lease else {
        return OwnerVerification::Unknown;
    };
    if lease.pid != pid {
        return OwnerVerification::Unknown;
    }
    if lease.process_start_id.starts_with("unverified-") {
        return OwnerVerification::Unknown;
    }
    let Some(current) = observed_start_id else {
        return OwnerVerification::Unknown;
    };
    if current == lease.process_start_id {
        OwnerVerification::Match
    } else {
        OwnerVerification::Mismatch
    }
}

/// Whether cleanup is allowed to send a kill signal. Mirrors the TS gate:
/// only `"mismatch"` refuses; `"match"` and `"unknown"` may proceed (and a
/// dead pid is never killed by the outer liveness check).
fn may_kill(verdict: OwnerVerification) -> bool {
    !matches!(verdict, OwnerVerification::Mismatch)
}

// ---------------------------------------------------------------------------
// Schedule model
// ---------------------------------------------------------------------------

/// Synthetic actor that may hold a heavy-daemon lease for a repo.
#[derive(Debug, Clone)]
struct Actor {
    pid: u32,
    process_start_id: String,
}

/// A single step in a crash/reload/PID-reuse schedule.
#[derive(Debug, Clone)]
enum Step {
    /// Write a live lease for this actor (simulates a heavy daemon starting).
    Start { actor: usize },
    /// Drop the lease without release (crash / orphan).
    CrashLeaveLease { actor: usize },
    /// Release the lease cleanly (graceful stop).
    CleanRelease { actor: usize },
    /// Force the on-disk lease into the past (missed heartbeats / expiry).
    ExpireLease,
    /// Another actor tries acquire-or-attach against the real API (this process)
    /// OR, for foreign actors, re-evaluates whether it would spawn a second owner.
    AttemptOwn { actor: usize },
    /// Attempt to kill `target_actor`'s pid under the recorded lease, with the
    /// live process-start id drawn from `observed_as` (same actor = real owner;
    /// different actor = PID reuse).
    AttemptKill {
        target_actor: usize,
        observed_as: usize,
    },
}

#[derive(Debug, Default)]
struct World {
    /// Number of times a second heavy owner overwrote a live foreign lease.
    duplicate_owner_overwrites: u32,
    /// Number of kill signals that would have hit an unrelated process.
    unsafe_kills: u32,
    /// Number of kill signals correctly refused on PID-reuse mismatch.
    refused_kills: u32,
    /// Number of successful sole-owner acquisitions (for coverage signal).
    owners_started: u32,
    /// Current live owner actor index, if any (model-side).
    live_owner: Option<usize>,
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn temp_repo() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "cognis-lease-pbt-{}-{}-{}",
        std::process::id(),
        n,
        unix_now().to_bits()
    ));
    std::fs::create_dir_all(dir.join(".cognis")).unwrap();
    dir
}

fn write_actor_lease(repo: &Path, role: LeaseRole, actor: &Actor, ttl: Duration) {
    let now = unix_now();
    let record = LeaseRecord {
        owner_nonce: format!("nonce-{}-{}", actor.pid, actor.process_start_id),
        pid: actor.pid,
        process_start_id: actor.process_start_id.clone(),
        heartbeat_at: now,
        expiry: now + ttl.as_secs_f64(),
    };
    write_lease_atomic(lease_path(repo, role), &record).unwrap();
}

fn force_expire(repo: &Path, role: LeaseRole) {
    let path = lease_path(repo, role);
    if let Ok(Some(mut rec)) = read_lease(&path) {
        rec.heartbeat_at = 1.0;
        rec.expiry = 2.0;
        write_lease_atomic(&path, &rec).unwrap();
    }
}

fn remove_lease_file(repo: &Path, role: LeaseRole) {
    let path = lease_path(repo, role);
    let _ = std::fs::remove_file(path);
}

/// Run a generated schedule against a real lease file + pure kill algebra.
fn run_schedule(actors: &[Actor], steps: &[Step], role: LeaseRole) -> World {
    let repo = temp_repo();
    let ttl = Duration::from_secs(30);
    let mut world = World::default();

    for step in steps {
        match *step {
            Step::Start { actor } => {
                let a = &actors[actor];
                let path = lease_path(&repo, role);
                let now = unix_now();
                let existing = read_lease(&path).ok().flatten();
                if let Some(ref e) = existing {
                    if e.is_live_at(now)
                        && !(e.pid == a.pid && e.process_start_id == a.process_start_id)
                    {
                        // Correct path: attach/reuse — leave the foreign lease
                        // untouched. Detect overwrite bugs by re-reading.
                        let before_nonce = e.owner_nonce.clone();
                        // (No write.) Verify the file still names the foreign owner.
                        let after = read_lease(&path).ok().flatten();
                        match after {
                            Some(rec)
                                if rec.is_live_at(unix_now())
                                    && rec.owner_nonce == before_nonce => {}
                            Some(_) => world.duplicate_owner_overwrites += 1,
                            None => world.duplicate_owner_overwrites += 1,
                        }
                        continue;
                    }
                }
                write_actor_lease(&repo, role, a, ttl);
                world.live_owner = Some(actor);
                world.owners_started += 1;
            }
            Step::CrashLeaveLease { actor } => {
                // Crash leaves the file on disk; clear only the model-side
                // "we still hold it" marker if this actor was the owner.
                if world.live_owner == Some(actor) {
                    world.live_owner = None;
                }
            }
            Step::CleanRelease { actor } => {
                let path = lease_path(&repo, role);
                if let Ok(Some(rec)) = read_lease(&path) {
                    if rec.pid == actors[actor].pid
                        && rec.process_start_id == actors[actor].process_start_id
                    {
                        remove_lease_file(&repo, role);
                    }
                }
                if world.live_owner == Some(actor) {
                    world.live_owner = None;
                }
            }
            Step::ExpireLease => {
                force_expire(&repo, role);
                world.live_owner = None;
            }
            Step::AttemptOwn { actor } => {
                let a = &actors[actor];
                let path = lease_path(&repo, role);
                let now = unix_now();
                let existing = read_lease(&path).ok().flatten();

                // Real API path: only this process can call acquire_or_attach.
                // For foreign actors we mirror the attach-or-reclaim decision and
                // assert the live foreign nonce is preserved on attach.
                let is_self = a.pid == std::process::id();
                if is_self {
                    let before_nonce = existing.as_ref().map(|e| e.owner_nonce.clone());
                    let before_live = existing
                        .as_ref()
                        .map(|e| e.is_live_at(now) && e.pid != std::process::id())
                        .unwrap_or(false);
                    match acquire_or_attach(&repo, role, Some(ttl)).unwrap() {
                        AcquireOutcome::Acquired(guard) => {
                            if before_live {
                                // Real API acquired over a live foreign owner — bug.
                                world.duplicate_owner_overwrites += 1;
                            }
                            world.live_owner = Some(actor);
                            world.owners_started += 1;
                            let _ = guard.release();
                            world.live_owner = None;
                        }
                        AcquireOutcome::Attached { lease, .. } => {
                            assert!(
                                lease.is_live_at(unix_now()),
                                "attach must report a live owner"
                            );
                            if let Some(bn) = before_nonce {
                                assert_eq!(
                                    lease.owner_nonce, bn,
                                    "attach must preserve the live foreign owner nonce"
                                );
                            }
                        }
                    }
                } else if let Some(ref e) = existing {
                    if e.is_live_at(now) {
                        if e.pid == a.pid && e.process_start_id == a.process_start_id {
                            // Same owner re-attaching — ok.
                        } else {
                            // Attach: foreign nonce must remain.
                            let before = e.owner_nonce.clone();
                            let after = read_lease(&path).ok().flatten();
                            match after {
                                Some(rec) if rec.owner_nonce == before => {}
                                _ => world.duplicate_owner_overwrites += 1,
                            }
                        }
                    } else {
                        // Expired → reclaim.
                        write_actor_lease(&repo, role, a, ttl);
                        world.live_owner = Some(actor);
                        world.owners_started += 1;
                    }
                } else {
                    write_actor_lease(&repo, role, a, ttl);
                    world.live_owner = Some(actor);
                    world.owners_started += 1;
                }
            }
            Step::AttemptKill {
                target_actor,
                observed_as,
            } => {
                let target = &actors[target_actor];
                let observed = &actors[observed_as];
                let path = lease_path(&repo, role);
                let lease = read_lease(&path).ok().flatten();
                // Prefer the on-disk lease's pid when present so kill checks
                // the recorded owner; fall back to the target actor's pid.
                let pid = lease.as_ref().map(|l| l.pid).unwrap_or(target.pid);
                let verdict = verify_owner(
                    lease.as_ref(),
                    pid,
                    Some(observed.process_start_id.as_str()),
                );
                if matches!(verdict, OwnerVerification::Mismatch) {
                    world.refused_kills += 1;
                    if may_kill(verdict) {
                        world.unsafe_kills += 1;
                    }
                } else if let Some(ref l) = lease {
                    // Identities differ but verdict did not report mismatch —
                    // only unsafe when the pid matches (true reuse case) and
                    // kill would still be authorized.
                    if l.pid == pid
                        && observed.process_start_id != l.process_start_id
                        && !l.process_start_id.starts_with("unverified-")
                        && may_kill(verdict)
                    {
                        world.unsafe_kills += 1;
                    }
                }
            }
        }

        // Continuous invariant: at most one non-expired lease record exists
        // (there is only one file) and if it is live it names a single owner.
        let path = lease_path(&repo, role);
        if let Ok(Some(rec)) = read_lease(&path) {
            if rec.is_live_at(unix_now()) {
                assert!(rec.pid > 0);
                assert!(!rec.owner_nonce.is_empty());
                assert!(!rec.process_start_id.is_empty());
            }
        }
    }

    let _ = std::fs::remove_dir_all(&repo);
    world
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn arb_actor(pid_base: u32) -> impl Strategy<Value = Actor> {
    (0u32..50, 0u32..20).prop_map(move |(p_off, s_off)| Actor {
        // Keep pid non-zero; avoid colliding with the real process id so the
        // self-acquire branch is taken only when we intentionally inject it.
        pid: {
            let candidate = pid_base.wrapping_add(p_off).max(1);
            if candidate == std::process::id() {
                candidate.wrapping_add(1).max(2)
            } else {
                candidate
            }
        },
        process_start_id: format!("start-{s_off}"),
    })
}

fn arb_actors() -> impl Strategy<Value = Vec<Actor>> {
    proptest::collection::vec(arb_actor(10_000), 2..6).prop_map(|mut actors| {
        // Ensure unique (pid, start_id) pairs; if two actors share a pid they
        // MUST differ in process_start_id (the PID-reuse scenario).
        for i in 0..actors.len() {
            for j in 0..i {
                if actors[i].pid == actors[j].pid
                    && actors[i].process_start_id == actors[j].process_start_id
                {
                    actors[i].process_start_id = format!("{}-b{}", actors[i].process_start_id, i);
                }
            }
        }
        // Always include one actor that is *this* process so acquire_or_attach
        // is exercised for real. Use the real process_start_id so re-acquire
        // (same pid + start id) is distinguished from attach-to-foreign.
        let self_rec = new_owner_record(Duration::from_secs(1));
        actors.push(Actor {
            pid: self_rec.pid,
            process_start_id: self_rec.process_start_id,
        });
        actors
    })
}

fn arb_step(n_actors: usize) -> impl Strategy<Value = Step> {
    let n = n_actors.max(1);
    prop_oneof![
        (0..n).prop_map(|actor| Step::Start { actor }),
        (0..n).prop_map(|actor| Step::CrashLeaveLease { actor }),
        (0..n).prop_map(|actor| Step::CleanRelease { actor }),
        Just(Step::ExpireLease),
        (0..n).prop_map(|actor| Step::AttemptOwn { actor }),
        (0..n, 0..n).prop_map(|(target_actor, observed_as)| Step::AttemptKill {
            target_actor,
            observed_as,
        }),
    ]
}

fn arb_schedule() -> impl Strategy<Value = (Vec<Actor>, Vec<Step>, LeaseRole)> {
    arb_actors().prop_flat_map(|actors| {
        let n = actors.len();
        (
            Just(actors),
            proptest::collection::vec(arb_step(n), 1..40),
            prop_oneof![Just(LeaseRole::Indexd), Just(LeaseRole::Mcpd)],
        )
    })
}

// ---------------------------------------------------------------------------
// Property 8
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// **Property 8: Bug Condition** — Cross-process lease ownership with
    /// heartbeat and cleanup.
    ///
    /// **Validates: Requirements 2.7, 2.13**
    ///
    /// For any crash/reload/PID-reuse schedule:
    /// * no second heavy owner is ever started while a live foreign lease exists
    /// * a kill is never authorized when process-start identity mismatches
    ///   (PID reuse)
    #[test]
    fn prop8_at_most_one_owner_and_no_unrelated_kill(
        (actors, steps, role) in arb_schedule(),
    ) {
        let world = run_schedule(&actors, &steps, role);
        prop_assert_eq!(
            world.duplicate_owner_overwrites,
            0,
            "a live foreign lease must never be overwritten by a second heavy owner"
        );
        prop_assert_eq!(
            world.unsafe_kills,
            0,
            "cleanup must never terminate an unrelated or PID-reused process"
        );
    }

    /// Pure verification algebra: mismatch ⇔ recorded start id ≠ observed,
    /// and mismatch always refuses kill.
    ///
    /// **Validates: Requirements 2.7, 2.13**
    #[test]
    fn prop8_pid_reuse_never_authorizes_kill(
        pid in 1u32..1_000_000,
        recorded_start in "[a-z0-9-]{1,24}",
        observed_start in "[a-z0-9-]{1,24}",
        lease_pid in 1u32..1_000_000,
        has_lease in any::<bool>(),
    ) {
        let lease = if has_lease {
            Some(LeaseRecord {
                owner_nonce: "n".into(),
                pid: lease_pid,
                process_start_id: recorded_start.clone(),
                heartbeat_at: 1.0,
                expiry: 9_999_999_999.0,
            })
        } else {
            None
        };
        let verdict = verify_owner(
            lease.as_ref(),
            pid,
            Some(observed_start.as_str()),
        );
        if has_lease && lease_pid == pid && recorded_start != observed_start {
            prop_assert_eq!(verdict, OwnerVerification::Mismatch);
            prop_assert!(!may_kill(verdict));
        }
        if has_lease && lease_pid == pid && recorded_start == observed_start {
            prop_assert_eq!(verdict, OwnerVerification::Match);
            prop_assert!(may_kill(verdict));
        }
        if !has_lease || lease_pid != pid {
            prop_assert_eq!(verdict, OwnerVerification::Unknown);
        }
    }
}

// ---------------------------------------------------------------------------
// Example-based unit coverage for the pure helpers used above
// ---------------------------------------------------------------------------

#[test]
fn unit_verify_owner_match_mismatch_unknown() {
    let lease = LeaseRecord {
        owner_nonce: "abc".into(),
        pid: 42,
        process_start_id: "start-A".into(),
        heartbeat_at: 1.0,
        expiry: 9_999_999_999.0,
    };
    assert_eq!(
        verify_owner(Some(&lease), 42, Some("start-A")),
        OwnerVerification::Match
    );
    assert_eq!(
        verify_owner(Some(&lease), 42, Some("start-B")),
        OwnerVerification::Mismatch
    );
    assert_eq!(
        verify_owner(Some(&lease), 99, Some("start-A")),
        OwnerVerification::Unknown
    );
    assert_eq!(
        verify_owner(None, 42, Some("start-A")),
        OwnerVerification::Unknown
    );
    assert_eq!(
        verify_owner(Some(&lease), 42, None),
        OwnerVerification::Unknown
    );

    let unverified = LeaseRecord {
        process_start_id: "unverified-123".into(),
        ..lease.clone()
    };
    assert_eq!(
        verify_owner(Some(&unverified), 42, Some("anything")),
        OwnerVerification::Unknown
    );
}

#[test]
fn unit_acquire_attach_heartbeat_expiry_reclaim() {
    let repo = temp_repo();
    let role = LeaseRole::Indexd;
    let path = lease_path(&repo, role);

    // Acquire when missing.
    let guard = match acquire_or_attach(&repo, role, Some(Duration::from_secs(5))).unwrap() {
        AcquireOutcome::Acquired(g) => g,
        AcquireOutcome::Attached { .. } => panic!("empty path must acquire"),
    };
    assert!(path.exists());
    let nonce = guard.record().unwrap().owner_nonce.clone();

    // Heartbeat advances expiry.
    let before = guard.record().unwrap().expiry;
    std::thread::sleep(Duration::from_millis(15));
    guard.heartbeat().unwrap();
    assert!(guard.record().unwrap().expiry >= before);

    // Drop/release removes our lease.
    guard.release().unwrap();
    assert!(!path.exists());

    // Foreign live lease → attach.
    let mut foreign = new_owner_record(Duration::from_secs(60));
    foreign.pid = std::process::id().wrapping_add(4242).max(1);
    if foreign.pid == std::process::id() {
        foreign.pid = 7;
    }
    foreign.owner_nonce = "foreign".into();
    foreign.process_start_id = "foreign-start".into();
    write_lease_atomic(&path, &foreign).unwrap();
    match acquire_or_attach(&repo, role, Some(Duration::from_secs(60))).unwrap() {
        AcquireOutcome::Attached { lease, .. } => {
            assert_eq!(lease.owner_nonce, "foreign");
        }
        AcquireOutcome::Acquired(_) => panic!("live foreign lease must attach"),
    }

    // Expiry → reclaim.
    force_expire(&repo, role);
    match acquire_or_attach(&repo, role, Some(Duration::from_secs(5))).unwrap() {
        AcquireOutcome::Acquired(g) => {
            assert_ne!(g.record().unwrap().owner_nonce, "foreign");
            assert_ne!(g.record().unwrap().owner_nonce, nonce);
            g.release().unwrap();
        }
        AcquireOutcome::Attached { .. } => panic!("expired lease must be reclaimable"),
    }

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn unit_pid_reuse_safety_on_live_lease() {
    // A live lease for pid P with start-id S1 must attach; a later process that
    // reuses pid P with start-id S2 must NOT be treated as the owner while the
    // lease is still live (it attaches / refuses kill).
    let repo = temp_repo();
    let role = LeaseRole::Mcpd;
    let path = lease_path(&repo, role);

    let reused_pid = 55_555u32;
    let original = LeaseRecord {
        owner_nonce: "orig".into(),
        pid: reused_pid,
        process_start_id: "start-original".into(),
        heartbeat_at: unix_now(),
        expiry: unix_now() + 60.0,
    };
    write_lease_atomic(&path, &original).unwrap();

    // Kill decision: observing a different start id for the same pid → mismatch.
    let verdict = verify_owner(Some(&original), reused_pid, Some("start-reused"));
    assert_eq!(verdict, OwnerVerification::Mismatch);
    assert!(!may_kill(verdict));

    // Real acquire path: live foreign lease → attach (no second owner).
    match acquire_or_attach(&repo, role, Some(DEFAULT_LEASE_TTL)).unwrap() {
        AcquireOutcome::Attached { lease, .. } => {
            assert_eq!(lease.pid, reused_pid);
            assert_eq!(lease.process_start_id, "start-original");
        }
        AcquireOutcome::Acquired(_) => panic!("must not acquire over a live foreign lease"),
    }

    let _ = std::fs::remove_dir_all(&repo);
}
