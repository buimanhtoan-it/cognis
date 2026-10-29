/**
 * Property 30: Turn-on order is preserved (task 16.11).
 *
 * For any starting state (database present or not, watcher running or not,
 * any step failing or aborting), the turn-on step trace `runTurnOn` emits is
 * a subsequence of the canonical Activation_Manager order (Requirement 3.9):
 * verify Engine_Binary → verify Model_Assets → open/create Workspace_Database
 * → reconcile and index → start watcher → write Managed_MCP_Entry.
 *
 * Validates: Requirements 3.9
 * Properties: P30
 */
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  TURN_ON_ORDER,
  runTurnOn,
  type ActivationHost,
  type ActivationStepResult,
} from "../activation";
import { PROPERTY_RUNS, TURN_ON_ORDER as GENERATOR_TURN_ON_ORDER } from "./generators";

type HostStep =
  | "verifyEngineBinary"
  | "verifyModelAssets"
  | "openOrCreateDatabase"
  | "reconcileAndIndex"
  | "startWatcher"
  | "writeManagedMcpEntry";

const HOST_ORDER: readonly HostStep[] = [
  "verifyEngineBinary",
  "verifyModelAssets",
  "openOrCreateDatabase",
  "reconcileAndIndex",
  "startWatcher",
  "writeManagedMcpEntry",
];

interface StartingState {
  dbExists: boolean;
  watcherRunning: boolean;
  failing: Record<HostStep, boolean>;
  abortAt: HostStep | undefined;
}

const arbFailing = fc.record({
  verifyEngineBinary: fc.boolean(),
  verifyModelAssets: fc.boolean(),
  openOrCreateDatabase: fc.boolean(),
  reconcileAndIndex: fc.boolean(),
  startWatcher: fc.boolean(),
  writeManagedMcpEntry: fc.boolean(),
});

const noneFailing: Record<HostStep, boolean> = {
  verifyEngineBinary: false,
  verifyModelAssets: false,
  openOrCreateDatabase: false,
  reconcileAndIndex: false,
  startWatcher: false,
  writeManagedMcpEntry: false,
};

const arbStartingState: fc.Arbitrary<StartingState> = fc.oneof(
  {
    weight: 2,
    arbitrary: fc.record({
      dbExists: fc.boolean(),
      watcherRunning: fc.boolean(),
      failing: fc.constant(noneFailing),
      abortAt: fc.constant(undefined),
    }),
  },
  {
    weight: 3,
    arbitrary: fc
      .tuple(fc.boolean(), fc.boolean(), fc.constantFrom(...HOST_ORDER))
      .map(([dbExists, watcherRunning, failed]) => ({
        dbExists,
        watcherRunning,
        failing: { ...noneFailing, [failed]: true },
        abortAt: undefined,
      })),
  },
  {
    weight: 2,
    arbitrary: fc.record({
      dbExists: fc.boolean(),
      watcherRunning: fc.boolean(),
      failing: arbFailing,
      abortAt: fc.option(fc.constantFrom(...HOST_ORDER), { nil: undefined }),
    }),
  },
  {
    weight: 1,
    arbitrary: fc
      .tuple(fc.boolean(), fc.boolean(), fc.constantFrom(...HOST_ORDER))
      .map(([dbExists, watcherRunning, abortAt]) => ({
        dbExists,
        watcherRunning,
        failing: noneFailing,
        abortAt,
      })),
  }
);

function isSubsequence(trace: readonly string[], canonical: readonly string[]): boolean {
  let index = 0;
  for (const item of trace) {
    const found = canonical.indexOf(item, index);
    if (found === -1) {
      return false;
    }
    index = found + 1;
  }
  return true;
}

function makeHost(state: StartingState): {
  host: ActivationHost;
  calls: HostStep[];
  phases: Array<string | undefined>;
} {
  const calls: HostStep[] = [];
  const phases: Array<string | undefined> = [];
  const controller = new AbortController();

  const run = async (name: HostStep): Promise<void> => {
    calls.push(name);
    if (state.abortAt === name) {
      controller.abort();
      const error = new Error("Activation aborted");
      error.name = "AbortError";
      throw error;
    }
    if (state.failing[name]) {
      throw new Error(`${name} failed`);
    }
  };

  const host: ActivationHost = {
    signal: controller.signal,
    onPhase(phase) {
      phases.push(phase);
    },
    verifyEngineBinary: () => run("verifyEngineBinary"),
    verifyModelAssets: () => run("verifyModelAssets"),
    databaseExists() {
      return state.dbExists;
    },
    openOrCreateDatabase: () => run("openOrCreateDatabase"),
    reconcileAndIndex: () => run("reconcileAndIndex"),
    startWatcher: () => run("startWatcher"),
    isWatcherRunning() {
      return state.watcherRunning;
    },
    writeManagedMcpEntry: () => run("writeManagedMcpEntry"),
  };
  return { host, calls, phases };
}

function emittedPhases(phases: Array<string | undefined>): string[] {
  return phases.filter((phase): phase is string => typeof phase === "string");
}

test("Property 30: turn-on step trace is a subsequence of TURN_ON_ORDER", async () => {
  assert.deepEqual([...TURN_ON_ORDER], [...GENERATOR_TURN_ON_ORDER]);

  await fc.assert(
    fc.asyncProperty(arbStartingState, async (state) => {
      const { host, calls, phases } = makeHost(state);
      let results: ActivationStepResult[] | undefined;
      try {
        results = await runTurnOn(host);
      } catch (error) {
        assert.equal(
          error instanceof Error && error.name === "AbortError",
          true,
          "non-abort failures must be degraded, not thrown"
        );
      }

      const trace = emittedPhases(phases);
      assert.equal(
        isSubsequence(trace, TURN_ON_ORDER),
        true,
        `onPhase trace ${JSON.stringify(trace)} is not a subsequence of ${JSON.stringify(TURN_ON_ORDER)}`
      );
      assert.equal(
        isSubsequence(calls, HOST_ORDER),
        true,
        `host calls ${JSON.stringify(calls)} are not a subsequence of ${JSON.stringify(HOST_ORDER)}`
      );

      if (results === undefined) {
        return;
      }

      const resultPhases = results.map((step) => step.phase);
      assert.deepEqual(resultPhases, [...TURN_ON_ORDER]);
      const executed = results.filter((step) => !step.skipped).map((step) => step.phase);
      assert.equal(isSubsequence(executed, TURN_ON_ORDER), true);
      assert.equal(isSubsequence(trace, executed), true);
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
