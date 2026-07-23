import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import type { HandshakeResult } from "../contract";
import {
  CompatibilityCoordinator,
  StaleCompatibilityProbeError,
} from "../compatibilityCoordinator";

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function result(
  engineVersion: string,
  compatibility: HandshakeResult["compatibility"] = "ok"
): HandshakeResult {
  return {
    compatibility,
    backendContractVersion: 1,
    expectedContractVersion: 1,
    engineVersion,
    expectedEngineVersion: "0.8.11",
    missingCommands: [],
    missingTools: [],
    usable: compatibility !== "unreadable",
  };
}

function tempRoot(tag: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), `cognis-compat-${tag}-`));
}
test("cache miss starts one probe and forwards the caller root and expected version", async () => {
  const root = tempRoot("miss");
  const compatible = result("0.8.11");
  const calls: Array<[string, string | undefined]> = [];
  const coordinator = new CompatibilityCoordinator(async (repoRoot, expectedVersion) => {
    calls.push([repoRoot, expectedVersion]);
    return compatible;
  }, 30_000, () => 1_000);

  try {
    assert.equal(coordinator.peek(root), undefined);
    const snapshot = await coordinator.getOrProbe(root, "0.8.11");

    assert.deepEqual(calls, [[root, "0.8.11"]]);
    assert.deepEqual(snapshot, {
      phase: "confirmed",
      result: compatible,
      generation: 1,
      observedAt: 1_000,
    });
    assert.strictEqual(coordinator.peek(root), snapshot);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("fresh cache hit preserves identity and performs no probe", async () => {
  const root = tempRoot("hit");
  let calls = 0;
  let now = 5_000;
  const coordinator = new CompatibilityCoordinator(
    async () => {
      calls += 1;
      return result("0.8.11");
    },
    30_000,
    () => now
  );

  try {
    const first = await coordinator.getOrProbe(root, "0.8.11");
    now += 29_999;
    const cachedPromise = coordinator.getOrProbe(root, "ignored-on-cache-hit");
    const cached = await cachedPromise;

    assert.strictEqual(cached, first);
    assert.strictEqual(coordinator.peek(root), first);
    assert.equal(calls, 1);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("snapshot is expired at the exact TTL boundary and old snapshot stays visible until commit", async () => {
  const root = tempRoot("ttl-boundary");
  let now = 10_000;
  let calls = 0;
  const secondFlight = deferred<HandshakeResult | undefined>();
  const coordinator = new CompatibilityCoordinator(
    async () => {
      calls += 1;
      return calls === 1 ? result("0.8.11") : secondFlight.promise;
    },
    30_000,
    () => now
  );

  try {
    const first = await coordinator.getOrProbe(root, "0.8.11");
    now += 30_000;
    const refresh = coordinator.getOrProbe(root, "0.8.12");
    await Promise.resolve();

    assert.equal(calls, 2, "age === TTL must be a cache miss");
    assert.strictEqual(coordinator.peek(root), first);

    const updated = result("0.8.12", "engine-outdated");
    secondFlight.resolve(updated);
    const second = await refresh;
    assert.equal(second.generation, 2);
    assert.strictEqual(second.phase === "confirmed" && second.result, updated);
    assert.strictEqual(coordinator.peek(root), second);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("concurrent normal requests use exactly one in-flight probe", async () => {
  const root = tempRoot("normal-flight");
  const flight = deferred<HandshakeResult | undefined>();
  const calls: Array<[string, string | undefined]> = [];
  const coordinator = new CompatibilityCoordinator(async (repoRoot, expectedVersion) => {
    calls.push([repoRoot, expectedVersion]);
    return flight.promise;
  });

  try {
    const first = coordinator.getOrProbe(root, "0.8.11");
    const joined = coordinator.getOrProbe(root, "0.8.99");

    assert.strictEqual(joined, first);
    await Promise.resolve();
    assert.deepEqual(calls, [[root, "0.8.11"]]);

    flight.resolve(result("0.8.11"));
    assert.strictEqual(await joined, await first);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
test("force bypasses a fresh snapshot and concurrent force requests share the forced flight", async () => {
  const root = tempRoot("force-cache");
  let calls = 0;
  const forcedFlight = deferred<HandshakeResult | undefined>();
  const coordinator = new CompatibilityCoordinator(async () => {
    calls += 1;
    return calls === 1 ? result("0.8.11") : forcedFlight.promise;
  });

  try {
    const cached = await coordinator.getOrProbe(root, "0.8.11");
    const forced = coordinator.getOrProbe(root, "0.8.12", { force: true });
    const joinedForce = coordinator.getOrProbe(root, "0.8.13", { force: true });

    assert.strictEqual(joinedForce, forced);
    assert.strictEqual(coordinator.peek(root), cached);
    await Promise.resolve();
    assert.equal(calls, 2);

    forcedFlight.resolve(result("0.8.12", "engine-outdated"));
    const refreshed = await forced;
    assert.equal(refreshed.generation, 2);
    assert.strictEqual(await joinedForce, refreshed);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("a normal request during a forced refresh still receives the fresh completed snapshot", async () => {
  const root = tempRoot("normal-during-force");
  const forcedFlight = deferred<HandshakeResult | undefined>();
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => {
    calls += 1;
    return calls === 1 ? result("0.8.11") : forcedFlight.promise;
  });

  try {
    const cached = await coordinator.getOrProbe(root, "0.8.11");
    const forced = coordinator.getOrProbe(root, "0.8.11", { force: true });
    const normal = coordinator.getOrProbe(root, "0.8.11");

    assert.notStrictEqual(normal, forced);
    assert.strictEqual(await normal, cached);
    await Promise.resolve();
    assert.equal(calls, 2, "normal refresh must not start a third probe");

    forcedFlight.resolve(result("0.8.12", "engine-outdated"));
    const refreshed = await forced;
    assert.notStrictEqual(refreshed, cached);
    assert.strictEqual(coordinator.peek(root), refreshed);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("force supersedes a normal flight and old completion after new completion is rejected", async () => {
  const root = tempRoot("old-after-new");
  const flights = [
    deferred<HandshakeResult | undefined>(),
    deferred<HandshakeResult | undefined>(),
  ];
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => flights[calls++]!.promise);

  try {
    const oldProbe = coordinator.getOrProbe(root, "0.8.11");
    await Promise.resolve();
    const newProbe = coordinator.getOrProbe(root, "0.8.12", { force: true });
    await Promise.resolve();
    assert.equal(calls, 2);

    const newResult = result("0.8.12", "engine-outdated");
    flights[1].resolve(newResult);
    const newest = await newProbe;
    assert.equal(newest.generation, 2);
    assert.strictEqual(newest.phase === "confirmed" && newest.result, newResult);

    flights[0].resolve(result("0.8.10", "engine-outdated"));
    await assert.rejects(oldProbe, StaleCompatibilityProbeError);
    assert.strictEqual(coordinator.peek(root), newest);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("old completion before the newer force result is rejected without disturbing the newer flight", async () => {
  const root = tempRoot("old-before-new");
  const flights = [
    deferred<HandshakeResult | undefined>(),
    deferred<HandshakeResult | undefined>(),
  ];
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => flights[calls++]!.promise);

  try {
    const oldProbe = coordinator.getOrProbe(root, "0.8.11");
    await Promise.resolve();
    const newProbe = coordinator.getOrProbe(root, "0.8.12", { force: true });
    await Promise.resolve();

    flights[0].resolve(result("0.8.10", "engine-outdated"));
    await assert.rejects(oldProbe, StaleCompatibilityProbeError);
    assert.equal(coordinator.peek(root), undefined);
    assert.strictEqual(
      coordinator.getOrProbe(root, "0.8.12"),
      newProbe,
      "stale completion must not clear the active newer flight"
    );

    const newResult = result("0.8.12", "engine-outdated");
    flights[1].resolve(newResult);
    const newest = await newProbe;
    assert.strictEqual(newest.phase === "confirmed" && newest.result, newResult);
    assert.strictEqual(coordinator.peek(root), newest);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
test("distinct canonical roots have isolated cache, generation, and in-flight state", async () => {
  const rootA = tempRoot("isolated-a");
  const rootB = tempRoot("isolated-b");
  const pendingA = deferred<HandshakeResult | undefined>();
  const pendingB = deferred<HandshakeResult | undefined>();
  const calls: string[] = [];
  const coordinator = new CompatibilityCoordinator(async (repoRoot) => {
    calls.push(repoRoot);
    return repoRoot === rootA ? pendingA.promise : pendingB.promise;
  });

  try {
    const probeA = coordinator.getOrProbe(rootA, "0.8.11");
    const probeB = coordinator.getOrProbe(rootB, "0.8.11");
    assert.notStrictEqual(probeA, probeB);
    await Promise.resolve();
    assert.deepEqual(calls, [rootA, rootB]);

    const resultA = result("0.8.10", "engine-outdated");
    const resultB = result("0.8.12", "engine-newer");
    pendingA.resolve(resultA);
    pendingB.resolve(resultB);
    const [snapshotA, snapshotB] = await Promise.all([probeA, probeB]);

    assert.equal(snapshotA.generation, 1);
    assert.equal(snapshotB.generation, 1);
    assert.strictEqual(snapshotA.phase === "confirmed" && snapshotA.result, resultA);
    assert.strictEqual(snapshotB.phase === "confirmed" && snapshotB.result, resultB);
    assert.strictEqual(coordinator.peek(rootA), snapshotA);
    assert.strictEqual(coordinator.peek(rootB), snapshotB);
  } finally {
    coordinator.dispose();
    fs.rmSync(rootA, { recursive: true, force: true });
    fs.rmSync(rootB, { recursive: true, force: true });
  }
});

test("canonical aliases share cache identity while the probe receives the first caller path", async (t) => {
  const parent = tempRoot("aliases");
  const realRoot = path.join(parent, "real");
  const aliasRoot = path.join(parent, "alias");
  fs.mkdirSync(realRoot);
  try {
    fs.symlinkSync(realRoot, aliasRoot, process.platform === "win32" ? "junction" : "dir");
  } catch (error) {
    fs.rmSync(parent, { recursive: true, force: true });
    t.skip(`directory alias unavailable: ${String(error)}`);
    return;
  }

  const flight = deferred<HandshakeResult | undefined>();
  const calls: string[] = [];
  const coordinator = new CompatibilityCoordinator(async (repoRoot) => {
    calls.push(repoRoot);
    return flight.promise;
  });

  try {
    const viaAlias = coordinator.getOrProbe(aliasRoot, "0.8.11");
    const viaRealRoot = coordinator.getOrProbe(realRoot, "0.8.11");
    assert.strictEqual(viaRealRoot, viaAlias);
    await Promise.resolve();
    assert.deepEqual(calls, [aliasRoot]);

    flight.resolve(result("0.8.11"));
    const snapshot = await viaAlias;
    assert.strictEqual(coordinator.peek(aliasRoot), snapshot);
    assert.strictEqual(coordinator.peek(realRoot), snapshot);
  } finally {
    coordinator.dispose();
    fs.rmSync(parent, { recursive: true, force: true });
  }
});

test("undefined and thrown probes without a prior mismatch commit unavailable snapshots", async () => {
  for (const scenario of ["undefined", "throw"] as const) {
    const root = tempRoot(`initial-${scenario}`);
    let now = scenario === "undefined" ? 42 : 43;
    const coordinator = new CompatibilityCoordinator(
      async () => {
        if (scenario === "throw") {
          throw new Error("probe failed");
        }
        return undefined;
      },
      30_000,
      () => now
    );

    try {
      const unavailable = await coordinator.getOrProbe(root, "0.8.11");
      assert.deepEqual(unavailable, {
        phase: "unavailable",
        generation: 1,
        observedAt: now,
      });
      assert.strictEqual(coordinator.peek(root), unavailable);
    } finally {
      coordinator.dispose();
      fs.rmSync(root, { recursive: true, force: true });
    }
  }
});
test("unavailable refresh retains the prior mismatch but not a prior confirmed ok", async () => {
  for (const scenario of ["mismatch", "ok"] as const) {
    const root = tempRoot(`prior-${scenario}`);
    const initial = scenario === "mismatch"
      ? result("0.8.10", "engine-outdated")
      : result("0.8.11", "ok");
    let calls = 0;
    let now = 100;
    const coordinator = new CompatibilityCoordinator(
      async () => calls++ === 0 ? initial : undefined,
      30_000,
      () => now
    );

    try {
      const first = await coordinator.getOrProbe(root, "0.8.11");
      now = 200;
      const refreshed = await coordinator.getOrProbe(root, "0.8.11", { force: true });

      assert.equal(refreshed.generation, 2);
      assert.equal(refreshed.observedAt, 200);
      if (scenario === "mismatch") {
        assert.equal(refreshed.phase, "confirmed");
        assert.strictEqual(refreshed.phase === "confirmed" && refreshed.result, initial);
      } else {
        assert.deepEqual(refreshed, {
          phase: "unavailable",
          generation: 2,
          observedAt: 200,
        });
      }
      assert.notStrictEqual(refreshed, first);
      assert.strictEqual(coordinator.peek(root), refreshed);
    } finally {
      coordinator.dispose();
      fs.rmSync(root, { recursive: true, force: true });
    }
  }
});

test("a new confirmed mismatch replaces the prior mismatch and confirmed ok clears it", async () => {
  const root = tempRoot("mismatch-transitions");
  const oldMismatch = result("0.8.10", "engine-outdated");
  const newMismatch = result("0.8.12", "engine-newer");
  const compatible = result("0.8.11", "ok");
  const results = [oldMismatch, newMismatch, compatible];
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => results[calls++]!);

  try {
    const first = await coordinator.getOrProbe(root, "0.8.11");
    assert.strictEqual(first.phase === "confirmed" && first.result, oldMismatch);

    const replaced = await coordinator.getOrProbe(root, "0.8.11", { force: true });
    assert.strictEqual(replaced.phase === "confirmed" && replaced.result, newMismatch);

    const cleared = await coordinator.getOrProbe(root, "0.8.11", { force: true });
    assert.strictEqual(cleared.phase === "confirmed" && cleared.result, compatible);
    assert.equal(cleared.phase === "confirmed" && cleared.result.compatibility, "ok");
    assert.strictEqual(coordinator.peek(root), cleared);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("evict clears a completed snapshot and a later request starts fresh state", async () => {
  const root = tempRoot("evict-completed");
  const outputs = [result("0.8.10", "engine-outdated"), result("0.8.11", "ok")];
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => outputs[calls++]!);

  try {
    const beforeEvict = await coordinator.getOrProbe(root, "0.8.11");
    coordinator.evict(root);
    assert.equal(coordinator.peek(root), undefined);

    const afterEvict = await coordinator.getOrProbe(root, "0.8.11");
    assert.equal(calls, 2);
    assert.notStrictEqual(afterEvict, beforeEvict);
    assert.equal(afterEvict.generation, 1, "a re-opened root gets new per-root state");
    assert.strictEqual(afterEvict.phase === "confirmed" && afterEvict.result, outputs[1]);
  } finally {
    coordinator.dispose();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("evict invalidates an in-flight alias and prevents its result from repopulating cache", async (t) => {
  const parent = tempRoot("evict-flight");
  const realRoot = path.join(parent, "real");
  const aliasRoot = path.join(parent, "alias");
  fs.mkdirSync(realRoot);
  try {
    fs.symlinkSync(realRoot, aliasRoot, process.platform === "win32" ? "junction" : "dir");
  } catch (error) {
    fs.rmSync(parent, { recursive: true, force: true });
    t.skip(`directory alias unavailable: ${String(error)}`);
    return;
  }

  const flight = deferred<HandshakeResult | undefined>();
  const coordinator = new CompatibilityCoordinator(async () => flight.promise);

  try {
    const pending = coordinator.getOrProbe(aliasRoot, "0.8.11");
    await Promise.resolve();
    coordinator.evict(realRoot);
    assert.equal(coordinator.peek(aliasRoot), undefined);

    flight.resolve(result("0.8.10", "engine-outdated"));
    await assert.rejects(pending, StaleCompatibilityProbeError);
    assert.equal(coordinator.peek(realRoot), undefined);
  } finally {
    coordinator.dispose();
    fs.rmSync(parent, { recursive: true, force: true });
  }
});
test("dispose clears all completed roots, is idempotent, and rejects future probes", async () => {
  const rootA = tempRoot("dispose-a");
  const rootB = tempRoot("dispose-b");
  let calls = 0;
  const coordinator = new CompatibilityCoordinator(async () => {
    calls += 1;
    return result("0.8.11");
  });

  try {
    await coordinator.getOrProbe(rootA, "0.8.11");
    await coordinator.getOrProbe(rootB, "0.8.11");
    coordinator.dispose();
    coordinator.dispose();

    assert.equal(coordinator.peek(rootA), undefined);
    assert.equal(coordinator.peek(rootB), undefined);
    assert.throws(() => coordinator.getOrProbe(rootA, "0.8.11"), /disposed/);
    assert.equal(calls, 2);
  } finally {
    coordinator.dispose();
    fs.rmSync(rootA, { recursive: true, force: true });
    fs.rmSync(rootB, { recursive: true, force: true });
  }
});

test("dispose invalidates every in-flight probe even when results complete afterward", async () => {
  const rootA = tempRoot("dispose-flight-a");
  const rootB = tempRoot("dispose-flight-b");
  const flightA = deferred<HandshakeResult | undefined>();
  const flightB = deferred<HandshakeResult | undefined>();
  const coordinator = new CompatibilityCoordinator(async (repoRoot) =>
    repoRoot === rootA ? flightA.promise : flightB.promise
  );

  try {
    const pendingA = coordinator.getOrProbe(rootA, "0.8.11");
    const pendingB = coordinator.getOrProbe(rootB, "0.8.11");
    await Promise.resolve();
    coordinator.dispose();

    flightA.resolve(result("0.8.10", "engine-outdated"));
    flightB.resolve(result("0.8.11"));
    await assert.rejects(pendingA, StaleCompatibilityProbeError);
    await assert.rejects(pendingB, StaleCompatibilityProbeError);
    assert.equal(coordinator.peek(rootA), undefined);
    assert.equal(coordinator.peek(rootB), undefined);
  } finally {
    coordinator.dispose();
    fs.rmSync(rootA, { recursive: true, force: true });
    fs.rmSync(rootB, { recursive: true, force: true });
  }
});

test("rejects invalid TTL values", () => {
  const probe = async (): Promise<HandshakeResult | undefined> => undefined;
  for (const ttl of [-1, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => new CompatibilityCoordinator(probe, ttl), RangeError);
  }
});
