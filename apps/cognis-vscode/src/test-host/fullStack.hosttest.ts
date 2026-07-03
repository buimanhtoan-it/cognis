/**
 * Full-stack e2e: the real extension, in a real VS Code host, against the real
 * pure-Rust `cognis` engine binary. Runs the actual ``cognis.setupWorkspace``
 * command (no stubs) and asserts the observable outcomes a user would get —
 * plus that the flow is captured in the diagnostics trace, so the monitoring
 * story is verified end to end.
 */
import * as assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";

import * as vscode from "vscode";

const EXTENSION_ID = "ToanBui.cognis-vscode";

function readTrace(): Array<Record<string, unknown>> {
  const dir = process.env.COGNIS_DIAGNOSTICS_DIR;
  if (!dir) {
    return [];
  }
  const file = path.join(dir, "diagnostics.jsonl");
  if (!fs.existsSync(file)) {
    return [];
  }
  return fs
    .readFileSync(file, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line) as Record<string, unknown>);
}

async function waitFor(
  predicate: () => boolean,
  timeoutMs: number,
  intervalMs = 500
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) {
      return true;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return predicate();
}

suite("Full-stack: real VS Code host + real Rust engine binary", () => {
  test("Set Up Workspace creates .cognis + mcp.json and is traced", async function () {
    this.timeout(180_000);

    const workspace = process.env.COGNIS_HOST_WORKSPACE;
    assert.ok(workspace, "COGNIS_HOST_WORKSPACE not set by the runner");
    const binary = (process.env.COGNIS_BINARY_PATH ?? "").trim();
    if (!binary) {
      this.skip(); // No engine binary built — nothing real to drive.
    }

    // Drive a deterministic MCP target; the engine binary is picked up from
    // COGNIS_BINARY_PATH (binary.ts override) so the extension resolves the
    // Rust binary for cli/mcpd/indexd.
    const cfg = vscode.workspace.getConfiguration("cognis");
    await cfg.update("mcpHost", "cursor", vscode.ConfigurationTarget.Workspace);
    await cfg.update("mcpConfigScope", "workspace", vscode.ConfigurationTarget.Workspace);

    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, `extension ${EXTENSION_ID} not found in host`);
    await ext.activate();

    // Run the real flow exactly as the command palette / panel would. Fire it
    // (don't await) and poll the outcomes, so a hang in the flow surfaces as a
    // missing-artifact assertion (with the trace) rather than a bare timeout.
    let commandError: unknown;
    void Promise.resolve(
      vscode.commands.executeCommand("cognis.setupWorkspace")
    ).catch((err) => {
      commandError = err;
    });

    const dumpTrace = (): string => {
      const entries = readTrace().slice(-40);
      return entries.map((e) => JSON.stringify(e)).join("\n");
    };

    // Observable outcome 1: the workspace was provisioned.
    const configYaml = path.join(workspace, ".cognis", "config.yaml");
    const created = await waitFor(() => fs.existsSync(configYaml), 150_000);
    assert.ok(
      created,
      `setupWorkspace did not create .cognis/config.yaml` +
        `${commandError ? ` (command threw: ${String(commandError)})` : ""}\n` +
        `--- diagnostics trace ---\n${dumpTrace()}`
    );

    // Observable outcome 2: a real workspace mcp.json was written (cursor host,
    // workspace scope -> <ws>/.cursor/mcp.json) with a Cognis server entry.
    const mcpPath = path.join(workspace, ".cursor", "mcp.json");
    const mcpWritten = await waitFor(() => fs.existsSync(mcpPath), 30_000);
    assert.ok(
      mcpWritten,
      `setupWorkspace did not write .cursor/mcp.json\n--- diagnostics trace ---\n${dumpTrace()}`
    );
    const mcp = JSON.parse(fs.readFileSync(mcpPath, "utf8")) as {
      mcpServers?: Record<string, unknown>;
    };
    assert.ok(mcp.mcpServers, "mcp.json missing mcpServers");
    const serverNames = Object.keys(mcp.mcpServers);
    assert.ok(
      serverNames.some((n) => n.startsWith("cognis")),
      `mcp.json has no cognis server entry: ${serverNames.join(", ")}`
    );

    // Observable outcome 3: the flow is reconstructable from the trace — the
    // monitoring story, verified end to end through the real host.
    const traced = await waitFor(
      () =>
        readTrace().some(
          (e) =>
            e.scope === "flow" &&
            typeof e.message === "string" &&
            (e.message as string).includes("Set Up Workspace")
        ),
      10_000
    );
    assert.ok(traced, "no 'flow: Set Up Workspace …' entry in diagnostics.jsonl");
  });
});
