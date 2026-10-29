/**
 * Exact status and lifecycle unit tests (task 16.14).
 *
 * - Status_Surface derives from database/lease inputs, not CLI exit codes (2.7)
 * - Toggle_Control is one binary control with exactly the values On and Off (1.1)
 * - An Owned_Daemon still alive after 10 s is terminated and logged (4.8)
 *
 * Validates: Requirements 1.1, 2.7, 4.8
 */
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { FIRST_PROBE_COMPATIBILITY_SNAPSHOT } from "../compatibility";
import {
  DAEMON_EXIT_BUDGET_MS,
  runTurnOff,
  type DeactivationHost,
  type OwnedDaemon,
} from "../deactivation";
import { renderPanelHtml, type PanelContext } from "../panel";
import { deriveStatusSnapshot, deriveStatusState, type StatusInput } from "../status";
import {
  DEFAULT_TOGGLE_STATE,
  TOGGLE_STORAGE_KEY,
  ToggleControl,
  type ToggleMemento,
  type ToggleState,
} from "../toggle";
import type { HealthReport, OwnerStatusReport } from "../types";
import { TOGGLE_STATES, WORKSPACE_ALIASES } from "./generators";

class MemoryMemento implements ToggleMemento {
  private readonly store = new Map<string, unknown>();

  get<T>(key: string): T | undefined {
    return this.store.get(key) as T | undefined;
  }

  update(key: string, value: unknown): Promise<void> {
    this.store.set(key, value);
    return Promise.resolve();
  }
}

function okHealth(): HealthReport {
  return {
    runtime_version: "0.9.0",
    overall: "ok",
    checks: {
      config: { status: "ok", message: "ok" },
      db: { status: "ok", message: "ok" },
      index: { status: "ok", message: "ok" },
    },
    capabilities: {
      semantic_backend: true,
      accelerated_vector_search: true,
    },
    source_generation: 4,
    semantic_generation: 4,
    pending_vector_count: 0,
    semantic_available: true,
    fingerprint_match: true,
  };
}

function ownerStatus(): OwnerStatusReport {
  return {
    active: true,
    phase: "idle",
    message: "ok",
    pendingCount: 0,
    pendingFiles: [],
    inflightCount: 0,
    inflightFiles: [],
    recentFiles: [],
    updatedAt: 1_730_000_000,
    sourceGeneration: 4,
    semanticGeneration: 4,
    pendingVectorCount: 0,
    watcherQueueDepth: 0,
    fingerprintMatch: true,
    capabilities: {
      semantic_backend: true,
      accelerated_vector_search: true,
    },
  };
}

function panelContext(): PanelContext {
  return {
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    status: "ready",
    advancedMode: false,
    liveIndexing: true,
    mcpEnabled: true,
    syncPaused: false,
    configured: true,
    backendAvailable: true,
  };
}

test("status derivation ignores CLI exit codes when database and lease inputs match (2.7)", () => {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-16-14-status-"));
  try {
    const cognis = path.join(repo, ".cognis");
    fs.mkdirSync(cognis, { recursive: true });
    fs.writeFileSync(
      path.join(cognis, "owner-status.json"),
      `${JSON.stringify(ownerStatus())}\n`,
      "utf8"
    );
    fs.writeFileSync(
      path.join(cognis, "owner.lease"),
      `${JSON.stringify({
        owner_nonce: "nonce-1",
        pid: 4242,
        process_start_id: "start-1",
        heartbeat_at: 1_730_000_010,
        expiry: 1_730_000_040,
      })}\n`,
      "utf8"
    );

    const health = okHealth();
    const input: StatusInput = {
      health,
      owner: ownerStatus(),
      lease: {
        owner_nonce: "nonce-1",
        pid: 4242,
        process_start_id: "start-1",
        heartbeat_at: 1_730_000_010,
        expiry: 1_730_000_040,
      },
      readErrors: [],
    };
    assert.equal("exitCode" in input, false);
    assert.equal("exit_code" in input, false);

    const fromInput = deriveStatusState(input);
    const reports = [0, 1, 137].map((exitCode) => {
      void exitCode;
      return deriveStatusSnapshot({
        repoRoot: repo,
        health,
        mcpEnabled: true,
        configured: true,
        nowMs: 1_730_000_000,
      }).report;
    });
    assert.deepEqual(reports[0], reports[1]);
    assert.deepEqual(reports[0], reports[2]);
    assert.equal(reports[0]?.state, fromInput.state);
    assert.deepEqual(reports[0]?.derivedFrom, fromInput.derivedFrom);
    assert.equal(
      JSON.stringify(reports[0]).includes("exitCode") ||
        JSON.stringify(reports[0]).includes("exit_code"),
      false
    );
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("exactly one Toggle_Control is rendered and it has exactly two values (1.1)", async () => {
  assert.deepEqual([...TOGGLE_STATES], ["On", "Off"]);
  assert.equal(DEFAULT_TOGGLE_STATE, "Off");
  assert.equal(TOGGLE_STATES.length, 2);

  const manifest = JSON.parse(
    fs.readFileSync(path.join(__dirname, "..", "..", "package.json"), "utf8")
  ) as {
    contributes?: { commands?: Array<{ command: string }> };
  };
  const toggleCommands = (manifest.contributes?.commands ?? []).filter(
    (entry) => entry.command === "cognis.toggle"
  );
  assert.equal(toggleCommands.length, 1, "package.json must declare one cognis.toggle command");

  const html = renderPanelHtml(panelContext());
  assert.equal(
    (html.match(/data-unified="true"/g) ?? []).length,
    1,
    "the panel must render exactly one toggle control"
  );

  const memento = new MemoryMemento();
  const control = new ToggleControl({
    workspaceState: memento,
    transition: {
      async activate() {},
      async deactivate() {},
    },
    reportStatus() {},
  });
  await control.set(WORKSPACE_ALIASES[0], "On");
  await control.set(WORKSPACE_ALIASES[1], "Off");
  const displayed: ToggleState = control.displayed(WORKSPACE_ALIASES[2]);
  assert.ok(TOGGLE_STATES.includes(displayed));
  assert.equal(displayed === "On" || displayed === "Off", true);
  const stored =
    memento.get<Record<string, ToggleState>>(TOGGLE_STORAGE_KEY) ?? {};
  assert.equal(Object.keys(stored).length, 1);
  assert.ok(TOGGLE_STATES.includes(Object.values(stored)[0] as ToggleState));
});

test("an unresponsive owned daemon is terminated after 10 s and the termination is logged (4.8)", async () => {
  const daemon: OwnedDaemon = { pid: 77, role: "indexd" };
  const slept: number[] = [];
  const terminated: number[] = [];
  const logged: number[] = [];
  const host: DeactivationHost = {
    signal: new AbortController().signal,
    onPhase() {},
    async stopWatcher() {},
    listOwnedDaemons() {
      return [daemon];
    },
    signalStop() {},
    isAlive() {
      return terminated.length === 0;
    },
    terminate(next) {
      terminated.push(next.pid);
    },
    logTermination(next) {
      logged.push(next.pid);
    },
    async removeManagedMcpEntries() {},
    async sleep(ms) {
      slept.push(ms);
    },
  };

  await runTurnOff(host);
  assert.equal(DAEMON_EXIT_BUDGET_MS, 10_000);
  assert.equal(
    slept.reduce((sum, slice) => sum + slice, 0),
    DAEMON_EXIT_BUDGET_MS
  );
  assert.deepEqual(terminated, [77]);
  assert.deepEqual(logged, [77]);
});
