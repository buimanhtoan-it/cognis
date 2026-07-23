// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  ACTION_COMMANDS,
  derivePanelView,
  deriveStatusHint,
  deriveStatusLine,
  deriveUnifiedControl,
  outcomeLabelForContext,
  renderPanelHtml,
  type PanelContext,
  type StatusLineText,
} from "../panel";
import { isIndexStatusBusy } from "../state";
import {
  compatibilitySnapshotFromHandshake,
  isConfirmedMismatch,
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  type CompatibilitySnapshot,
} from "../compatibility";
import { evaluateHandshake, REQUIRED_CLI_COMMANDS, REQUIRED_MCP_TOOLS } from "../contract";
import type { ContractCompatibility, HandshakeResult } from "../contract";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-engine-compatibility-ux — Correctness Properties 1–4, 7.
//
// These property-based tests extend the existing PanelContext arbitraries with
// a compatibility snapshot (confirmed mismatches across ALL Compatibility_Kind
// values, plus ok/checking/unavailable) and verify:
//
//   Property 1 (No false Ready): for every PanelContext with a
//     Confirmed_Mismatch and indexing not busy, deriveStatusLine returns
//     "Needs attention" and deriveUnifiedControl returns a
//     Compatibility_Primary_Action. (R1.4, R1.5, R3)
//   Property 2 (Exactly one primary control): for every PanelContext, the
//     rendered HTML contains exactly one [data-unified] element, including when
//     a mismatch replaces Start/Pause/Resume, on BOTH advancedMode true/false.
//     (R4)
//   Property 3 (No raw data on Minimal Surface): for every snapshot, the
//     Minimal_Surface Status Line and hint belong to the closed vocabulary and
//     contain no raw version/URL/id/error. (R5)
//   Property 4 (Remediation resolves command): every data-action of a
//     Compatibility_Primary_Action resolves to a command registered in
//     ACTION_COMMANDS (and thus a cognis.* command). (R3, R6)
//   Property 7 (Terminology): user-visible text (Status Line, Unified_Control
//     label, Status Bar) uses Engine/Extension, never "Backend", and never the
//     forbidden jargon (handshake/transport/socket). (R5, R6)
//
// Validates: Requirements 1.4, 1.5, 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8,
// 3.9, 4.1, 4.2, 4.3, 4.4, 4.5, 5.1, 5.2, 5.3, 5.4, 5.5
// ---------------------------------------------------------------------------

const CODEBASE_NUM_RUNS = 300;

/** The complete set of workspace statuses (from `types.ts`). */
const WORKSPACE_STATUSES: WorkspaceStatus[] = [
  "notInstalled",
  "indexing",
  "ready",
  "mcpEnabled",
  "degraded",
  "unknown",
];

/** Every non-`ok` Compatibility_Kind — the confirmed-mismatch cases. */
const MISMATCH_KINDS: Exclude<ContractCompatibility, "ok">[] = [
  "backend-older",
  "backend-newer",
  "engine-outdated",
  "engine-newer",
  "capabilities-missing",
  "unreadable",
];

/** The three permitted Compatibility_Primary_Action ids (R3.7). */
const COMPATIBILITY_PRIMARY_IDS = new Set([
  "installBackend",
  "updateExtension",
  "reinstallEngine",
]);

/** The closed Status_Line vocabulary. */
const CLOSED_STATUS_VOCAB = new Set<string>([
  "Ready",
  "Working",
  "Paused",
  "Off",
  "Needs attention",
]);

/** Forbidden jargon list (extension-ux-coherence R9) plus "Backend". */
const FORBIDDEN_WORDS = [/backend/i, /handshake/i, /transport/i, /socket/i];

// ---------------------------------------------------------------------------
// Handshake result builders — one per Compatibility_Kind. Each produces a real
// HandshakeResult via evaluateHandshake so the mismatch verdicts are faithful
// (not hand-forged), exercising the actual contract logic.
// ---------------------------------------------------------------------------

function makeHandshakeResult(kind: ContractCompatibility): HandshakeResult {
  const full = {
    contract_version: 1,
    engine_version: "0.8.11",
    cli_commands: [...REQUIRED_CLI_COMMANDS],
    mcp_tools: [...REQUIRED_MCP_TOOLS],
  };
  switch (kind) {
    case "ok":
      return evaluateHandshake(full, "0.8.11");
    case "engine-outdated":
      return evaluateHandshake({ ...full, engine_version: "0.8.10" }, "0.8.11");
    case "engine-newer":
      return evaluateHandshake({ ...full, engine_version: "0.8.12" }, "0.8.11");
    case "backend-older":
      return evaluateHandshake({ ...full, contract_version: 0 }, "0.8.11");
    case "backend-newer":
      return evaluateHandshake({ ...full, contract_version: 2 }, "0.8.11");
    case "capabilities-missing":
      return evaluateHandshake(
        { ...full, cli_commands: REQUIRED_CLI_COMMANDS.slice(0, -1) },
        "0.8.11"
      );
    case "unreadable":
      return evaluateHandshake(
        { ...full, contract_version: undefined as unknown as number },
        "0.8.11"
      );
  }
}

// Cache the results so the arbitrary is cheap; verify each builder produces the
// intended kind up front (a guard so a contract change is caught here).
const HANDSHAKE_BY_KIND: Record<ContractCompatibility, HandshakeResult> = {
  ok: makeHandshakeResult("ok"),
  "engine-outdated": makeHandshakeResult("engine-outdated"),
  "engine-newer": makeHandshakeResult("engine-newer"),
  "backend-older": makeHandshakeResult("backend-older"),
  "backend-newer": makeHandshakeResult("backend-newer"),
  "capabilities-missing": makeHandshakeResult("capabilities-missing"),
  unreadable: makeHandshakeResult("unreadable"),
};

test("compatibility builders produce the intended kind (arbitrary sanity guard)", () => {
  for (const kind of Object.keys(HANDSHAKE_BY_KIND) as ContractCompatibility[]) {
    assert.equal(
      HANDSHAKE_BY_KIND[kind].compatibility,
      kind,
      `builder for "${kind}" produced "${HANDSHAKE_BY_KIND[kind].compatibility}"`
    );
  }
});

/**
 * A compatibility snapshot arbitrary spanning all seven Compatibility_Kind
 * confirmed verdicts plus the non-confirmed phases:
 *   - confirmed `ok`
 *   - confirmed mismatch (each of the six non-ok kinds)
 *   - checking
 *   - unavailable
 */
const arbCompatibilitySnapshot: fc.Arbitrary<CompatibilitySnapshot> = fc.oneof(
  // Confirmed verdicts (ok + every mismatch kind), with varied generation/time.
  fc
    .tuple(
      fc.constantFrom<ContractCompatibility>(
        "ok",
        ...MISMATCH_KINDS
      ),
      fc.integer({ min: 1, max: 1000 }),
      fc.integer({ min: 1, max: 2_000_000_000 })
    )
    .map(([kind, generation, observedAt]) =>
      compatibilitySnapshotFromHandshake(
        HANDSHAKE_BY_KIND[kind],
        generation,
        observedAt
      )
    ),
  // Checking phase.
  fc
    .tuple(fc.integer({ min: 1, max: 1000 }), fc.integer({ min: 0, max: 2_000_000_000 }))
    .map(
      ([generation, observedAt]): CompatibilitySnapshot => ({
        phase: "checking",
        generation,
        observedAt,
      })
    ),
  // Unavailable phase (including the frozen first-probe default).
  fc.constant<CompatibilitySnapshot>(FIRST_PROBE_COMPATIBILITY_SNAPSHOT),
  fc
    .tuple(fc.integer({ min: 1, max: 1000 }), fc.integer({ min: 0, max: 2_000_000_000 }))
    .map(
      ([generation, observedAt]): CompatibilitySnapshot => ({
        phase: "unavailable",
        generation,
        observedAt,
      })
    )
);

/** A confirmed-mismatch-only snapshot arbitrary (for Property 1). */
const arbConfirmedMismatchSnapshot: fc.Arbitrary<CompatibilitySnapshot> = fc
  .tuple(
    fc.constantFrom(...MISMATCH_KINDS),
    fc.integer({ min: 1, max: 1000 }),
    fc.integer({ min: 1, max: 2_000_000_000 })
  )
  .map(([kind, generation, observedAt]) =>
    compatibilitySnapshotFromHandshake(HANDSHAKE_BY_KIND[kind], generation, observedAt)
  );

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

/**
 * A NON-busy index status arbitrary: never `active` with in-flight/pending
 * work and always a steady-state phase, so `isIndexStatusBusy` is guaranteed
 * false (Property 1 requires "indexing not busy").
 */
const arbNonBusyIndexStatus: fc.Arbitrary<IndexStatusReport | undefined> = fc.option(
  fc.record({
    active: fc.constant(false),
    phase: fc.constantFrom("watching", "idle", "stopped"),
    message: fc.constantFrom("", "Watching for file changes"),
    pendingCount: fc.constant(0),
    pendingFiles: fc.constant([] as string[]),
    inflightCount: fc.constant(0),
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

// Raw technical values injected so the "no raw data" assertions are meaningful.
const RAW_MCP_URL = "http://127.0.0.1:50001/mcp";
const RAW_MCP_NAME = "cognis-workspace-ab12cd";
const RAW_MCP_ERROR = "cognis-mcpd exited with code=1 on port 50001";
const RAW_MCP_CONFIG_PATH = "/repo/.cursor/mcp.json";

/**
 * A general PanelContext arbitrary carrying the full compatibility snapshot
 * space (all kinds + phases), across both advancedMode values and every
 * status / health / flag combination, with raw technical values injected.
 */
const arbPanelContext: fc.Arbitrary<PanelContext> = fc.record({
  compatibility: arbCompatibilitySnapshot,
  status: fc.constantFrom(...WORKSPACE_STATUSES),
  advancedMode: fc.boolean(),
  liveIndexing: fc.boolean(),
  mcpEnabled: fc.boolean(),
  syncPaused: fc.boolean(),
  configured: fc.boolean(),
  backendAvailable: fc.boolean(),
  version: fc.constantFrom("0.8.11", "0.8.10", "0.8.12"),
  health: arbHealth,
  indexStatus: arbIndexStatus,
  prerequisites: arbPrerequisites,
  mcpServerName: fc.option(fc.constantFrom(RAW_MCP_NAME), { nil: undefined }),
  mcpServerUrl: fc.option(fc.constantFrom(RAW_MCP_URL), { nil: undefined }),
  mcpServerError: fc.option(fc.constantFrom(RAW_MCP_ERROR), { nil: undefined }),
  mcpConfigPath: fc.option(fc.constantFrom(RAW_MCP_CONFIG_PATH), {
    nil: undefined,
  }),
});

/**
 * A PanelContext arbitrary guaranteed to carry a Confirmed_Mismatch AND a
 * non-busy index state (Property 1 preconditions).
 */
const arbMismatchIdleContext: fc.Arbitrary<PanelContext> = fc.record({
  compatibility: arbConfirmedMismatchSnapshot,
  // Never "indexing" — Property 1 requires indexing not busy.
  status: fc.constantFrom<WorkspaceStatus>(
    "notInstalled",
    "ready",
    "mcpEnabled",
    "degraded",
    "unknown"
  ),
  advancedMode: fc.boolean(),
  liveIndexing: fc.boolean(),
  mcpEnabled: fc.boolean(),
  syncPaused: fc.boolean(),
  configured: fc.boolean(),
  backendAvailable: fc.boolean(),
  version: fc.constantFrom("0.8.11", "0.8.10", "0.8.12"),
  health: arbHealth,
  indexStatus: arbNonBusyIndexStatus,
  prerequisites: arbPrerequisites,
  mcpServerName: fc.option(fc.constantFrom(RAW_MCP_NAME), { nil: undefined }),
  mcpServerUrl: fc.option(fc.constantFrom(RAW_MCP_URL), { nil: undefined }),
  mcpServerError: fc.option(fc.constantFrom(RAW_MCP_ERROR), { nil: undefined }),
  mcpConfigPath: fc.option(fc.constantFrom(RAW_MCP_CONFIG_PATH), {
    nil: undefined,
  }),
});

/** Extract user-visible text from rendered HTML (drop style/script + tags). */
function visibleText(html: string): string {
  return html
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<[^>]*>/g, " ");
}

/** Count non-overlapping occurrences of a literal substring. */
function countOccurrences(haystack: string, needle: string): number {
  let count = 0;
  let index = haystack.indexOf(needle);
  while (index !== -1) {
    count += 1;
    index = haystack.indexOf(needle, index + needle.length);
  }
  return count;
}

// ---------------------------------------------------------------------------
// Property 1 — No false Ready.
// ---------------------------------------------------------------------------

test("Property 1: a Confirmed_Mismatch (indexing not busy) always reads as Needs attention with a Compatibility_Primary_Action", () => {
  fc.assert(
    fc.property(arbMismatchIdleContext, (ctx) => {
      // Precondition guard: the context is a confirmed mismatch.
      assert.ok(
        isConfirmedMismatch(ctx.compatibility),
        "arbitrary must produce a Confirmed_Mismatch"
      );

      // Property scope: "indexing not busy" (design Property 1). Active/working
      // display states (busy indexing OR the transient "Finishing setup…"
      // status-active view) legitimately read as "Working" first, so exclude
      // them — the property governs the idle case only.
      fc.pre(
        ctx.status !== "indexing" &&
          !isIndexStatusBusy(ctx.indexStatus) &&
          derivePanelView(ctx).statusClass !== "status-active"
      );

      const statusLine = deriveStatusLine(ctx);
      const control = deriveUnifiedControl(ctx);
      const statusBar = outcomeLabelForContext(ctx);

      // Status line negates Ready and is exactly "Needs attention" (R1.4/R1.5).
      assert.equal(
        statusLine,
        "Needs attention",
        `expected "Needs attention", got "${statusLine}"`
      );
      assert.notEqual(statusLine, "Ready");

      // Status bar mirrors it as "Action needed".
      assert.equal(statusBar, "$(warning) Cognis: Action needed");

      // The unified control is a Compatibility_Primary_Action.
      assert.ok(
        COMPATIBILITY_PRIMARY_IDS.has(control.id),
        `unified control id "${control.id}" is not a Compatibility_Primary_Action`
      );
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

// ---------------------------------------------------------------------------
// Property 2 — Exactly one primary control (both advancedMode values, including
// mismatch overrides).
// ---------------------------------------------------------------------------

test("Property 2: renderPanelHtml contains exactly one [data-unified] in every state (both modes, incl. mismatch)", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const html = renderPanelHtml(ctx);
      const count = countOccurrences(html, "data-unified");
      assert.equal(
        count,
        1,
        `expected exactly one data-unified element, found ${count} (advancedMode=${ctx.advancedMode})`
      );
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

test("Property 2 (mismatch-forced): a Confirmed_Mismatch still yields exactly one [data-unified] on both surfaces", () => {
  fc.assert(
    fc.property(
      arbConfirmedMismatchSnapshot,
      fc.boolean(),
      (compatibility, advancedMode) => {
        const ctx: PanelContext = {
          status: "mcpEnabled",
          compatibility,
          advancedMode,
          configured: true,
          backendAvailable: true,
          mcpEnabled: true,
          liveIndexing: true,
          syncPaused: false,
          version: "0.8.11",
        };
        const html = renderPanelHtml(ctx);
        assert.equal(countOccurrences(html, "data-unified"), 1);
      }
    ),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

// ---------------------------------------------------------------------------
// Property 3 — No raw data on Minimal Surface (closed vocab + no raw values).
// ---------------------------------------------------------------------------

test("Property 3: Status_Line + hint stay in the closed vocabulary and leak no raw version/URL/id/error", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const statusLine: StatusLineText = deriveStatusLine(ctx);
      const hint = deriveStatusHint(ctx);

      // (a) The status line is in the closed vocabulary.
      assert.ok(
        CLOSED_STATUS_VOCAB.has(statusLine),
        `Status_Line "${statusLine}" is outside the closed vocabulary`
      );

      // (b) Neither the status line nor the hint leaks a raw technical value.
      for (const text of [statusLine, hint]) {
        for (const raw of [
          ctx.mcpServerUrl,
          ctx.mcpServerName,
          ctx.mcpServerError,
          ctx.mcpConfigPath,
        ]) {
          if (raw) {
            assert.ok(
              !text.includes(raw),
              `Minimal_Surface text leaked raw value "${raw}": ${JSON.stringify(text)}`
            );
          }
        }
        // No raw version, URL scheme, or standalone multi-digit number.
        assert.doesNotMatch(text, /\d+\.\d+/, `text embeds a dotted version: ${JSON.stringify(text)}`);
        assert.doesNotMatch(text, /https?:\/\//, `text embeds a URL: ${JSON.stringify(text)}`);
        assert.doesNotMatch(text, /\d{2,}/, `text embeds a raw number: ${JSON.stringify(text)}`);
      }
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

test("Property 3 (Minimal_Surface render): advancedMode off never leaks the raw mismatch versions", () => {
  fc.assert(
    fc.property(arbConfirmedMismatchSnapshot, (compatibility) => {
      // No `version` here: the hero renders `ctx.version` as a labeled version
      // badge (a legitimately-displayed value, not a leaked *mismatch* value).
      // Omitting it isolates the property to the compatibility raw versions.
      const ctx: PanelContext = {
        status: "mcpEnabled",
        compatibility,
        advancedMode: false,
        configured: true,
        backendAvailable: true,
        mcpEnabled: true,
        liveIndexing: true,
        syncPaused: false,
      };
      const html = renderPanelHtml(ctx);
      // The Minimal_Surface must not surface the raw Engine versions used by
      // the mismatch builders (0.8.10 / 0.8.11 / 0.8.12) — those belong only in
      // the labeled Advanced_Surface detail.
      if (isConfirmedMismatch(compatibility)) {
        const result = compatibility.result;
        for (const v of [result.engineVersion, result.expectedEngineVersion]) {
          if (v) {
            assert.ok(
              !html.includes(v),
              `Minimal_Surface render leaked raw version "${v}"`
            );
          }
        }
      }
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

// ---------------------------------------------------------------------------
// Property 4 — Remediation resolves to a registered command.
// ---------------------------------------------------------------------------

test("Property 4: a Compatibility_Primary_Action data-action resolves to a registered cognis.* command", () => {
  fc.assert(
    fc.property(arbMismatchIdleContext, (ctx) => {
      const control = deriveUnifiedControl(ctx);

      // Precondition: this is a Compatibility_Primary_Action.
      assert.ok(
        COMPATIBILITY_PRIMARY_IDS.has(control.id),
        `unified control id "${control.id}" is not a Compatibility_Primary_Action`
      );

      // The id is a key of ACTION_COMMANDS…
      assert.ok(
        Object.prototype.hasOwnProperty.call(ACTION_COMMANDS, control.id),
        `data-action "${control.id}" is not a key of ACTION_COMMANDS`
      );
      const command = ACTION_COMMANDS[control.id];
      // …and maps to a cognis.* command. (Task 4.5 scopes Property 4 to
      // ACTION_COMMANDS resolution; the manifest `contributes.commands`
      // declaration of cognis.updateExtension lands in Task 5.4, so the
      // manifest cross-check belongs to that wave — see panelUiContract.test.ts
      // for the whole-panel "no dead buttons" invariant.)
      assert.match(command, /^cognis\./, `"${control.id}" maps to non-cognis command "${command}"`);
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

// ---------------------------------------------------------------------------
// Property 7 — Terminology (Engine/Extension, never Backend/jargon) in the
// primary user-visible status text.
// ---------------------------------------------------------------------------

test("Property 7: Status_Line, Unified_Control label, and Status Bar use Engine/Extension and never Backend/jargon", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const primaryTexts = [
        deriveStatusLine(ctx),
        deriveStatusHint(ctx),
        deriveUnifiedControl(ctx).label,
        outcomeLabelForContext(ctx),
      ];

      for (const text of primaryTexts) {
        for (const re of FORBIDDEN_WORDS) {
          assert.doesNotMatch(
            text,
            re,
            `primary status text leaks forbidden word ${re}: ${JSON.stringify(text)}`
          );
        }
      }
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});

test("Property 7 (rendered): user-visible panel text never contains Backend, in either mode", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const text = visibleText(renderPanelHtml(ctx));
      assert.ok(
        !text.includes("Backend"),
        "user-visible text must use the unified term Engine, not Backend"
      );
      assert.ok(
        !text.includes("Cognis: Cognis:"),
        "user-visible text must not contain the doubled prefix"
      );
    }),
    { numRuns: CODEBASE_NUM_RUNS }
  );
});
