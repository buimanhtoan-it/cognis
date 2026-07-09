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
// Feature: extension-minimal-panel, Property 1: Đúng một Unified_Control trong
// mọi trạng thái.
//
// Validates: Requirements 1.1
//
// For any PanelContext (regardless of advancedMode true or false, regardless of
// state), the HTML rendered by renderPanelHtml(ctx) contains EXACTLY ONE element
// carrying `data-unified` — the single unified primary control.
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
 * (mcpServerUrl/name/error/configPath) to exercise the full input space.
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

test("Property 1: exactly one Unified_Control (data-unified) is rendered in every state", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const html = renderPanelHtml(ctx);
      const count = (html.match(/data-unified/g) || []).length;
      assert.equal(
        count,
        1,
        `expected exactly one data-unified element, found ${count}`
      );
    }),
    { numRuns: 300 }
  );
});
