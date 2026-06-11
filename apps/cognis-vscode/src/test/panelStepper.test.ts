// Harness first: installs the vscode stub before panel.ts (which imports vscode)
// is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import {
  derivePanelView,
  deriveSetupSteps,
  outcomeLabelForContext,
  renderMcpSection,
  renderStepperSection,
  type PanelContext,
} from "../panel";
import type { HealthReport, PrerequisiteReport } from "../types";

function okHealth(): HealthReport {
  const ok = { status: "ok", message: "ok" };
  return {
    runtime_version: "0.3.2",
    overall: "ok",
    checks: { config: ok, db: ok, index: ok, vector: ok, embedder: ok, version: ok },
  };
}

function readyPrereqs(ready = true): PrerequisiteReport {
  return {
    python: "python",
    ready,
    combined_install_target: ready ? "" : ".[indexer]",
    items: [
      {
        id: "indexer",
        label: "Code parsers",
        description: "Parses code.",
        status: ready ? "ok" : "missing",
        required: true,
        install_target: ".[indexer]",
        detail: ready ? "Installed." : "Missing.",
      },
    ],
  };
}

function stepState(ctx: PanelContext, id: string): string {
  return deriveSetupSteps(ctx).find((s) => s.id === id)!.state;
}

test("fresh workspace: only the backend step is active, rest pending", () => {
  const ctx: PanelContext = { status: "notInstalled" };
  assert.equal(stepState(ctx, "backend"), "active");
  assert.equal(stepState(ctx, "components"), "pending");
  assert.equal(stepState(ctx, "indexed"), "pending");
  assert.equal(stepState(ctx, "connected"), "pending");
});

test("missing required component flags the components step as error", () => {
  const ctx: PanelContext = {
    status: "notInstalled",
    prerequisites: readyPrereqs(false),
  };
  assert.equal(stepState(ctx, "backend"), "done");
  assert.equal(stepState(ctx, "components"), "error");
  assert.equal(stepState(ctx, "indexed"), "pending");
});

test("indexing in progress marks the index step active", () => {
  const ctx: PanelContext = {
    status: "indexing",
    prerequisites: readyPrereqs(true),
    configured: true,
  };
  assert.equal(stepState(ctx, "components"), "done");
  assert.equal(stepState(ctx, "indexed"), "active");
});

test("broken python interpreter flags the backend step as error", () => {
  const ctx: PanelContext = {
    status: "unknown",
    setupHint: "python",
    configured: true,
  };
  assert.equal(stepState(ctx, "backend"), "error");
  assert.equal(stepState(ctx, "components"), "pending");
});

test("fully wired workspace marks every step done and hides the stepper", () => {
  const ctx: PanelContext = {
    status: "mcpEnabled",
    health: okHealth(),
    prerequisites: readyPrereqs(true),
    configured: true,
    mcpEnabled: true,
    liveIndexing: true,
  };
  for (const step of deriveSetupSteps(ctx)) {
    assert.equal(step.state, "done", `${step.id} should be done`);
  }
  // Stepper hides itself once there is nothing left to do.
  assert.equal(renderStepperSection(ctx), "");
});

test("index ready but MCP not connected keeps the AI step active", () => {
  const ctx: PanelContext = {
    status: "ready",
    health: okHealth(),
    prerequisites: readyPrereqs(true),
    configured: true,
    mcpEnabled: false,
    liveIndexing: false,
  };
  assert.equal(stepState(ctx, "indexed"), "done");
  assert.equal(stepState(ctx, "connected"), "active");
  // Stepper is still visible while a step is outstanding.
  assert.ok(renderStepperSection(ctx).includes("Getting started"));
  // The primary action connects the MCP server (writes mcp.json), not first-run
  // setup: the index is built, only MCP wiring remains.
  const view = derivePanelView(ctx);
  assert.equal(view.primary?.id, "connectAi");
  assert.match(view.primary?.label ?? "", /mcp/i);
  assert.match(view.headline, /mcp/i);
});

test("status bar collapses to a short, stable vocabulary", () => {
  assert.equal(
    outcomeLabelForContext({ status: "indexing" }),
    "$(sync~spin) Cognis: Indexing"
  );
  assert.equal(
    outcomeLabelForContext({ status: "notInstalled" }),
    "$(circle-slash) Cognis: Not set up"
  );
  assert.equal(
    outcomeLabelForContext({
      status: "mcpEnabled",
      health: okHealth(),
      mcpEnabled: true,
      liveIndexing: true,
    }),
    "$(plug) Cognis: Ready"
  );
  assert.equal(
    outcomeLabelForContext({
      status: "ready",
      health: okHealth(),
      mcpEnabled: false,
    }),
    "$(check) Cognis: Index ready"
  );
});

test("fresh machine with no backend flags the backend step as error", () => {
  const ctx: PanelContext = {
    status: "notInstalled",
    backendAvailable: false,
  };
  assert.equal(stepState(ctx, "backend"), "error");
  assert.equal(stepState(ctx, "components"), "pending");
});

test("backend present (doctor ran) but no health keeps backend step done", () => {
  const ctx: PanelContext = {
    status: "notInstalled",
    backendAvailable: true,
    prerequisites: readyPrereqs(true),
  };
  assert.equal(stepState(ctx, "backend"), "done");
});

test("fresh machine panel guides the user to install the backend first", () => {
  const view = derivePanelView({ status: "notInstalled", backendAvailable: false });
  assert.match(view.headline, /Install the Cognis backend/);
  // It must offer a one-click install, not a setup button that would fail.
  assert.equal(view.primary?.id, "installBackend");
});

test("backend unknown (not yet probed) still shows the plain setup prompt", () => {
  const view = derivePanelView({ status: "notInstalled" });
  assert.equal(view.headline, "Setup required");
  assert.equal(view.primary?.id, "setup");
});

// ---------------------------------------------------------------------------
// Regression: fresh-install cold-start race. During the embedding backfill the
// DB is WAL-locked and the vector table is incomplete, so a health poll can
// momentarily read a failing "vector" check or fail to open the DB. The panel
// must keep showing progress and must NOT regress a configured workspace to
// "Set Up for AI" or loop on "Troubleshoot". (Reproduces the user-reported
// first-run setup⇄repair loop that the e2e suite missed.)
// ---------------------------------------------------------------------------

function vectorFailHealth(): HealthReport {
  const ok = { status: "ok", message: "ok" };
  return {
    runtime_version: "0.5.1",
    overall: "fail",
    checks: {
      config: ok,
      db: ok,
      index: ok,
      vector: { status: "fail", message: "no vectors yet (embedding in progress)" },
      embedder: ok,
      version: ok,
    },
  };
}

function embeddingStatus(): PanelContext["indexStatus"] {
  return {
    active: true,
    phase: "embedding",
    message: "Generating semantic embeddings… 100/200 symbols (search already works)",
    progressPercent: 85,
    pendingCount: 0,
    pendingFiles: [],
    inflightCount: 0,
    inflightFiles: [],
    recentFiles: [],
    updatedAt: Date.now(),
  };
}

test("embedding in progress never regresses to Set Up for AI (configured + health gap)", () => {
  // indexd is still embedding (active) and a poll landed while the DB was
  // locked, so health is momentarily undefined.
  const view = derivePanelView({
    status: "indexing",
    configured: true,
    indexStatus: embeddingStatus(),
  });
  assert.match(view.headline, /generating embeddings/i);
  assert.notEqual(view.primary?.id, "setup");
  assert.notEqual(view.primary?.id, "repair");
});

test("embedding in progress is shown as progress, not a repairable failure", () => {
  // health momentarily reports vector=fail because vectors aren't written yet —
  // an active embedding op must show progress, never "Troubleshoot".
  const view = derivePanelView({
    status: "indexing",
    configured: true,
    health: vectorFailHealth(),
    indexStatus: embeddingStatus(),
  });
  assert.match(view.headline, /generating embeddings/i);
  assert.notEqual(view.primary?.id, "repair");
});

test("an active index op wins even if status was momentarily computed as degraded", () => {
  // Defensive: indexStatus.active is the source of truth for an in-flight op.
  const view = derivePanelView({
    status: "degraded",
    configured: true,
    health: vectorFailHealth(),
    indexStatus: embeddingStatus(),
  });
  assert.match(view.headline, /generating embeddings/i);
  assert.notEqual(view.primary?.id, "repair");
});

test("configured workspace with a transient health gap does not regress to setup", () => {
  // No active op, health undefined, but already set up — show a non-destructive
  // checking state, never "Set Up for AI"/"Install backend".
  const view = derivePanelView({ status: "unknown", configured: true });
  assert.equal(view.statusClass, "status-active");
  assert.notEqual(view.primary?.id, "setup");
  assert.notEqual(view.primary?.id, "installBackend");
  assert.match(view.headline, /finishing setup/i);
});

test("fresh (unconfigured) machine still shows Set Up / Install — fix does not over-reach", () => {
  assert.equal(derivePanelView({ status: "notInstalled" }).primary?.id, "setup");
  assert.equal(
    derivePanelView({ status: "notInstalled", backendAvailable: false }).primary?.id,
    "installBackend"
  );
});

// ---------------------------------------------------------------------------
// MCP server status surface: Cognis is an MCP server, so the panel states the
// server status explicitly and offers the one action that connects it (writes
// the workspace mcp.json) — instead of vague "AI" wording.
// ---------------------------------------------------------------------------

test("MCP section is hidden before the workspace is set up", () => {
  assert.equal(renderMcpSection({ status: "notInstalled" }), "");
});

test("MCP section: not connected shows the mcp.json setup action + server/config", () => {
  const html = renderMcpSection({
    status: "ready",
    configured: true,
    mcpEnabled: false,
    mcpHost: "cursor",
    mcpServerName: "cognis-workspace-ab12cd",
    mcpConfigPath: "/repo/.cursor/mcp.json",
  });
  assert.match(html, /not connected/i);
  assert.match(html, /Set up MCP \(mcp\.json\)/);
  assert.match(html, /data-action="connectAi"/);
  assert.match(html, /Cursor/);
  assert.match(html, /cognis-workspace-ab12cd/);
  assert.match(html, /\.cursor[\\/]mcp\.json/);
});

test("MCP section: connected shows connected status + a re-write action", () => {
  const html = renderMcpSection({
    status: "mcpEnabled",
    configured: true,
    mcpEnabled: true,
    mcpHost: "vscode",
    mcpServerName: "cognis-workspace-ab12cd",
  });
  assert.match(html, /MCP server — connected/);
  assert.match(html, /VS Code/);
  assert.match(html, /data-action="connectAi"/);
  assert.match(html, /Re-write mcp\.json/);
});

test("connected panel headline names the MCP server (not vague 'AI search ready')", () => {
  const view = derivePanelView({
    status: "mcpEnabled",
    health: okHealth(),
    mcpEnabled: true,
    liveIndexing: true,
    configured: true,
  });
  assert.match(view.headline, /mcp server connected/i);
  assert.equal(view.primary, undefined);
});

test("the MCP-connected onboarding step is labelled 'MCP connected'", () => {
  const ctx: PanelContext = {
    status: "ready",
    health: okHealth(),
    configured: true,
    mcpEnabled: false,
    liveIndexing: false,
  };
  const step = deriveSetupSteps(ctx).find((s) => s.id === "connected");
  assert.equal(step?.label, "MCP connected");
});


// ---------------------------------------------------------------------------
// Standalone HTTP MCP server sub-section: panel-managed, opt-in, with URL.
// ---------------------------------------------------------------------------

test("HTTP MCP subsection: stopped offers a Start button (default)", () => {
  const html = renderMcpSection({
    status: "ready",
    configured: true,
    mcpEnabled: true,
    mcpHost: "vscode",
    mcpServerPhase: "stopped",
  });
  assert.match(html, /Standalone HTTP MCP server — Stopped/);
  assert.match(html, /data-action="startMcp"/);
  assert.doesNotMatch(html, /data-action="stopMcp"/);
});

test("HTTP MCP subsection: running shows the URL and a Stop button", () => {
  const html = renderMcpSection({
    status: "ready",
    configured: true,
    mcpEnabled: true,
    mcpHost: "vscode",
    mcpServerPhase: "running",
    mcpServerUrl: "http://127.0.0.1:50001/mcp",
  });
  assert.match(html, /Standalone HTTP MCP server — Running/);
  assert.match(html, /http:\/\/127\.0\.0\.1:50001\/mcp/);
  assert.match(html, /data-action="stopMcp"/);
});

test("HTTP MCP subsection: starting shows progress and a Stop button", () => {
  const html = renderMcpSection({
    status: "ready",
    configured: true,
    mcpEnabled: true,
    mcpHost: "vscode",
    mcpServerPhase: "starting",
    mcpServerUrl: "http://127.0.0.1:50001/mcp",
  });
  assert.match(html, /Standalone HTTP MCP server — Starting/);
  assert.match(html, /data-action="stopMcp"/);
});

test("HTTP MCP subsection: error surfaces the message and offers Start to retry", () => {
  const html = renderMcpSection({
    status: "ready",
    configured: true,
    mcpEnabled: true,
    mcpHost: "vscode",
    mcpServerPhase: "error",
    mcpServerError: "cognis-mcpd exited with code=1",
  });
  assert.match(html, /Standalone HTTP MCP server — Error/);
  assert.match(html, /code=1/);
  assert.match(html, /data-action="startMcp"/);
});
