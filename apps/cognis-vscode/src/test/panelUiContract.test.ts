// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { ACTION_COMMANDS, renderPanelHtml, type PanelContext } from "../panel";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-minimal-panel, Property 3: Bất biến hợp đồng UI — không có
// nút chết (UI contract invariant — no dead buttons).
//
// Validates: Requirements 1.7, 4.6, 8.5, 8.9
//
// For any PanelContext (BOTH modes), every rendered <button> that carries a
// `data-action` resolves to a registered Command: the `data-action` value is a
// key of ACTION_COMMANDS (or is `installPrerequisite` accompanied by a
// `data-item` attribute), and the mapped value is a command id declared in
// package.json's contributes.commands. The invariant passes only when 100% of
// data-actions resolve; if any doesn't, it fails.
// ---------------------------------------------------------------------------

interface CommandContribution {
  command: string;
  title: string;
  category?: string;
}

interface Manifest {
  contributes?: {
    commands?: CommandContribution[];
  };
}

// out/test/ -> repo (apps/cognis-vscode) root is two levels up. Read the
// declared command ids straight from the manifest, mirroring
// contributions.test.ts, so "maps to a declared command id" is checked against
// the real source of truth rather than a hand-copied list.
const MANIFEST_PATH = path.join(__dirname, "..", "..", "package.json");

const DECLARED_COMMAND_IDS: Set<string> = (() => {
  const manifest = JSON.parse(
    fs.readFileSync(MANIFEST_PATH, "utf8")
  ) as Manifest;
  const commands = manifest.contributes?.commands;
  assert.ok(
    Array.isArray(commands) && commands.length > 0,
    "package.json must declare contributes.commands"
  );
  return new Set(commands!.map((c) => c.command));
})();

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
 * Generates random PanelContext values across BOTH modes and every combination
 * of status / health / flags, and optionally injects raw technical values
 * (mcpServerUrl/name/error/configPath) to exercise the full input space.
 * Mirrors the arbitrary in panelUnifiedControl.test.ts.
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

/** Match every `<button …>` opening tag in the rendered HTML. */
const BUTTON_TAG = /<button\b[^>]*>/g;

/** Extract the value of a double-quoted attribute from a single tag. */
function attr(tag: string, name: string): string | undefined {
  const match = new RegExp(`${name}="([^"]*)"`).exec(tag);
  return match ? match[1] : undefined;
}

test("Property 3: every rendered data-action resolves to a declared command (no dead buttons)", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const html = renderPanelHtml(ctx);

      for (const tag of html.match(BUTTON_TAG) ?? []) {
        const action = attr(tag, "data-action");
        if (action === undefined) {
          // Buttons without a data-action (e.g. plain UI toggles) carry no UI
          // contract obligation.
          continue;
        }

        if (action === "installPrerequisite") {
          // Per-item prerequisite install is dispatched directly to
          // cognis.installPrerequisite with the item id as a payload, so it is
          // valid iff the same button also carries a data-item attribute.
          const item = attr(tag, "data-item");
          assert.ok(
            typeof item === "string" && item.length > 0,
            `installPrerequisite button must carry a non-empty data-item (tag: ${tag})`
          );
          continue;
        }

        // Otherwise the data-action must be a key of ACTION_COMMANDS…
        assert.ok(
          Object.prototype.hasOwnProperty.call(ACTION_COMMANDS, action),
          `data-action "${action}" is not a key of ACTION_COMMANDS (dead button)`
        );

        // …and its mapped value must be a command id declared in the manifest.
        const command = ACTION_COMMANDS[action];
        assert.ok(
          DECLARED_COMMAND_IDS.has(command),
          `data-action "${action}" maps to "${command}", which is not declared in contributes.commands`
        );
      }
    }),
    { numRuns: 300 }
  );
});
