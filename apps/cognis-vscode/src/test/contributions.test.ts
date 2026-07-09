// Static manifest audit: these assertions read package.json directly and need
// no VS Code host — they lock the command-contribution invariants the UX audit
// established (single "Cognis:" prefix, unified "Engine" terminology, and
// distinct labels for the six easily-confused commands).

// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required by the ACTION_COMMANDS assertion below. Reading the
// exported map keeps this a static, host-free audit.
import "./testHarness";

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

import { ACTION_COMMANDS } from "../panel";

interface CommandContribution {
  command: string;
  title: string;
  category?: string;
}

interface MenuContribution {
  command?: string;
  when?: string;
  group?: string;
}

interface ConfigurationProperty {
  type?: string | string[];
  default?: unknown;
  description?: string;
}

interface Manifest {
  contributes?: {
    commands?: CommandContribution[];
    menus?: {
      [location: string]: MenuContribution[];
    };
    configuration?: {
      properties?: {
        [key: string]: ConfigurationProperty;
      };
    };
  };
}

// out/test/ -> repo (apps/cognis-vscode) root is two levels up.
const MANIFEST_PATH = path.join(__dirname, "..", "..", "package.json");

function loadCommands(): CommandContribution[] {
  const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8")) as Manifest;
  const commands = manifest.contributes?.commands;
  assert.ok(Array.isArray(commands) && commands.length > 0, "package.json must declare contributes.commands");
  return commands!;
}

function loadViewTitleMenus(): MenuContribution[] {
  const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8")) as Manifest;
  const viewTitle = manifest.contributes?.menus?.["view/title"];
  assert.ok(
    Array.isArray(viewTitle) && viewTitle.length > 0,
    'package.json must declare contributes.menus["view/title"]'
  );
  return viewTitle!;
}

function loadConfigurationProperties(): Record<string, ConfigurationProperty> {
  const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8")) as Manifest;
  const properties = manifest.contributes?.configuration?.properties;
  assert.ok(
    properties && typeof properties === "object",
    "package.json must declare contributes.configuration.properties"
  );
  return properties!;
}

const commands = loadCommands();

function titleFor(commandId: string): string {
  const entry = commands.find((c) => c.command === commandId);
  assert.ok(entry, `command ${commandId} must be declared in contributes.commands`);
  return entry!.title;
}

// R2.2 / R2.4: the Command Palette already prepends `category` ("Cognis"), so a
// title that also starts with "Cognis:" would render the double "Cognis: Cognis:"
// prefix. No title (after trimming leading whitespace) may start with that
// prefix, case-insensitively (covers "Cognis:", "cognis:", "Cognis :").
test("no command title carries a redundant 'Cognis:' prefix", () => {
  const doublePrefix = /^cognis\s*:/i;
  for (const { command, title } of commands) {
    assert.ok(typeof title === "string", `command ${command} must have a string title`);
    assert.doesNotMatch(
      title.replace(/^\s+/, ""),
      doublePrefix,
      `command ${command} title "${title}" must not start with a "Cognis:" prefix`
    );
  }
});

// R3: user-visible titles use the single term "Engine", never "Backend".
test("no command title uses the 'Backend' term", () => {
  for (const { command, title } of commands) {
    assert.doesNotMatch(
      title,
      /backend/i,
      `command ${command} title "${title}" must not contain "Backend"`
    );
  }
});

// R3.2: install (cognis.installBackend) and reinstall (cognis.reinstallEngine)
// share the same noun "Engine" so users see them as symmetric actions.
test("installBackend and reinstallEngine titles both use 'Engine'", () => {
  const install = titleFor("cognis.installBackend");
  const reinstall = titleFor("cognis.reinstallEngine");
  assert.match(install, /engine/i, `installBackend title "${install}" must contain "Engine"`);
  assert.match(reinstall, /engine/i, `reinstallEngine title "${reinstall}" must contain "Engine"`);
});

// R7.4 / R7.5: the six easily-confused commands must each have a unique,
// non-empty label of at most 40 characters so users can tell them apart in the
// Command Palette.
test("the six confusable commands have unique, non-empty labels within 40 chars", () => {
  const confusable = [
    "cognis.clearAndReindex",
    "cognis.coldRestart",
    "cognis.showOutput",
    "cognis.showDiagnostics",
    "cognis.removeFromWorkspace",
    "cognis.prepareUninstall",
  ];

  const seen = new Map<string, string>();
  for (const id of confusable) {
    const title = titleFor(id);
    const trimmed = title.trim();
    assert.ok(trimmed.length > 0, `command ${id} must have a non-empty title`);
    assert.ok(
      title.length <= 40,
      `command ${id} title "${title}" (${title.length} chars) must be at most 40 characters`
    );
    const clash = seen.get(title);
    assert.equal(
      clash,
      undefined,
      `command ${id} title "${title}" duplicates ${clash ?? ""}`
    );
    seen.set(title, id);
  }
});

// R1.7 / R12.1: every command wired into the view/title menu must be a real
// declared command, otherwise the toolbar button dispatches to nothing.
test("every view/title menu command is declared in contributes.commands", () => {
  const declared = new Set(commands.map((c) => c.command));
  const viewTitle = loadViewTitleMenus();
  for (const entry of viewTitle) {
    assert.ok(
      typeof entry.command === "string" && entry.command.length > 0,
      `view/title menu entry must reference a command (got ${JSON.stringify(entry.command)})`
    );
    assert.ok(
      declared.has(entry.command!),
      `view/title menu command "${entry.command}" must be declared in contributes.commands`
    );
  }
});

// R4.1 / R5.6 / R6.3: the three new lifecycle commands must be declared so the
// disconnect, cancel-indexing, and uninstall actions are dispatchable.
test("the three new lifecycle commands are declared in contributes.commands", () => {
  const required = ["cognis.disconnectMcp", "cognis.cancelIndexing", "cognis.uninstallEngine"];
  const declared = new Set(commands.map((c) => c.command));
  for (const id of required) {
    assert.ok(declared.has(id), `command ${id} must be declared in contributes.commands`);
  }
});

// --- extension-minimal-panel: static manifest audit for the minimal panel ---

// R3.1 / R3.2 / R8.6: the Advanced_Mode_Setting must be declared as a boolean
// that defaults off, with a developer-facing description so ordinary users are
// never nudged toward the full detail surface by accident.
test("cognis.advancedMode is a boolean config defaulting to false for developers", () => {
  const properties = loadConfigurationProperties();
  const advancedMode = properties["cognis.advancedMode"];
  assert.ok(
    advancedMode,
    "cognis.advancedMode must be declared in contributes.configuration.properties"
  );
  assert.equal(
    advancedMode.type,
    "boolean",
    `cognis.advancedMode type must be "boolean" (got ${JSON.stringify(advancedMode.type)})`
  );
  assert.equal(
    advancedMode.default,
    false,
    `cognis.advancedMode default must be false (got ${JSON.stringify(advancedMode.default)})`
  );
  assert.ok(
    typeof advancedMode.description === "string" && advancedMode.description.trim().length > 0,
    "cognis.advancedMode must have a non-empty description"
  );
  // The description must signal it is a developer-only toggle that only affects
  // what the panel shows (R3.2).
  assert.match(
    advancedMode.description!,
    /developer/i,
    `cognis.advancedMode description must state it is for developers (got "${advancedMode.description}")`
  );
});

// R8.7 / R1.6: the Start_Cognis_Command must be declared so the unified control
// (in the `off` state) has a real command to dispatch to.
test("cognis.startCognis is declared in contributes.commands", () => {
  const declared = new Set(commands.map((c) => c.command));
  assert.ok(
    declared.has("cognis.startCognis"),
    "command cognis.startCognis must be declared in contributes.commands"
  );
});

// R1.6: the Action_Command_Map must resolve the `startCognis` data-action to
// the declared Start_Cognis_Command so the unified control has no dead button.
test("ACTION_COMMANDS.startCognis maps to cognis.startCognis", () => {
  assert.equal(
    ACTION_COMMANDS.startCognis,
    "cognis.startCognis",
    `ACTION_COMMANDS.startCognis must equal "cognis.startCognis" (got ${JSON.stringify(
      ACTION_COMMANDS.startCognis
    )})`
  );
});

// R7.1 / R7.3: simplifying the panel must not drop any existing command id.
// Every command listed in R7.1 must still be declared so it stays runnable from
// the Command Palette regardless of cognis.advancedMode.
test("all pre-existing command ids are still declared (no regression)", () => {
  const preExisting = [
    "cognis.clearAndReindex",
    "cognis.reinstallEngine",
    "cognis.uninstallEngine",
    "cognis.coldRestart",
    "cognis.removeFromWorkspace",
    "cognis.prepareUninstall",
    "cognis.connectMcp",
    "cognis.disconnectMcp",
    "cognis.startMcpServer",
    "cognis.stopMcpServer",
    "cognis.cancelIndexing",
    "cognis.pauseSync",
    "cognis.resumeSync",
    "cognis.showOutput",
    "cognis.showDiagnostics",
    "cognis.showHealth",
    "cognis.refreshPrerequisites",
    "cognis.installAllPrerequisites",
  ];
  const declared = new Set(commands.map((c) => c.command));
  for (const id of preExisting) {
    assert.ok(
      declared.has(id),
      `pre-existing command ${id} must remain declared in contributes.commands (R7.1/R7.3)`
    );
  }
});
