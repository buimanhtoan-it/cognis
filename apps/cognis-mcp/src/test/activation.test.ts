/**
 * Activation_Manager turn-on order, degrade-not-abort, checksum gate, and
 * watcher ⇔ Managed_MCP_Entry reconciler (task 16.5).
 *
 * Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8, 3.9, 3.11,
 * 3.12, 21.3, 21.4
 */
import "./testHarness";

import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import test from "node:test";

import {
  ENTRY_RESTORE_BUDGET_MS,
  TURN_ON_ORDER,
  WatcherEntryReconciler,
  downloadedPayloadIsUsable,
  runTurnOn,
  type ActivationHost,
  type ActivationStepResult,
} from "../activation";
import { TURN_ON_ORDER as GENERATOR_TURN_ON_ORDER } from "./generators";

const ENGINE_STEPS = [
  "open_database",
  "reconcile_and_index",
  "start_watcher",
  "write_mcp_entry",
] as const;

function sha256Hex(payload: Buffer): string {
  return crypto.createHash("sha256").update(payload).digest("hex");
}

function makeHost(
  overrides: Partial<ActivationHost> & {
    fail?: Partial<Record<keyof ActivationHost, string>>;
    dbExists?: boolean;
    watcherRunning?: boolean;
  } = {}
): { host: ActivationHost; calls: string[]; phases: Array<string | undefined> } {
  const calls: string[] = [];
  const phases: Array<string | undefined> = [];
  let watcher = overrides.watcherRunning ?? true;
  const fail = overrides.fail ?? {};

  const run = async (name: keyof ActivationHost): Promise<void> => {
    calls.push(String(name));
    const message = fail[name];
    if (message) {
      throw new Error(message);
    }
  };

  const host: ActivationHost = {
    signal: overrides.signal ?? new AbortController().signal,
    onPhase: (phase) => {
      phases.push(phase);
      overrides.onPhase?.(phase);
    },
    verifyEngineBinary: async () => {
      await run("verifyEngineBinary");
    },
    verifyModelAssets: async () => {
      await run("verifyModelAssets");
    },
    databaseExists: () => {
      calls.push("databaseExists");
      return overrides.dbExists ?? false;
    },
    openOrCreateDatabase: async (mode) => {
      calls.push(`openOrCreateDatabase:${mode}`);
      if (fail.openOrCreateDatabase) {
        throw new Error(fail.openOrCreateDatabase);
      }
    },
    reconcileAndIndex: async (mode) => {
      calls.push(`reconcileAndIndex:${mode}`);
      if (fail.reconcileAndIndex) {
        throw new Error(fail.reconcileAndIndex);
      }
    },
    startWatcher: async () => {
      await run("startWatcher");
    },
    isWatcherRunning: () => {
      calls.push("isWatcherRunning");
      return watcher;
    },
    writeManagedMcpEntry: async () => {
      await run("writeManagedMcpEntry");
    },
    ...overrides,
  };
  // Restore recording wrappers when overrides replace them.
  if (overrides.verifyEngineBinary) {
    const inner = overrides.verifyEngineBinary;
    host.verifyEngineBinary = async () => {
      calls.push("verifyEngineBinary");
      await inner.call(host);
    };
  }
  if (overrides.verifyModelAssets) {
    const inner = overrides.verifyModelAssets;
    host.verifyModelAssets = async () => {
      calls.push("verifyModelAssets");
      await inner.call(host);
    };
  }
  return { host, calls, phases };
}

function executedPhases(results: ActivationStepResult[]): string[] {
  return results.filter((step) => !step.skipped).map((step) => step.phase);
}

function isSubsequence(trace: string[], canonical: readonly string[]): boolean {
  let index = 0;
  for (const phase of trace) {
    const found = canonical.indexOf(phase as (typeof canonical)[number], index);
    if (found === -1) {
      return false;
    }
    index = found + 1;
  }
  return true;
}

test("TURN_ON_ORDER matches the generator canonical order (Requirement 3.9)", () => {
  assert.deepEqual([...TURN_ON_ORDER], [...GENERATOR_TURN_ON_ORDER]);
});

test("happy path runs every step in the fixed order and writes MCP only after the watcher", async () => {
  const { host, calls, phases } = makeHost({ dbExists: false });
  const results = await runTurnOn(host);
  assert.deepEqual(
    results.map((step) => step.phase),
    [...TURN_ON_ORDER]
  );
  assert.ok(results.every((step) => step.ok && !step.skipped));
  assert.deepEqual(executedPhases(results), [...TURN_ON_ORDER]);
  assert.ok(isSubsequence(phases.filter((p): p is string => Boolean(p)), TURN_ON_ORDER));
  assert.equal(calls.indexOf("verifyEngineBinary") < calls.indexOf("verifyModelAssets"), true);
  assert.equal(calls.indexOf("verifyModelAssets") < calls.indexOf("openOrCreateDatabase:initial"), true);
  assert.equal(
    calls.indexOf("openOrCreateDatabase:initial") < calls.indexOf("reconcileAndIndex:initial"),
    true
  );
  assert.equal(calls.indexOf("reconcileAndIndex:initial") < calls.indexOf("startWatcher"), true);
  assert.equal(calls.indexOf("startWatcher") < calls.indexOf("writeManagedMcpEntry"), true);
  assert.ok(calls.includes("isWatcherRunning"));
});

test("absent database uses initial index; existing database uses reconcile (3.3, 3.4)", async () => {
  const fresh = makeHost({ dbExists: false });
  await runTurnOn(fresh.host);
  assert.ok(fresh.calls.includes("openOrCreateDatabase:initial"));
  assert.ok(fresh.calls.includes("reconcileAndIndex:initial"));

  const existing = makeHost({ dbExists: true });
  await runTurnOn(existing.host);
  assert.ok(existing.calls.includes("openOrCreateDatabase:existing"));
  assert.ok(existing.calls.includes("reconcileAndIndex:existing"));
});

test("a failed engine binary skips every later engine process (3.1, 3.7, 21.4)", async () => {
  const { host, calls } = makeHost({
    fail: { verifyEngineBinary: "checksum mismatch" },
  });
  const results = await runTurnOn(host);
  assert.equal(results[0]?.ok, false);
  assert.equal(results[0]?.phase, "verify_engine_binary");
  assert.ok(results.slice(1).every((step) => step.skipped));
  assert.deepEqual(results.map((step) => step.phase), [...TURN_ON_ORDER]);
  assert.ok(!calls.includes("verifyModelAssets"));
  for (const step of ENGINE_STEPS) {
    assert.ok(
      !calls.some((call) => call.startsWith(step) || call.includes(step)),
      `engine step ${step} must not run after a binary failure`
    );
  }
  assert.ok(!calls.includes("startWatcher"));
  assert.ok(!calls.includes("writeManagedMcpEntry"));
  assert.ok(!calls.includes("openOrCreateDatabase:initial"));
});

test("a failed model download continues to lexical turn-on (3.2, 3.7, 3.8)", async () => {
  const { host, calls } = makeHost({
    fail: { verifyModelAssets: "model checksum mismatch" },
    dbExists: true,
  });
  const results = await runTurnOn(host);
  const model = results.find((step) => step.phase === "verify_model_assets");
  assert.equal(model?.ok, false);
  assert.equal(model?.skipped, undefined);
  assert.ok(results.filter((step) => step.phase !== "verify_model_assets").every((step) => step.ok));
  assert.ok(calls.includes("openOrCreateDatabase:existing"));
  assert.ok(calls.includes("reconcileAndIndex:existing"));
  assert.ok(calls.includes("startWatcher"));
  assert.ok(calls.includes("writeManagedMcpEntry"));
});

test("MCP entry is not written unless the watcher is running (3.5, 3.6)", async () => {
  const { host, calls } = makeHost({ watcherRunning: false });
  const results = await runTurnOn(host);
  const write = results.find((step) => step.phase === "write_mcp_entry");
  assert.equal(write?.skipped, true);
  assert.ok(!calls.includes("writeManagedMcpEntry"));
});

test("a mismatched payload is never usable (21.4)", () => {
  const payload = Buffer.from("cognis-engine-bytes");
  const digest = sha256Hex(payload);
  assert.equal(downloadedPayloadIsUsable(payload, `${digest}  cognis-bin`), true);
  assert.equal(downloadedPayloadIsUsable(payload, `${"0".repeat(64)}  cognis-bin`), false);
  assert.equal(downloadedPayloadIsUsable(payload, "not a checksum"), false);
});

test("an abort stops the sequence without writing MCP", async () => {
  const controller = new AbortController();
  const { host, calls } = makeHost({
    signal: controller.signal,
    verifyEngineBinary: async () => {
      controller.abort();
    },
  });
  await assert.rejects(
    () => runTurnOn(host),
    (error: unknown) => error instanceof Error && error.name === "AbortError"
  );
  assert.ok(!calls.includes("writeManagedMcpEntry"));
  assert.ok(!calls.includes("startWatcher"));
});

test("reconciler restores a removed entry within 5 s while the watcher runs (3.11)", async () => {
  let entry = false;
  let now = 0;
  const restored: string[] = [];
  const reconciler = new WatcherEntryReconciler({
    isWatcherRunning: () => true,
    isEntryPresent: () => entry,
    restoreEntry: async (root) => {
      restored.push(root);
      entry = true;
    },
    removeEntry: async () => {
      entry = false;
    },
    now: () => now,
    restoreBudgetMs: ENTRY_RESTORE_BUDGET_MS,
  });
  await reconciler.tick("/repo");
  assert.deepEqual(restored, []);
  now = ENTRY_RESTORE_BUDGET_MS - 1;
  await reconciler.tick("/repo");
  assert.deepEqual(restored, []);
  now = ENTRY_RESTORE_BUDGET_MS;
  await reconciler.tick("/repo");
  assert.deepEqual(restored, ["/repo"]);
  assert.equal(entry, true);
});

test("reconciler restores immediately on the next tick when no budget is set", async () => {
  let entry = false;
  const restored: string[] = [];
  const reconciler = new WatcherEntryReconciler({
    isWatcherRunning: () => true,
    isEntryPresent: () => entry,
    restoreEntry: async (root) => {
      restored.push(root);
      entry = true;
    },
    removeEntry: async () => {
      entry = false;
    },
  });
  await reconciler.tick("/repo");
  assert.deepEqual(restored, ["/repo"]);
});

test("reconciler removes the entry when the watcher stops (3.12)", async () => {
  let entry = true;
  const removed: string[] = [];
  const reconciler = new WatcherEntryReconciler({
    isWatcherRunning: () => false,
    isEntryPresent: () => entry,
    restoreEntry: async () => {},
    removeEntry: async (root) => {
      removed.push(root);
      entry = false;
    },
  });
  await reconciler.tick("/repo");
  assert.deepEqual(removed, ["/repo"]);
  assert.equal(entry, false);
});

test("reconciler is a no-op when watcher and entry already match, and during a transition", async () => {
  const ops: string[] = [];
  const matching = new WatcherEntryReconciler({
    isWatcherRunning: () => true,
    isEntryPresent: () => true,
    restoreEntry: async () => {
      ops.push("restore");
    },
    removeEntry: async () => {
      ops.push("remove");
    },
  });
  await matching.tick("/repo");

  const transitioning = new WatcherEntryReconciler({
    isWatcherRunning: () => true,
    isEntryPresent: () => false,
    restoreEntry: async () => {
      ops.push("restore");
    },
    removeEntry: async () => {
      ops.push("remove");
    },
    inTransition: () => true,
  });
  await transitioning.tick("/repo");
  assert.deepEqual(ops, []);
});
