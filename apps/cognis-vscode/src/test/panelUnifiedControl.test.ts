// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  deriveCognisState,
  deriveUnifiedControl,
  type PanelContext,
} from "../panel";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-minimal-panel, Property 2: Nhãn và lệnh của Unified_Control
// khớp Cognis_State và luôn không phá hủy.
//
// Validates: Requirements 1.2, 1.3, 1.4, 1.8, 8.4
//
// For any PanelContext, let s = deriveCognisState(ctx); the derived
// UnifiedControl satisfies: s=off → label "Start Cognis" & id "startCognis";
// s=running → label "Pause" & id "pauseSync"; s=paused → label "Resume" & id
// "resumeSync"; and in all cases the id is within the Non_Destructive_Action
// set {startCognis, pauseSync, resumeSync} (never a Destructive_Action).
// ---------------------------------------------------------------------------

/** The complete set of workspace statuses (from `types.ts`). */
const WORKSPACE_STATUSES: WorkspaceStatus[] = [
  "notInstalled",
  "indexing",
  "ready",
  "mcpEnabled",
  "degraded",
  "unknown",
];

/** Non_Destructive_Action set the Unified_Control must always resolve to. */
const NON_DESTRUCTIVE_IDS = new Set(["startCognis", "pauseSync", "resumeSync"]);

/** Expected (label, id) for each derived Cognis_State. */
const EXPECTED_CONTROL: Record<string, { id: string; label: string }> = {
  off: { id: "startCognis", label: "Start Cognis" },
  running: { id: "pauseSync", label: "Pause" },
  paused: { id: "resumeSync", label: "Resume" },
};

const arbHealth: fc.Arbitrary<HealthReport | undefined> = fc.option(
  fc.record({
    runtime_version: fc.constantFrom("0.3.2", "0.7.1", "1.0.0"),
    overall: fc.constantFrom<"ok" | "warn" | "fail">("ok", "warn", "fail"),
    checks: fc.constant({
      config: { status: "ok", message: "ok" },
      db: { status: "ok", message: "ok" },
      index: { status: "ok", message: "ok" },
      vector: { status: "ok", message: "ok" },
      embedder: { status: "ok", message: "ok" },
      version: { status: "ok", message: "ok" },
    }),
  }),
  { nil: undefined }
);

const arbIndexStatus: fc.Arbitrary<IndexStatusReport | undefined> = fc.option(
  fc.record({
    active: fc.boolean(),
    // Include steady-state phases ("watching"/"idle"/"stopped") and active
    // work phases so both busy and non-busy index states are generated.
    phase: fc.constantFrom(
      "watching",
      "idle",
      "stopped",
      "indexing",
      "embedding",
      "scanning"
    ),
    message: fc.constantFrom("", "Watching for file changes", "Indexing…"),
    pendingCount: fc.nat({ max: 5 }),
    pendingFiles: fc.constant([] as string[]),
    inflightCount: fc.nat({ max: 5 }),
    inflightFiles: fc.constant([] as string[]),
    recentFiles: fc.constant([] as string[]),
    updatedAt: fc.constant(0),
  }),
  { nil: undefined }
);

const arbPrerequisites: fc.Arbitrary<PrerequisiteReport | undefined> = fc.option(
  fc.record({
    ready: fc.boolean(),
    combined_install_target: fc.constant(""),
    items: fc.constant([] as PrerequisiteReport["items"]),
  }),
  { nil: undefined }
);

/**
 * Generates random PanelContext values across both modes and every combination
 * of status / health / flags, and optionally injects raw technical values
 * (mcpServerUrl/name/error/configPath, port) to exercise the full input space.
 */
const arbPanelContext: fc.Arbitrary<PanelContext> = fc.record({
  status: fc.constantFrom(...WORKSPACE_STATUSES),
  advancedMode: fc.boolean(),
  liveIndexing: fc.boolean(),
  mcpEnabled: fc.boolean(),
  syncPaused: fc.boolean(),
  configured: fc.boolean(),
  backendAvailable: fc.boolean(),
  health: arbHealth,
  indexStatus: arbIndexStatus,
  prerequisites: arbPrerequisites,
  mcpServerName: fc.option(fc.constantFrom("cognis-workspace-ab12cd"), {
    nil: undefined,
  }),
  mcpServerUrl: fc.option(fc.constantFrom("http://127.0.0.1:50001/mcp"), {
    nil: undefined,
  }),
  mcpServerError: fc.option(fc.constantFrom("cognis-mcpd exited with code=1"), {
    nil: undefined,
  }),
  mcpConfigPath: fc.option(fc.constantFrom("/repo/.cursor/mcp.json"), {
    nil: undefined,
  }),
});

test("configured + connected + live sync off offers Resume instead of Pause", () => {
  const ctx: PanelContext = {
    status: "mcpEnabled",
    configured: true,
    mcpEnabled: true,
    liveIndexing: false,
    syncPaused: false,
  };

  assert.equal(deriveCognisState(ctx), "paused");
  assert.deepEqual(deriveUnifiedControl(ctx), {
    id: "resumeSync",
    label: "Resume",
  });
});

test("Property 2: Unified_Control label + action match Cognis_State and are always non-destructive", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const state = deriveCognisState(ctx);
      const unified = deriveUnifiedControl(ctx);

      // The derived control must match the fixed mapping for the state.
      const expected = EXPECTED_CONTROL[state];
      assert.ok(expected, `unexpected Cognis_State: ${state}`);
      assert.equal(unified.id, expected.id);
      assert.equal(unified.label, expected.label);

      // In all cases the id stays within the Non_Destructive_Action set and is
      // therefore never a Destructive_Action.
      assert.ok(
        NON_DESTRUCTIVE_IDS.has(unified.id),
        `Unified_Control id "${unified.id}" is not a Non_Destructive_Action`
      );
    }),
    { numRuns: 300 }
  );
});
