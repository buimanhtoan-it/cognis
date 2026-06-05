// Harness first: installs the vscode stub before panel.ts (which imports vscode)
// is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import {
  derivePanelView,
  deriveSetupSteps,
  outcomeLabelForContext,
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
  // The primary action must say "Connect to AI" (not "Set Up for AI") here:
  // the index is built, only MCP wiring remains.
  const view = derivePanelView(ctx);
  assert.equal(view.primary?.label, "Connect to AI");
  assert.match(view.headline, /connect ai/i);
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
