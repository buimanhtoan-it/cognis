/**
 * Deactivation_Manager turn-off order, signal-then-terminate, owned-only
 * targeting, and database preservation (task 16.6).
 *
 * Validates: Requirements 4.1, 4.2, 4.3, 4.4, 4.5, 4.7, 4.8, 4.9, 21.8
 */
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import {
  DAEMON_EXIT_BUDGET_MS,
  TURN_OFF_ORDER,
  runTurnOff,
  type DeactivationHost,
  type OwnedDaemon,
} from "../deactivation";
import { TURN_OFF_ORDER as GENERATOR_TURN_OFF_ORDER } from "./generators";

function makeHost(
  overrides: Partial<DeactivationHost> & {
    daemons?: OwnedDaemon[];
    alive?: Set<number>;
  } = {}
): {
  host: DeactivationHost;
  phases: string[];
  calls: string[];
  signaled: number[];
  terminated: number[];
  logged: number[];
} {
  const phases: string[] = [];
  const calls: string[] = [];
  const signaled: number[] = [];
  const terminated: number[] = [];
  const logged: number[] = [];
  const alive = overrides.alive ?? new Set<number>();
  const daemons = overrides.daemons ?? [];

  const host: DeactivationHost = {
    signal: overrides.signal ?? new AbortController().signal,
    onPhase(phase) {
      phases.push(phase ?? "idle");
    },
    async stopWatcher() {
      calls.push("stopWatcher");
    },
    listOwnedDaemons() {
      calls.push("listOwnedDaemons");
      return [...daemons];
    },
    signalStop(daemon) {
      calls.push(`signal:${daemon.pid}`);
      signaled.push(daemon.pid);
    },
    isAlive(daemon) {
      return alive.has(daemon.pid);
    },
    terminate(daemon) {
      calls.push(`terminate:${daemon.pid}`);
      terminated.push(daemon.pid);
      alive.delete(daemon.pid);
    },
    logTermination(daemon) {
      logged.push(daemon.pid);
    },
    async removeManagedMcpEntries() {
      calls.push("removeManagedMcpEntries");
    },
    async sleep() {
      // Tests inject fake time: default no-op so the 10 s budget is instant.
    },
    ...overrides,
  };

  return { host, phases, calls, signaled, terminated, logged };
}

test("turn-off order matches the spec and the generator constant", () => {
  assert.deepEqual([...TURN_OFF_ORDER], [...GENERATOR_TURN_OFF_ORDER]);
  assert.deepEqual([...TURN_OFF_ORDER], [
    "stop_watcher",
    "stop_daemons",
    "remove_mcp_entry",
  ]);
});

test("runTurnOff stops watcher, then owned daemons, then MCP entries", async () => {
  const { host, calls, phases } = makeHost();
  const results = await runTurnOff(host);
  assert.deepEqual(
    results.map((step) => step.phase),
    [...TURN_OFF_ORDER]
  );
  assert.ok(results.every((step) => step.ok));
  assert.deepEqual(
    calls.filter((name) =>
      ["stopWatcher", "listOwnedDaemons", "removeManagedMcpEntries"].includes(
        name
      )
    ),
    ["stopWatcher", "listOwnedDaemons", "removeManagedMcpEntries"]
  );
  assert.ok(phases.indexOf("stop_watcher") < phases.indexOf("stop_daemons"));
  assert.ok(phases.indexOf("stop_daemons") < phases.indexOf("remove_mcp_entry"));
});

test("a daemon that exits after the stop signal is not terminated", async () => {
  const daemon: OwnedDaemon = { pid: 42, role: "indexd" };
  const { host, signaled, terminated, logged } = makeHost({
    daemons: [daemon],
    signalStop(next) {
      signaled.push(next.pid);
    },
    isAlive() {
      return false;
    },
  });
  await runTurnOff(host);
  assert.deepEqual(signaled, [42]);
  assert.deepEqual(terminated, []);
  assert.deepEqual(logged, []);
});

test("a daemon still alive after 10s is terminated and logged", async () => {
  const daemon: OwnedDaemon = { pid: 7, role: "mcpd" };
  const slept: number[] = [];
  const { host, signaled, terminated, logged } = makeHost({
    daemons: [daemon],
    alive: new Set([7]),
    async sleep(ms) {
      slept.push(ms);
    },
  });
  await runTurnOff(host);
  assert.deepEqual(signaled, [7]);
  assert.deepEqual(terminated, [7]);
  assert.deepEqual(logged, [7]);
  assert.equal(DAEMON_EXIT_BUDGET_MS, 10_000);
  assert.equal(
    slept.reduce((sum, slice) => sum + slice, 0),
    DAEMON_EXIT_BUDGET_MS
  );
});

test("foreign processes are never signaled or terminated", async () => {
  const foreign = 999;
  const owned: OwnedDaemon = { pid: 11, role: "mcpd" };
  const seen: number[] = [];
  const { host, signaled, terminated } = makeHost({
    daemons: [owned],
    alive: new Set([owned.pid, foreign]),
    signalStop(daemon) {
      seen.push(daemon.pid);
      signaled.push(daemon.pid);
    },
    terminate(daemon) {
      seen.push(daemon.pid);
      terminated.push(daemon.pid);
    },
  });
  await runTurnOff(host);
  assert.ok(!seen.includes(foreign));
  assert.ok(!signaled.includes(foreign));
  assert.ok(!terminated.includes(foreign));
});

test("turn-off never deletes or rewrites the workspace database", async () => {
  const db = { bytes: Buffer.from("uckg-contents") };
  const { host } = makeHost({
    async stopWatcher() {},
    async removeManagedMcpEntries() {},
  });
  const before = Buffer.from(db.bytes);
  await runTurnOff(host);
  assert.ok(before.equals(db.bytes));
});

test("abort before work throws and does not remove MCP entries", async () => {
  const controller = new AbortController();
  controller.abort();
  const calls: string[] = [];
  const { host } = makeHost({
    signal: controller.signal,
    async stopWatcher() {
      calls.push("stopWatcher");
    },
    async removeManagedMcpEntries() {
      calls.push("removeManagedMcpEntries");
    },
  });
  await assert.rejects(() => runTurnOff(host), { name: "AbortError" });
  assert.deepEqual(calls, []);
});
