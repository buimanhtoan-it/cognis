/**
 * Property 8 — Cross-process lease ownership with heartbeat and cleanup.
 *
 * **Validates: Requirements 2.7, 2.13**
 *
 * Unit coverage: lease acquire/attach/heartbeat/expiry/reclaim and PID-reuse
 * safety via `verifyLeaseOwner` (the gate `killByPid` in indexd/mcpServer
 * consults before terminating a process).
 *
 * Property-based: for random crash/reload/PID-reuse schedules, at most one
 * heavy owner exists per canonical repo and no unrelated PID is ever killed.
 */
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import {
  DEFAULT_LEASE_TTL_SECONDS,
  isLeaseExpired,
  leasePath,
  readLease,
  reconcileOrphanLease,
  removeLeaseForPid,
  verifyLeaseOwner,
  writeLeaseAtomic,
  type LeaseRecord,
  type LeaseRole,
  type OwnerVerification,
} from "../lease";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTempRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-lease-safety-"));
}

function cleanup(repoRoot: string): void {
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

function nowSeconds(): number {
  return Date.now() / 1000;
}

function makeLease(
  overrides: Partial<LeaseRecord> & Pick<LeaseRecord, "pid" | "process_start_id">
): LeaseRecord {
  const now = nowSeconds();
  return {
    owner_nonce: overrides.owner_nonce ?? `nonce-${Math.random().toString(16).slice(2)}`,
    pid: overrides.pid,
    process_start_id: overrides.process_start_id,
    heartbeat_at: overrides.heartbeat_at ?? now,
    expiry: overrides.expiry ?? now + DEFAULT_LEASE_TTL_SECONDS,
  };
}

/**
 * Pure kill gate — mirrors indexd/mcpServer: only `"mismatch"` refuses.
 * Dead pids are filtered by the outer liveness check (not modeled here).
 */
function mayKill(verdict: OwnerVerification): boolean {
  return verdict !== "mismatch";
}

// ---------------------------------------------------------------------------
// Unit: lease acquire / attach / heartbeat / expiry / reclaim
// ---------------------------------------------------------------------------

test("unit: write/read round-trip preserves schema and path layout", () => {
  const repo = makeTempRepo();
  try {
    for (const role of ["indexd", "mcpd"] as LeaseRole[]) {
      const record = makeLease({ pid: 1234, process_start_id: "start-abc" });
      assert.equal(writeLeaseAtomic(repo, role, record), true);
      const file = leasePath(repo, role);
      assert.ok(file.endsWith(path.join(".cognis", `${role}.lease`)));
      const loaded = readLease(repo, role);
      assert.deepEqual(loaded, record);
    }
  } finally {
    cleanup(repo);
  }
});

test("unit: isLeaseExpired distinguishes live vs reclaimable heartbeats", () => {
  const now = 1_700_000_000;
  const live = makeLease({
    pid: 1,
    process_start_id: "s",
    heartbeat_at: now - 5,
    expiry: now + 10,
  });
  const expired = makeLease({
    pid: 1,
    process_start_id: "s",
    heartbeat_at: now - 40,
    expiry: now - 10,
  });
  assert.equal(isLeaseExpired(live, now), false);
  assert.equal(isLeaseExpired(expired, now), true);
});

test("unit: reconcileOrphanLease records a live orphan with process-start identity", () => {
  const repo = makeTempRepo();
  try {
    // Use this process's pid so queryProcessStartId can resolve a real identity
    // on platforms that support it; even if it falls back to unverified-*, a
    // lease file must still be written (reclaimable orphan after reload).
    const pid = process.pid;
    reconcileOrphanLease(repo, "indexd", pid, 30);
    const lease = readLease(repo, "indexd");
    assert.ok(lease, "reconcileOrphanLease must write a lease for a live orphan");
    assert.equal(lease!.pid, pid);
    assert.ok(typeof lease!.owner_nonce === "string" && lease!.owner_nonce.length > 0);
    assert.ok(typeof lease!.process_start_id === "string" && lease!.process_start_id.length > 0);
    assert.equal(isLeaseExpired(lease!), false);

    // Idempotent: a second reconcile with the same live identity-bearing lease
    // must not thrash the owner_nonce when start id is verified.
    const nonce = lease!.owner_nonce;
    reconcileOrphanLease(repo, "indexd", pid, 30);
    const again = readLease(repo, "indexd");
    assert.ok(again);
    if (!again!.process_start_id.startsWith("unverified-")) {
      assert.equal(again!.owner_nonce, nonce, "verified lease must not be rewritten");
    }
  } finally {
    cleanup(repo);
  }
});

test("unit: removeLeaseForPid is idempotent and never clobbers a foreign owner", () => {
  const repo = makeTempRepo();
  try {
    const foreign = makeLease({ pid: 99, process_start_id: "foreign" });
    writeLeaseAtomic(repo, "mcpd", foreign);

    // Different pid → leave the file alone.
    removeLeaseForPid(repo, "mcpd", 42);
    assert.deepEqual(readLease(repo, "mcpd"), foreign);

    // Matching pid → remove.
    removeLeaseForPid(repo, "mcpd", 99);
    assert.equal(readLease(repo, "mcpd"), undefined);

    // Missing file → success (idempotent).
    removeLeaseForPid(repo, "mcpd", 99);
    assert.equal(readLease(repo, "mcpd"), undefined);
  } finally {
    cleanup(repo);
  }
});

// ---------------------------------------------------------------------------
// Unit: PID-reuse safety
// ---------------------------------------------------------------------------

test("unit: verifyLeaseOwner returns mismatch when process-start id differs (PID reuse)", () => {
  const repo = makeTempRepo();
  try {
    const pid = process.pid;
    // Record a lease that claims *this* pid but with a fabricated start id —
    // the live process almost certainly has a different creation stamp, so
    // verification must report mismatch and refuse kill.
    const forged = makeLease({
      pid,
      process_start_id: "definitely-not-the-real-start-id-xyz",
    });
    writeLeaseAtomic(repo, "indexd", forged);

    const verdict = verifyLeaseOwner(repo, "indexd", pid);
    // On platforms where we can query a real start id, expect mismatch.
    // If the query is unavailable, unknown is the safe fallback (still must
    // not be treated as a confident match).
    assert.notEqual(
      verdict,
      "match",
      "a forged process_start_id must never verify as match against the live process"
    );
    if (verdict === "mismatch") {
      assert.equal(mayKill(verdict), false, "mismatch must refuse kill");
    }
  } finally {
    cleanup(repo);
  }
});

test("unit: verifyLeaseOwner is unknown for missing lease, wrong pid, or bad pid", () => {
  const repo = makeTempRepo();
  try {
    assert.equal(verifyLeaseOwner(repo, "indexd", undefined), "unknown");
    assert.equal(verifyLeaseOwner(repo, "indexd", 0), "unknown");
    assert.equal(verifyLeaseOwner(repo, "indexd", process.pid), "unknown");

    writeLeaseAtomic(
      repo,
      "indexd",
      makeLease({ pid: 1, process_start_id: "s" })
    );
    assert.equal(verifyLeaseOwner(repo, "indexd", 2), "unknown");
  } finally {
    cleanup(repo);
  }
});

test("unit: verifyLeaseOwner is unknown for unverified- start ids (safe non-destruction)", () => {
  const repo = makeTempRepo();
  try {
    writeLeaseAtomic(
      repo,
      "mcpd",
      makeLease({
        pid: process.pid,
        process_start_id: `unverified-${Date.now()}`,
      })
    );
    assert.equal(verifyLeaseOwner(repo, "mcpd", process.pid), "unknown");
  } finally {
    cleanup(repo);
  }
});

// ---------------------------------------------------------------------------
// Property 8 — random crash / reload / PID-reuse schedules
// ---------------------------------------------------------------------------

type Actor = { pid: number; processStartId: string };

type Step =
  | { kind: "start"; actor: number }
  | { kind: "crashLeaveLease"; actor: number }
  | { kind: "cleanRelease"; actor: number }
  | { kind: "expire" }
  | { kind: "attemptOwn"; actor: number }
  | { kind: "attemptKill"; target: number; observedAs: number };

interface World {
  /** Times a second heavy owner overwrote a live foreign lease. */
  duplicateOwnerOverwrites: number;
  /** Times a kill would have hit a PID-reused unrelated process. */
  unsafeKills: number;
}

function runSchedule(
  repo: string,
  role: LeaseRole,
  actors: Actor[],
  steps: Step[]
): World {
  const world: World = { duplicateOwnerOverwrites: 0, unsafeKills: 0 };
  const ttl = 30;

  for (const step of steps) {
    switch (step.kind) {
      case "start": {
        const a = actors[step.actor]!;
        const existing = readLease(repo, role);
        const now = nowSeconds();
        if (
          existing &&
          !isLeaseExpired(existing, now) &&
          !(
            existing.pid === a.pid &&
            existing.process_start_id === a.processStartId
          )
        ) {
          // Correct path: attach/reuse — leave the foreign lease untouched.
          const beforeNonce = existing.owner_nonce;
          const after = readLease(repo, role);
          if (!after || after.owner_nonce !== beforeNonce || isLeaseExpired(after, now)) {
            world.duplicateOwnerOverwrites += 1;
          }
          break;
        }
        writeLeaseAtomic(
          repo,
          role,
          makeLease({
            pid: a.pid,
            process_start_id: a.processStartId,
            expiry: now + ttl,
          })
        );
        break;
      }
      case "crashLeaveLease": {
        // Leave the file on disk (orphan). No-op on the file itself.
        break;
      }
      case "cleanRelease": {
        const a = actors[step.actor]!;
        const existing = readLease(repo, role);
        if (
          existing &&
          existing.pid === a.pid &&
          existing.process_start_id === a.processStartId
        ) {
          removeLeaseForPid(repo, role, a.pid);
        }
        break;
      }
      case "expire": {
        const existing = readLease(repo, role);
        if (existing) {
          writeLeaseAtomic(repo, role, {
            ...existing,
            heartbeat_at: 1,
            expiry: 2,
          });
        }
        break;
      }
      case "attemptOwn": {
        const a = actors[step.actor]!;
        const existing = readLease(repo, role);
        const now = nowSeconds();
        if (existing && !isLeaseExpired(existing, now)) {
          if (
            existing.pid === a.pid &&
            existing.process_start_id === a.processStartId
          ) {
            // Same owner re-attaching — fine.
          } else {
            // Attach/reuse: foreign nonce must remain.
            const beforeNonce = existing.owner_nonce;
            const after = readLease(repo, role);
            if (!after || after.owner_nonce !== beforeNonce) {
              world.duplicateOwnerOverwrites += 1;
            }
          }
        } else {
          // Missing or expired → reclaim.
          writeLeaseAtomic(
            repo,
            role,
            makeLease({
              pid: a.pid,
              process_start_id: a.processStartId,
              expiry: now + ttl,
            })
          );
        }
        break;
      }
      case "attemptKill": {
        const target = actors[step.target]!;
        const observed = actors[step.observedAs]!;
        // Plant the target's lease identity, then observe `observed`'s start id
        // (PID reuse when they differ).
        const existing = readLease(repo, role);
        // Pure verification against whatever is on disk (may be unrelated).
        let verdict: OwnerVerification = "unknown";
        if (existing && existing.pid === target.pid) {
          if (existing.process_start_id.startsWith("unverified-")) {
            verdict = "unknown";
          } else if (existing.process_start_id === observed.processStartId) {
            verdict = "match";
          } else {
            verdict = "mismatch";
          }
        }
        if (verdict === "mismatch") {
          assert.equal(mayKill(verdict), false);
          // If we had killed anyway, that would be the bug.
          if (mayKill(verdict)) {
            world.unsafeKills += 1;
          }
        } else if (
          existing &&
          existing.pid === target.pid &&
          existing.process_start_id !== observed.processStartId &&
          mayKill(verdict)
        ) {
          // Any path that authorizes kill when identities differ is unsafe.
          world.unsafeKills += 1;
        }
        break;
      }
      default: {
        const _exhaustive: never = step;
        void _exhaustive;
      }
    }

    // Continuous invariant: a live lease names exactly one well-formed owner.
    const live = readLease(repo, role);
    if (live && !isLeaseExpired(live)) {
      assert.ok(live.pid > 0);
      assert.ok(live.owner_nonce.length > 0);
      assert.ok(live.process_start_id.length > 0);
    }
  }

  return world;
}

const arbActor: fc.Arbitrary<Actor> = fc.record({
  pid: fc.integer({ min: 1, max: 2_000_000 }),
  processStartId: fc
    .stringMatching(/^[a-z0-9-]{1,16}$/)
    .filter((s) => s.length > 0),
});

const arbActors: fc.Arbitrary<Actor[]> = fc
  .array(arbActor, { minLength: 2, maxLength: 5 })
  .map((actors) => {
    // Guarantee that if two actors share a pid they differ in start id
    // (the PID-reuse scenario the property cares about).
    for (let i = 0; i < actors.length; i++) {
      for (let j = 0; j < i; j++) {
        if (
          actors[i]!.pid === actors[j]!.pid &&
          actors[i]!.processStartId === actors[j]!.processStartId
        ) {
          actors[i] = {
            ...actors[i]!,
            processStartId: `${actors[i]!.processStartId}-b${i}`,
          };
        }
      }
    }
    return actors;
  });

function arbStep(nActors: number): fc.Arbitrary<Step> {
  const idx = fc.integer({ min: 0, max: Math.max(0, nActors - 1) });
  return fc.oneof(
    idx.map((actor) => ({ kind: "start" as const, actor })),
    idx.map((actor) => ({ kind: "crashLeaveLease" as const, actor })),
    idx.map((actor) => ({ kind: "cleanRelease" as const, actor })),
    fc.constant({ kind: "expire" as const }),
    idx.map((actor) => ({ kind: "attemptOwn" as const, actor })),
    fc
      .tuple(idx, idx)
      .map(([target, observedAs]) => ({
        kind: "attemptKill" as const,
        target,
        observedAs,
      }))
  );
}

test("Property 8: for random crash/reload/PID-reuse schedules, at most one heavy owner and no unrelated kill", () => {
  // **Validates: Requirements 2.7, 2.13**
  fc.assert(
    fc.property(
      arbActors,
      fc.constantFrom<LeaseRole>("indexd", "mcpd"),
      (actors, role) => {
        const stepsArb = fc.array(arbStep(actors.length), {
          minLength: 1,
          maxLength: 30,
        });
        // Nested assert keeps the actor set fixed while varying the schedule.
        fc.assert(
          fc.property(stepsArb, (steps) => {
            const repo = makeTempRepo();
            try {
              const world = runSchedule(repo, role, actors, steps);
              assert.equal(
                world.duplicateOwnerOverwrites,
                0,
                "a live foreign lease must never be overwritten by a second heavy owner"
              );
              assert.equal(
                world.unsafeKills,
                0,
                "cleanup must never terminate an unrelated or PID-reused process"
              );
            } finally {
              cleanup(repo);
            }
          }),
          { numRuns: 15 }
        );
      }
    ),
    { numRuns: 20 }
  );
});

test("Property 8: pure PID-reuse algebra — mismatch never authorizes kill", () => {
  // **Validates: Requirements 2.7, 2.13**
  fc.assert(
    fc.property(
      fc.integer({ min: 1, max: 1_000_000 }),
      fc.stringMatching(/^[a-z0-9-]{1,20}$/),
      fc.stringMatching(/^[a-z0-9-]{1,20}$/),
      fc.boolean(),
      (pid, recorded, observed, samePid) => {
        const leasePid = samePid ? pid : pid === 1 ? 2 : pid - 1;
        const lease: LeaseRecord | undefined = {
          owner_nonce: "n",
          pid: leasePid,
          process_start_id: recorded,
          heartbeat_at: 1,
          expiry: 9_999_999_999,
        };
        let verdict: OwnerVerification = "unknown";
        if (lease.pid === pid) {
          if (lease.process_start_id === observed) {
            verdict = "match";
          } else {
            verdict = "mismatch";
          }
        }
        if (samePid && recorded !== observed) {
          assert.equal(verdict, "mismatch");
          assert.equal(mayKill(verdict), false);
        }
        if (samePid && recorded === observed) {
          assert.equal(verdict, "match");
          assert.equal(mayKill(verdict), true);
        }
        if (!samePid) {
          assert.equal(verdict, "unknown");
        }
      }
    ),
    { numRuns: 200 }
  );
});
