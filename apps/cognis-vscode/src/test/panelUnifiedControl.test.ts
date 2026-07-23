// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  deriveCognisState,
  deriveCompatibilityHint,
  deriveStatusHint,
  deriveStatusLine,
  deriveUnifiedControl,
  outcomeLabelForContext,
  renderCompatibilityDetail,
  renderPanelHtml,
  type PanelContext,
} from "../panel";
import {
  evaluateHandshake,
  REQUIRED_CLI_COMMANDS,
  REQUIRED_MCP_TOOLS,
  type HandshakePayload,
  type HandshakeResult,
} from "../contract";
import {
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
} from "../compatibility";
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
  compatibility: fc.constant(FIRST_PROBE_COMPATIBILITY_SNAPSHOT),
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

test("engine-outdated mismatch overrides healthy Ready/Pause with Needs attention/Update Engine", () => {
  const mismatch = evaluateHandshake(
    {
      contract_version: 1,
      engine_version: "0.8.10",
      cli_commands: [...REQUIRED_CLI_COMMANDS],
      mcp_tools: [...REQUIRED_MCP_TOOLS],
    },
    "0.8.11"
  );
  assert.equal(mismatch.compatibility, "engine-outdated");

  const context = {
    status: "mcpEnabled",
    configured: true,
    backendAvailable: true,
    mcpEnabled: true,
    liveIndexing: true,
    syncPaused: false,
    health: {
      runtime_version: "0.8.10",
      overall: "ok",
      checks: {
        config: { status: "ok", message: "ok" },
        db: { status: "ok", message: "ok" },
        index: { status: "ok", message: "ok" },
        vector: { status: "ok", message: "ok" },
      },
    },
    indexStatus: {
      active: true,
      phase: "watching",
      message: "Watching for file changes",
      pendingCount: 0,
      pendingFiles: [],
      inflightCount: 0,
      inflightFiles: [],
      recentFiles: [],
      updatedAt: 0,
    },
    compatibility: {
      phase: "confirmed",
      generation: 1,
      observedAt: 0,
      result: mismatch,
    },
  } satisfies PanelContext & {
    compatibility: {
      phase: "confirmed";
      generation: number;
      observedAt: number;
      result: HandshakeResult;
    };
  };

  const statusLine = deriveStatusLine(context);
  const unifiedControl = deriveUnifiedControl(context);

  assert.deepEqual(
    { statusLine, unifiedControl },
    {
      statusLine: "Needs attention",
      unifiedControl: { id: "installBackend", label: "Update Engine" },
    }
  );
  assert.notEqual(statusLine, "Ready");
  assert.notEqual(unifiedControl.label, "Pause");
});

interface CompatibilityDecisionCase {
  name: string;
  result?: HandshakeResult;
  expectedKind: HandshakeResult["compatibility"] | "unavailable";
  expectedUsable: boolean | undefined;
  expectedPrimaryAction: { id: string; label: string };
  expectedStatusLine: "Ready" | "Needs attention";
  expectedStatusBar: string;
}

function completeHandshake(overrides: Partial<HandshakePayload> = {}): HandshakePayload {
  return {
    contract_version: 1,
    engine_version: "0.8.11",
    cli_commands: [...REQUIRED_CLI_COMMANDS],
    mcp_tools: [...REQUIRED_MCP_TOOLS],
    ...overrides,
  };
}

function healthyIdleContext(result?: HandshakeResult): PanelContext & {
  compatibility:
    | { phase: "confirmed"; generation: number; observedAt: number; result: HandshakeResult }
    | { phase: "unavailable"; generation: number; observedAt: number };
} {
  return {
    status: "mcpEnabled",
    configured: true,
    backendAvailable: true,
    mcpEnabled: true,
    liveIndexing: true,
    syncPaused: false,
    health: {
      runtime_version: "0.8.11",
      overall: "ok",
      checks: {
        config: { status: "ok", message: "ok" },
        db: { status: "ok", message: "ok" },
        index: { status: "ok", message: "ok" },
        vector: { status: "ok", message: "ok" },
      },
    },
    indexStatus: {
      active: true,
      phase: "watching",
      message: "Watching for file changes",
      pendingCount: 0,
      pendingFiles: [],
      inflightCount: 0,
      inflightFiles: [],
      recentFiles: [],
      updatedAt: 0,
    },
    compatibility: result
      ? { phase: "confirmed", generation: 1, observedAt: 0, result }
      : { phase: "unavailable", generation: 1, observedAt: 0 },
  };
}

const COMPATIBILITY_DECISION_CASES: CompatibilityDecisionCase[] = [
  {
    name: "ok keeps the operational control",
    result: evaluateHandshake(completeHandshake(), "0.8.11"),
    expectedKind: "ok",
    expectedUsable: true,
    expectedPrimaryAction: { id: "pauseSync", label: "Pause" },
    expectedStatusLine: "Ready",
    expectedStatusBar: "$(plug) Cognis: Ready",
  },
  {
    name: "engine-outdated updates the Engine",
    result: evaluateHandshake(
      completeHandshake({ engine_version: "0.8.10" }),
      "0.8.11"
    ),
    expectedKind: "engine-outdated",
    expectedUsable: true,
    expectedPrimaryAction: { id: "installBackend", label: "Update Engine" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "backend-older updates the Engine",
    result: evaluateHandshake(completeHandshake({ contract_version: 0 }), "0.8.11"),
    expectedKind: "backend-older",
    expectedUsable: true,
    expectedPrimaryAction: { id: "installBackend", label: "Update Engine" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "capabilities-missing updates the Engine",
    result: evaluateHandshake(
      completeHandshake({ cli_commands: REQUIRED_CLI_COMMANDS.slice(0, -1) }),
      "0.8.11"
    ),
    expectedKind: "capabilities-missing",
    expectedUsable: false,
    expectedPrimaryAction: { id: "installBackend", label: "Update Engine" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "engine-newer updates the Extension",
    result: evaluateHandshake(
      completeHandshake({ engine_version: "0.8.12" }),
      "0.8.11"
    ),
    expectedKind: "engine-newer",
    expectedUsable: true,
    expectedPrimaryAction: { id: "updateExtension", label: "Update Extension" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "backend-newer updates the Extension",
    result: evaluateHandshake(completeHandshake({ contract_version: 2 }), "0.8.11"),
    expectedKind: "backend-newer",
    expectedUsable: true,
    expectedPrimaryAction: { id: "updateExtension", label: "Update Extension" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "unreadable repairs the Engine",
    result: evaluateHandshake(
      completeHandshake({ contract_version: undefined as unknown as number }),
      "0.8.11"
    ),
    expectedKind: "unreadable",
    expectedUsable: false,
    expectedPrimaryAction: { id: "reinstallEngine", label: "Repair Engine" },
    expectedStatusLine: "Needs attention",
    expectedStatusBar: "$(warning) Cognis: Action needed",
  },
  {
    name: "unavailable without a prior mismatch keeps the operational control",
    expectedKind: "unavailable",
    expectedUsable: undefined,
    expectedPrimaryAction: { id: "pauseSync", label: "Pause" },
    expectedStatusLine: "Ready",
    expectedStatusBar: "$(plug) Cognis: Ready",
  },
];

for (const decision of COMPATIBILITY_DECISION_CASES) {
  test(`compatibility decision matrix: ${decision.name}`, () => {
    const context = healthyIdleContext(decision.result);

    assert.deepEqual(
      {
        kind: decision.result?.compatibility ?? "unavailable",
        usable: decision.result?.usable,
        primaryAction: deriveUnifiedControl(context),
        statusLine: deriveStatusLine(context),
        statusBar: outcomeLabelForContext(context),
      },
      {
        kind: decision.expectedKind,
        usable: decision.expectedUsable,
        primaryAction: decision.expectedPrimaryAction,
        statusLine: decision.expectedStatusLine,
        statusBar: decision.expectedStatusBar,
      }
    );
  });
}

test("configured + connected + live sync off offers Resume instead of Pause", () => {
  const ctx: PanelContext = {
    status: "mcpEnabled",
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
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

// ---------------------------------------------------------------------------
// deriveStatusHint / deriveCompatibilityHint — plain-language caption per
// remediation, with no raw technical value on the Minimal_Surface.
//
// Validates: Requirements 6.1, 6.2, 6.3, 6.4 (design "deriveStatusHint",
// Correctness Property 3).
//
// For each Compatibility_Kind that maps to a remediation, the "Needs attention"
// caption:
//   (a) is the correct plain-language caption for that remediation (names
//       whether the Engine or the Extension needs updating and how to proceed);
//   (b) never leaks a raw version number / URL / server id / verbatim error;
//   (c) never contains the word "Backend" or the forbidden jargon
//       (handshake / transport / socket).
// Raw versions only appear in the labeled Advanced_Surface detail.
// ---------------------------------------------------------------------------

/** The forbidden jargon list (extension-ux-coherence R9), plus "Backend". */
const FORBIDDEN_HINT_WORDS = [/backend/i, /handshake/i, /transport/i, /socket/i];

/** Assert a user-visible caption carries no raw technical value or jargon. */
function assertPlainCaption(caption: string, versions: string[]): void {
  // No raw version-like tokens (the mismatch versions in particular).
  for (const v of versions) {
    assert.ok(
      !caption.includes(v),
      `caption must not embed raw version "${v}": ${JSON.stringify(caption)}`
    );
  }
  // No dotted version number, URL, or standalone multi-digit number.
  assert.doesNotMatch(caption, /\d+\.\d+/, "caption embeds a dotted version");
  assert.doesNotMatch(caption, /https?:\/\//, "caption embeds a URL");
  assert.doesNotMatch(caption, /\d{2,}/, "caption embeds a raw number");
  // No forbidden jargon / "Backend".
  for (const re of FORBIDDEN_HINT_WORDS) {
    assert.doesNotMatch(caption, re, `caption leaks forbidden word ${re}`);
  }
}

interface HintCase {
  name: string;
  result: HandshakeResult;
  expectedHint: string;
}

const HINT_CASES: HintCase[] = [
  {
    name: "engine-outdated → Update Engine caption",
    result: evaluateHandshake(
      completeHandshake({ engine_version: "0.8.10" }),
      "0.8.11"
    ),
    expectedHint:
      "The Engine needs updating to match the Extension. Click Update Engine to continue.",
  },
  {
    name: "backend-older → Update Engine caption",
    result: evaluateHandshake(completeHandshake({ contract_version: 0 }), "0.8.11"),
    expectedHint:
      "The Engine needs updating to match the Extension. Click Update Engine to continue.",
  },
  {
    name: "capabilities-missing → Update Engine caption",
    result: evaluateHandshake(
      completeHandshake({ cli_commands: REQUIRED_CLI_COMMANDS.slice(0, -1) }),
      "0.8.11"
    ),
    expectedHint:
      "The Engine needs updating to match the Extension. Click Update Engine to continue.",
  },
  {
    name: "engine-newer → Update Extension caption",
    result: evaluateHandshake(
      completeHandshake({ engine_version: "0.8.12" }),
      "0.8.11"
    ),
    expectedHint:
      "The Extension needs updating to match the Engine. Click Update Extension to continue.",
  },
  {
    name: "backend-newer → Update Extension caption",
    result: evaluateHandshake(completeHandshake({ contract_version: 2 }), "0.8.11"),
    expectedHint:
      "The Extension needs updating to match the Engine. Click Update Extension to continue.",
  },
  {
    name: "unreadable → Repair Engine caption",
    result: evaluateHandshake(
      completeHandshake({ contract_version: undefined as unknown as number }),
      "0.8.11"
    ),
    expectedHint:
      "The Engine could not be read and needs repair. Click Repair Engine to continue.",
  },
];

for (const { name, result, expectedHint } of HINT_CASES) {
  test(`deriveStatusHint: ${name}`, () => {
    const ctx = healthyIdleContext(result);
    // Sanity: this context reads as "Needs attention" (a confirmed mismatch).
    assert.equal(deriveStatusLine(ctx), "Needs attention");

    const hint = deriveStatusHint(ctx);
    assert.equal(hint, expectedHint);
    // deriveCompatibilityHint is the pure source of the caption.
    assert.equal(deriveCompatibilityHint(result), expectedHint);

    // The caption carries no raw technical value or forbidden jargon.
    const versions = [
      result.engineVersion,
      result.expectedEngineVersion,
    ].filter((v): v is string => typeof v === "string");
    assertPlainCaption(hint, versions);
  });
}

test("deriveStatusHint: a non-compatibility Needs attention keeps the generic caption", () => {
  // A genuine health failure (not a compatibility mismatch) must NOT be given a
  // compatibility caption — the existing generic caption is preserved.
  const ctx: PanelContext = {
    status: "degraded",
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    configured: true,
    backendAvailable: true,
    mcpEnabled: true,
    liveIndexing: true,
    syncPaused: false,
    health: {
      runtime_version: "0.8.11",
      overall: "fail",
      checks: {
        config: { status: "ok", message: "ok" },
        db: { status: "fail", message: "db problem" },
        index: { status: "ok", message: "ok" },
        vector: { status: "ok", message: "ok" },
      },
    },
  };
  assert.equal(deriveStatusLine(ctx), "Needs attention");
  assert.equal(
    deriveStatusHint(ctx),
    "Something needs a look. Turn on Advanced mode (setting: cognis.advancedMode) to see details."
  );
});

test("deriveCompatibilityHint returns undefined for an ok result (no remediation)", () => {
  const okResult = evaluateHandshake(completeHandshake(), "0.8.11");
  assert.equal(okResult.compatibility, "ok");
  assert.equal(deriveCompatibilityHint(okResult), undefined);
});

// ---------------------------------------------------------------------------
// Raw versions live ONLY in the labeled Advanced_Surface detail — never on the
// Minimal_Surface (R6.2, R6.3, Correctness Property 3).
// ---------------------------------------------------------------------------

test("Minimal_Surface never leaks the raw mismatch versions (status line + hint)", () => {
  const result = evaluateHandshake(
    completeHandshake({ engine_version: "0.8.10" }),
    "0.8.11"
  );
  const ctx = healthyIdleContext(result);
  ctx.advancedMode = false;
  ctx.version = "0.8.11";

  const html = renderPanelHtml(ctx);
  // The minimal surface contains the status line + unified control only; the
  // raw versions must not appear anywhere in it.
  assert.ok(!html.includes("0.8.10"), "minimal surface leaked raw Engine version");
  // The status line + hint themselves carry no raw version.
  assertPlainCaption(deriveStatusHint(ctx), ["0.8.10", "0.8.11"]);
});

test("Advanced_Surface exposes the raw versions in a labeled detail area", () => {
  const result = evaluateHandshake(
    completeHandshake({ engine_version: "0.8.10" }),
    "0.8.11"
  );
  const ctx = healthyIdleContext(result);
  ctx.advancedMode = true;
  ctx.version = "0.8.11";

  const detail = renderCompatibilityDetail(ctx);
  // Labeled, and carries the raw engine version separate from the caption.
  assert.match(detail, /Compatibility/);
  assert.match(detail, /Engine version/);
  assert.ok(detail.includes("0.8.10"), "labeled detail must show the raw Engine version");

  // And it shows up in the full advanced render.
  const html = renderPanelHtml(ctx);
  assert.ok(html.includes("0.8.10"), "advanced surface should surface the raw Engine version");
});

test("renderCompatibilityDetail is empty when there is no confirmed mismatch", () => {
  const ctx = healthyIdleContext(evaluateHandshake(completeHandshake(), "0.8.11"));
  ctx.advancedMode = true;
  assert.equal(renderCompatibilityDetail(ctx), "");
});
