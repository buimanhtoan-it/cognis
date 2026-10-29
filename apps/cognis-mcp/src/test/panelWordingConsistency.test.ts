// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { renderPanelHtml, type PanelContext } from "../panel";
import { FIRST_PROBE_COMPATIBILITY_SNAPSHOT } from "../compatibility";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-minimal-panel, Property 6: Giữ tính nhất quán từ ngữ của
// spec extension-ux-coherence.
//
// Validates: Requirements 9.4
//
// For any PanelContext (both advancedMode values, every state), the
// user-visible text of the HTML produced by renderPanelHtml(ctx) does NOT
// contain the doubled prefix "Cognis: Cognis:" and does NOT contain the word
// "Backend" (case-sensitive as written; the unified terminology is "Engine").
//
// Design Property 6 scopes this to "text người dùng nhìn thấy" (user-visible
// text). We therefore strip <style>/<script> blocks and all HTML tags before
// asserting — so structural attribute values such as data-action="installBackend"
// (a UI-contract identifier, never shown to the user) do not produce false
// positives. User-facing labels in panel.ts use "Install engine"/"Engine".
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
 * Mirrors the arbitrary in panelUnifiedControl.test.ts.
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

/**
 * Extract the user-visible text from the rendered panel HTML: drop
 * <style>/<script> blocks entirely, then strip every tag (which removes all
 * attribute values, e.g. data-action="installBackend"). What remains is the
 * text a user actually reads.
 */
function visibleText(html: string): string {
  return html
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<[^>]*>/g, " ");
}

test("Property 6: rendered user-visible text keeps unified wording (no 'Cognis: Cognis:', no 'Backend')", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const text = visibleText(renderPanelHtml(ctx));

      assert.ok(
        !text.includes("Cognis: Cognis:"),
        `user-visible text must not contain the doubled prefix "Cognis: Cognis:"`
      );
      assert.ok(
        !text.includes("Backend"),
        `user-visible text must use the unified term "Engine", not "Backend"`
      );
    }),
    { numRuns: 300 }
  );
});
