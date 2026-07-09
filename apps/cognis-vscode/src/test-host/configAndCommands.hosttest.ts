/**
 * Host test: command registration + `cognis.advancedMode` reactivity, verified
 * against the *real* VS Code API (task 9.1).
 *
 * Unlike the node:test suites under `src/test/` (which stub `vscode`), this runs
 * inside a real VS Code extension host launched by `@vscode/test-electron`, so
 * `vscode.commands.getCommands(true)` and `vscode.workspace.getConfiguration`
 * are the genuine editor surfaces the Command Palette and Settings UI use.
 *
 * It does NOT need the Rust engine binary — command registration and the config
 * listener are pure extension-host wiring — so it never skips.
 *
 * Requirement coverage in this file:
 *  - R7.1/R7.3: every declared `cognis.*` command id (from package.json) is
 *    actually registered, including `cognis.startCognis`.
 *  - R3.4/R7.2/R7.4: that holds at BOTH `advancedMode` values, and hidden-from-
 *    panel commands stay dispatchable from the Command Palette.
 *  - R3.3: toggling `advancedMode` takes effect ≤2s without a window reload
 *    (observed via the config round-trip + the extension staying active and its
 *    command set unchanged — see the comment on that test for why this is the
 *    faithful host-observable proxy for "the panel re-renders in place").
 *  - R9.1: the engine-invocation command surface is unchanged (a cheap check;
 *    the heavy `Rust_Engine_Contract` guarantee is owned by contract.test.ts /
 *    contractParity.test.ts, which remain green).
 *
 * R9.3 (Indexd + MCP server termination on deactivate) is covered by
 * `fullStack.hosttest.ts` ("deactivate cleans up all daemons") and, in the fast
 * harness, by `src/test/activationRegistration.test.ts`.
 */
import * as assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";

import * as vscode from "vscode";

const EXTENSION_ID = "ToanBui.cognis-vscode";

/** Read the declared `cognis.*` command ids straight from the manifest. */
function declaredCommandIds(): string[] {
  // out/test-host -> out -> apps/cognis-vscode/package.json
  const pkgPath = path.resolve(__dirname, "..", "..", "package.json");
  const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8")) as {
    contributes?: { commands?: Array<{ command?: string }> };
  };
  const ids = (pkg.contributes?.commands ?? [])
    .map((c) => c.command ?? "")
    .filter((id) => id.startsWith("cognis."));
  assert.ok(ids.length > 0, "package.json declared no cognis.* commands");
  return ids;
}

async function setAdvancedMode(value: boolean): Promise<void> {
  await vscode.workspace
    .getConfiguration("cognis")
    .update("advancedMode", value, vscode.ConfigurationTarget.Workspace);
}

async function assertAllCommandsRegistered(context: string): Promise<void> {
  const declared = declaredCommandIds();
  const registered = new Set(await vscode.commands.getCommands(true));
  for (const id of declared) {
    assert.ok(
      registered.has(id),
      `command ${id} is not registered (${context}) — Command Palette would be missing it`
    );
  }
  // R1.6/R7.3: the new one-click command is present alongside the legacy ids.
  assert.ok(
    registered.has("cognis.startCognis"),
    `cognis.startCognis must be registered (${context})`
  );
}

suite("Host: command registration + advancedMode reactivity", () => {
  suiteSetup(async function () {
    this.timeout(60_000);
    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, `extension ${EXTENSION_ID} not found in host`);
    await ext.activate();
  });

  suiteTeardown(async () => {
    // Leave the workspace setting as the manifest default so we don't persist
    // test state into the throwaway workspace.
    await setAdvancedMode(false);
  });

  test("every declared cognis.* command is registered with advancedMode=false (R7.1/R7.3/R7.2)", async function () {
    this.timeout(30_000);
    await setAdvancedMode(false);
    await assertAllCommandsRegistered("advancedMode=false");
  });

  test("every declared cognis.* command is registered with advancedMode=true (R3.4/R7.4)", async function () {
    this.timeout(30_000);
    await setAdvancedMode(true);
    // R7.4: commands hidden from the minimal panel are still dispatchable from
    // the Command Palette. getCommands(true) is exactly the Palette's source of
    // truth, so their presence here proves they remain runnable regardless of
    // which surface the panel renders.
    await assertAllCommandsRegistered("advancedMode=true");
  });

  test("commands hidden from the minimal panel stay dispatchable when advancedMode is false (R7.4)", async function () {
    this.timeout(30_000);
    await setAdvancedMode(false);
    // With advancedMode off, the panel hides its advanced surfaces, but the
    // corresponding commands must remain runnable from the Command Palette and
    // keybindings. `vscode.commands.getCommands(true)` is precisely the set the
    // Command Palette can dispatch, so membership here is the faithful proxy
    // for "still activatable" — without invoking the handlers, whose side
    // effects (engine health, modals) are not what R7.4 is about and would
    // require a fully provisioned engine.
    const registered = new Set(await vscode.commands.getCommands(true));
    for (const hidden of [
      "cognis.showHealth",
      "cognis.clearAndReindex",
      "cognis.reinstallEngine",
      "cognis.coldRestart",
      "cognis.startMcpServer",
      "cognis.cancelIndexing",
      "cognis.showOutput",
      "cognis.showDiagnostics",
    ]) {
      assert.ok(
        registered.has(hidden),
        `panel-hidden command ${hidden} must stay dispatchable from the Command Palette with advancedMode off (R7.4)`
      );
    }
  });

  test("toggling advancedMode takes effect ≤2s without reloading the window (R3.3)", async function () {
    this.timeout(30_000);
    await setAdvancedMode(false);

    // The panel is a webview owned by the provider; its rendered HTML is not
    // directly observable from the host harness. The faithful, observable proxy
    // for "the panel re-renders in place within 2s without a reload" is:
    //   (a) the config change is applied within 2s (the onDidChangeConfiguration
    //       handler is what re-renders — see extension.ts updateStatusBar),
    //   (b) the extension stays active (no reload/re-activation), and
    //   (c) the registered command set is unchanged (no reload, and R3.4).
    const cfg = () =>
      vscode.workspace
        .getConfiguration("cognis")
        .get<boolean>("advancedMode", false);

    const commandsBefore = (await vscode.commands.getCommands(true)).filter((c) =>
      c.startsWith("cognis.")
    );

    const start = Date.now();
    await setAdvancedMode(true);
    // Wait until the new value is observable, bounded to 2s.
    const deadline = start + 2000;
    while (cfg() !== true && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 25));
    }
    const elapsed = Date.now() - start;

    assert.equal(cfg(), true, "advancedMode should read back as true after update");
    assert.ok(elapsed <= 2000, `advancedMode change took ${elapsed}ms, must be ≤2000ms`);

    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext?.isActive, "extension must remain active (no reload) after toggle");

    const commandsAfter = (await vscode.commands.getCommands(true)).filter((c) =>
      c.startsWith("cognis.")
    );
    assert.deepEqual(
      [...commandsAfter].sort(),
      [...commandsBefore].sort(),
      "toggling advancedMode must not change which commands are registered (R3.4, no reload)"
    );
  });

  test("engine-invocation command surface is unchanged (R9.1)", async () => {
    // The Rust_Engine_Contract is exercised in depth by contract.test.ts and
    // contractParity.test.ts (kept green). As a cheap host-side guard, assert
    // the commands that drive the engine still exist and were not renamed as a
    // side effect of the panel simplification.
    const registered = new Set(await vscode.commands.getCommands(true));
    for (const id of [
      "cognis.setupWorkspace",
      "cognis.installBackend",
      "cognis.clearAndReindex",
      "cognis.connectMcp",
      "cognis.startMcpServer",
      "cognis.startCognis",
    ]) {
      assert.ok(
        registered.has(id),
        `engine-driving command ${id} must be unchanged/registered (R9.1)`
      );
    }
  });
});
