// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { renderPanelHtml, type PanelContext } from "../panel";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-minimal-panel, Property 4: Bảo đảm Minimal_Surface khi
// Advanced_Mode tắt.
//
// Validates: Requirements 2.1, 2.4, 2.5, 2.6, 2.7, 5.1, 6.1, 8.2
//
// For any PanelContext with `advancedMode` falsy, the HTML rendered by
// renderPanelHtml satisfies ALL of:
//   (a) exactly one `data-unified` AND exactly one `data-status-line`;
//   (b) NO `data-action` in the Advanced_Only_Action set;
//   (c) NO `data-action` in the Destructive_Action set;
//   (d) does NOT render the stepper / file lists / prerequisites checklist /
//       health breakdown / logs content — asserted on the stable markers the
//       panel render helpers emit ("step-list", "file-sections", "prereq-list",
//       "footer-links", "Index Status", "Danger zone").
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

/**
 * Advanced_Only_Action `data-action` values — must never appear on the
 * Minimal_Surface (design "Advanced_Only_Action" set / R2.5, R8.2).
 */
const ADVANCED_ONLY_ACTIONS = new Set([
  "clearReindex",
  "reinstallEngine",
  "coldRestart",
  "remove",
  "prepareUninstall",
  "startMcp",
  "stopMcp",
  "connectMcp",
  "disconnectMcp",
  "cancelIndexing",
  "refreshPrerequisites",
  "installAllPrerequisites",
  "health",
  "output",
]);

/**
 * Destructive_Action `data-action` values — must never appear on the
 * Minimal_Surface (design "Destructive_Action" set / R6.1).
 */
const DESTRUCTIVE_ACTIONS = new Set([
  "clearReindex",
  "reinstallEngine",
  "coldRestart",
  "remove",
  "prepareUninstall",
]);

/**
 * Stable markers emitted by the detail-surface render helpers. Their absence
 * proves the Minimal_Surface renders none of: the 4-step stepper
 * (`renderStepperSection` → `<ol class="step-list">`), the queued/inflight/
 * recent file lists (`<div class="file-sections">`), the prerequisites
 * checklist (`renderPrerequisitesSection` → `<ul class="prereq-list">`), the
 * Index Status surface (title text "Index Status"), the Logs_View footer links
 * (`<div class="footer-links">`), or the danger zone (summary text
 * "Danger zone").
 *
 * The class-based markers are matched in their HTML class-attribute form
 * (`class="step-list"`, …) rather than as bare substrings: those same class
 * names also appear as CSS selectors (`.step-list { … }`) in the shared
 * `<style>` block that BOTH surfaces embed, so a bare-substring match would
 * false-positive on the stylesheet instead of on rendered content.
 */
const FORBIDDEN_MARKERS = [
  'class="step-list"',
  'class="file-sections"',
  'class="prereq-list"',
  'class="footer-links"',
  "Index Status",
  "Danger zone",
];

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
 * Generates random PanelContext values with `advancedMode` fixed to `false`,
 * across every combination of status / health / flags, and optionally injects
 * raw technical values (mcpServerUrl/name/error/configPath) to exercise the
 * full input space. Mirrors the shared arbitrary in
 * `panelUnifiedControl.test.ts` but pins `advancedMode: false` so every
 * generated case renders the Minimal_Surface.
 */
const arbMinimalPanelContext: fc.Arbitrary<PanelContext> = fc.record({
  status: fc.constantFrom(...WORKSPACE_STATUSES),
  advancedMode: fc.constant(false),
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

/** Count non-overlapping occurrences of a literal substring in `haystack`. */
function countOccurrences(haystack: string, needle: string): number {
  let count = 0;
  let index = haystack.indexOf(needle);
  while (index !== -1) {
    count += 1;
    index = haystack.indexOf(needle, index + needle.length);
  }
  return count;
}

/** Extract every `data-action="X"` value from the rendered HTML. */
function dataActions(html: string): string[] {
  const values: string[] = [];
  const re = /data-action="([^"]+)"/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(html)) !== null) {
    values.push(match[1]);
  }
  return values;
}

test("Property 4: Advanced_Mode off guarantees a bounded Minimal_Surface", () => {
  fc.assert(
    fc.property(arbMinimalPanelContext, (ctx) => {
      const html = renderPanelHtml(ctx);

      // (a) Exactly one unified control and exactly one status line.
      assert.equal(
        countOccurrences(html, "data-unified"),
        1,
        "expected exactly one data-unified element on the Minimal_Surface"
      );
      assert.equal(
        countOccurrences(html, "data-status-line"),
        1,
        "expected exactly one data-status-line element on the Minimal_Surface"
      );

      // (b) + (c) No data-action in the Advanced_Only or Destructive sets.
      for (const action of dataActions(html)) {
        assert.ok(
          !ADVANCED_ONLY_ACTIONS.has(action),
          `Advanced_Only_Action "${action}" leaked onto the Minimal_Surface`
        );
        assert.ok(
          !DESTRUCTIVE_ACTIONS.has(action),
          `Destructive_Action "${action}" leaked onto the Minimal_Surface`
        );
      }

      // (d) No stepper / file lists / prerequisites / Index Status / logs /
      // danger zone — asserted on the stable markers the helpers emit.
      for (const marker of FORBIDDEN_MARKERS) {
        assert.ok(
          !html.includes(marker),
          `Minimal_Surface must not contain detail-surface marker "${marker}"`
        );
      }
    }),
    { numRuns: 300 }
  );
});
