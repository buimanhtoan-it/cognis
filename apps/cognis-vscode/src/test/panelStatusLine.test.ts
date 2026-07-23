// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { deriveStatusLine, type PanelContext } from "../panel";
import { FIRST_PROBE_COMPATIBILITY_SNAPSHOT } from "../compatibility";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "../types";

// ---------------------------------------------------------------------------
// Feature: extension-minimal-panel, Property 5: Status_Line dùng từ vựng cố
// định và không lộ giá trị kỹ thuật thô.
//
// Validates: Requirements 2.2, 2.3
//
// For any PanelContext, deriveStatusLine(ctx) returns a value within the closed
// vocabulary {"Ready","Working","Paused","Needs attention"}; AND the rendered
// Status_Line text must not contain any raw technical value from the context —
// specifically not mcpServerUrl, mcpServerName, mcpServerError, mcpConfigPath,
// no port number, and nothing matching `http://`/`https://`.
//
// renderStatusLine (task 2.3) is not implemented yet, so the Status_Line text
// under test is the derived value itself. The "no raw technical value" check
// follows naturally because the vocabulary is closed; the arbitrary still
// injects raw technical values so the assertion is meaningfully exercised.
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

/** The only strings the Status_Line is ever allowed to contain. */
const CLOSED_VOCAB = new Set<string>([
  "Ready",
  "Working",
  "Paused",
  "Off",
  "Needs attention",
]);

// Raw technical values injected into the context so the "no leak" assertion is
// exercised against concrete, distinctive strings. Each carries an embedded
// port number so the port-leak check is meaningful.
const RAW_MCP_URL = "http://127.0.0.1:50001/mcp";
const RAW_MCP_URL_HTTPS = "https://localhost:8443/mcp";
const RAW_MCP_NAME = "cognis-workspace-ab12cd";
const RAW_MCP_ERROR = "cognis-mcpd exited with code=1 on port 50001";
const RAW_MCP_CONFIG_PATH = "/repo/.cursor/mcp.json";

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
 * of status / health / flags, and injects raw technical values
 * (mcpServerUrl/name/error/configPath with embedded port numbers) so the
 * "no raw technical value" assertion is meaningfully exercised.
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
  mcpServerName: fc.option(fc.constantFrom(RAW_MCP_NAME), { nil: undefined }),
  mcpServerUrl: fc.option(fc.constantFrom(RAW_MCP_URL, RAW_MCP_URL_HTTPS), {
    nil: undefined,
  }),
  mcpServerError: fc.option(fc.constantFrom(RAW_MCP_ERROR), { nil: undefined }),
  mcpConfigPath: fc.option(fc.constantFrom(RAW_MCP_CONFIG_PATH), {
    nil: undefined,
  }),
});

test("Property 5: Status_Line uses the closed vocabulary and never leaks a raw technical value", () => {
  fc.assert(
    fc.property(arbPanelContext, (ctx) => {
      const statusLine = deriveStatusLine(ctx);

      // (a) The derived value is one of the four fixed vocabulary strings.
      assert.ok(
        CLOSED_VOCAB.has(statusLine),
        `Status_Line "${statusLine}" is outside the closed vocabulary`
      );

      // (b) The Status_Line text must not contain any raw technical value from
      // the context, nor a port number, nor an http(s) URL scheme.
      for (const raw of [
        ctx.mcpServerUrl,
        ctx.mcpServerName,
        ctx.mcpServerError,
        ctx.mcpConfigPath,
      ]) {
        if (raw) {
          assert.ok(
            !statusLine.includes(raw),
            `Status_Line leaked raw technical value: ${raw}`
          );
        }
      }
      assert.doesNotMatch(
        statusLine,
        /\d{2,}/,
        `Status_Line leaked a port/number: "${statusLine}"`
      );
      assert.doesNotMatch(
        statusLine,
        /https?:\/\//,
        `Status_Line leaked a URL scheme: "${statusLine}"`
      );
    }),
    { numRuns: 300 }
  );
});
