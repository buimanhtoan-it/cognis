// Harness first: installs the vscode stub before panel.ts is required.
import "./testHarness";

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { FIRST_PROBE_COMPATIBILITY_SNAPSHOT } from "../compatibility";
import { renderPanelHtml, type PanelContext } from "../panel";
import {
  PROPERTY_RUNS,
  arbAdvancedModeSequence,
  type AdvancedModeStep,
} from "./generators";

interface MenuContribution {
  command?: string;
  when?: string;
}

interface Manifest {
  contributes?: {
    menus?: {
      [location: string]: MenuContribution[];
    };
  };
}

const MANIFEST_PATH = path.join(__dirname, "..", "..", "package.json");
const ADVANCED_MODE_WHEN = "config.cognis.advancedMode";
const ALWAYS_AVAILABLE_COMMANDS = new Set([
  "cognis.toggle",
  "cognis.showStatus",
  "cognis.showOutput",
  "cognis.showDiagnostics",
]);
const ADVANCED_COMMANDS = new Set([
  "cognis.installBackend",
  "cognis.reinstallEngine",
  "cognis.uninstallEngine",
  "cognis.forceCleanup",
  "cognis.coldRestart",
  "cognis.startMcpServer",
  "cognis.stopMcpServer",
  "cognis.pauseSync",
  "cognis.resumeSync",
  "cognis.connectMcp",
  "cognis.disconnectMcp",
  "cognis.repairSetup",
  "cognis.clearAndReindex",
  "cognis.removeFromWorkspace",
  "cognis.prepareUninstall",
  "cognis.cancelIndexing",
]);
const ADVANCED_ONLY_ACTIONS = new Set([
  "clearReindex",
  "reinstallEngine",
  "coldRestart",
  "remove",
  "prepareUninstall",
  "forceCleanup",
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

function loadMenu(location: string): MenuContribution[] {
  const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8")) as Manifest;
  const entries = manifest.contributes?.menus?.[location];
  assert.ok(
    Array.isArray(entries) && entries.length > 0,
    `package.json must declare contributes.menus[${JSON.stringify(location)}]`
  );
  return entries!;
}

function evaluateWhen(when: string | undefined, step: AdvancedModeStep): boolean {
  if (!when) {
    return true;
  }
  return when.split("&&").every((rawClause) => {
    const clause = rawClause.trim();
    switch (clause) {
      case "view == cognis.controlPanel":
        return true;
      case ADVANCED_MODE_WHEN:
        return step.advancedMode;
      case `!${ADVANCED_MODE_WHEN}`:
        return !step.advancedMode;
      case "cognis.mcpServerRunning":
        return step.mcpServerRunning;
      case "!cognis.mcpServerRunning":
        return !step.mcpServerRunning;
      case "cognis.syncPaused":
        return step.syncPaused;
      case "!cognis.syncPaused":
        return !step.syncPaused;
      default:
        assert.fail(`unsupported when-clause ${JSON.stringify(clause)}`);
    }
  });
}

function visibleMenuCommands(location: string, step: AdvancedModeStep): string[] {
  return loadMenu(location)
    .filter(
      (entry): entry is MenuContribution & { command: string } =>
        typeof entry.command === "string" && ADVANCED_COMMANDS.has(entry.command)
    )
    .filter((entry) => evaluateWhen(entry.when, step))
    .map((entry) => entry.command)
    .sort();
}

function panelContext(advancedMode: boolean): PanelContext {
  return {
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    status: "ready",
    advancedMode,
    liveIndexing: true,
    mcpEnabled: true,
    configured: true,
    backendAvailable: true,
    syncPaused: false,
  };
}

function panelAdvancedActions(advancedMode: boolean): string[] {
  const html = renderPanelHtml(panelContext(advancedMode));
  const values: string[] = [];
  const re = /data-action="([^"]+)"/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(html)) !== null) {
    if (ADVANCED_ONLY_ACTIONS.has(match[1])) {
      values.push(match[1]);
    }
  }
  return values.sort();
}

function expectedPalette(advancedMode: boolean): string[] {
  return advancedMode ? [...ADVANCED_COMMANDS].sort() : [];
}

// Feature: cognis-one-toggle-engine, Property 34: For any sequence of
// Advanced_Mode changes, the visible Advanced_Command set is determined solely
// by the current Advanced_Mode value and is independent of the value at
// activation or of the path taken to reach it.
// **Validates: Requirements 5.1, 5.2, 5.3**
test("Property 34: Advanced visibility is a pure function of the setting", () => {
  const palette = loadMenu("commandPalette");
  for (const entry of palette) {
    if (
      typeof entry.command === "string" &&
      ALWAYS_AVAILABLE_COMMANDS.has(entry.command)
    ) {
      assert.equal(
        entry.when,
        undefined,
        `${entry.command} must stay unconditioned`
      );
    }
  }

  fc.assert(
    fc.property(arbAdvancedModeSequence(), (history) => {
      for (const step of history) {
        assert.deepEqual(
          visibleMenuCommands("commandPalette", step),
          expectedPalette(step.advancedMode),
          `palette Advanced_Command set must follow current Advanced_Mode=${step.advancedMode}`
        );

        const viewTitle = visibleMenuCommands("view/title", step);
        if (!step.advancedMode) {
          assert.deepEqual(
            viewTitle,
            [],
            "disabled Advanced_Mode must hide every view/title Advanced_Command regardless of MCP/sync context"
          );
        }

        const leakedPanel = panelAdvancedActions(step.advancedMode);
        if (!step.advancedMode) {
          assert.deepEqual(
            leakedPanel,
            [],
            "disabled Advanced_Mode must hide every Advanced_Only_Action on the panel"
          );
        } else {
          assert.ok(
            leakedPanel.length > 0,
            "enabled Advanced_Mode must expose Advanced_Only_Action controls on the panel"
          );
        }
      }

      const current = history[history.length - 1]!;
      const activation = history[0]!;
      const direct: AdvancedModeStep[] = [current];
      const oppositeActivation: AdvancedModeStep[] = [
        {
          ...current,
          advancedMode: !activation.advancedMode,
        },
        {
          ...current,
          advancedMode: !current.advancedMode,
        },
        current,
      ];

      const byHistory = {
        palette: visibleMenuCommands("commandPalette", current),
        viewTitle: visibleMenuCommands("view/title", current),
        panel: panelAdvancedActions(current.advancedMode),
      };
      const byDirect = {
        palette: visibleMenuCommands("commandPalette", direct[0]!),
        viewTitle: visibleMenuCommands("view/title", direct[0]!),
        panel: panelAdvancedActions(direct[0]!.advancedMode),
      };
      const byOpposite = {
        palette: visibleMenuCommands(
          "commandPalette",
          oppositeActivation[oppositeActivation.length - 1]!
        ),
        viewTitle: visibleMenuCommands(
          "view/title",
          oppositeActivation[oppositeActivation.length - 1]!
        ),
        panel: panelAdvancedActions(
          oppositeActivation[oppositeActivation.length - 1]!.advancedMode
        ),
      };

      assert.deepEqual(
        byHistory,
        byDirect,
        "a history and a direct setting of the same value must yield identical visibility"
      );
      assert.deepEqual(
        byHistory,
        byOpposite,
        "the activation-time Advanced_Mode value and the path taken must not change final visibility"
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
