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

/** True when the process is gone — `process.kill(pid, 0)` throws (ESRCH). */
function isDead(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return false;
  } catch {
    return true;
  }
}

/** Parsed `.cognis/indexd-status.json`, or undefined if missing/empty/invalid. */
function readIndexdStatus(
  workspace: string
): { pid?: number; active?: boolean } | undefined {
  const statusFile = path.join(workspace, ".cognis", "indexd-status.json");
  if (!fs.existsSync(statusFile)) {
    return undefined;
  }
  try {
    const raw = JSON.parse(fs.readFileSync(statusFile, "utf8")) as {
      pid?: unknown;
      active?: unknown;
    };
    return {
      pid: typeof raw.pid === "number" ? raw.pid : undefined,
      active: raw.active === true,
    };
  } catch {
    return undefined;
  }
}

/**
 * The indexd pid recorded in the status file, if it carries a positive `pid`.
 * (Liveness is checked separately via {@link isDead}: the daemon may run a
 * short one-shot pass and exit, so the recorded pid can be present but dead.)
 */
function readIndexdPid(workspace: string): number | undefined {
  const pid = readIndexdStatus(workspace)?.pid;
  return pid !== undefined && pid > 0 ? pid : undefined;
}

/** True when the status file advertises a daemon that is `active` and alive. */
function hasLiveIndexd(workspace: string): boolean {
  const status = readIndexdStatus(workspace);
  const pid = status?.pid;
  return Boolean(status?.active && pid && pid > 0 && !isDead(pid));
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

  test("deactivate cleans up all daemons (no orphaned indexd)", async function () {
    this.timeout(180_000);

    const workspace = process.env.COGNIS_HOST_WORKSPACE;
    assert.ok(workspace, "COGNIS_HOST_WORKSPACE not set by the runner");
    const binary = (process.env.COGNIS_BINARY_PATH ?? "").trim();
    if (!binary) {
      this.skip(); // No engine binary built — nothing real to drive.
    }

    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, `extension ${EXTENSION_ID} not found in host`);
    await ext.activate();

    // Provision the workspace so indexd runs and records its pid (design D
    // step 1). Fire the command without awaiting (mirroring the setup test):
    // the flow can surface a blocking prompt in the headless host, so we drive
    // it and poll the observable outcome instead of hanging on the promise.
    // In this throwaway workspace the daemon may run a short one-shot pass and
    // exit, so the recorded pid can be present but already dead.
    void Promise.resolve(
      vscode.commands.executeCommand("cognis.setupWorkspace")
    ).catch(() => {
      /* outcome is polled below */
    });

    // Best-effort: capture the indexd pid if/when it shows up (bounded wait).
    // Missing it is acceptable — the orphan check below stands on its own.
    await waitFor(() => readIndexdPid(workspace) !== undefined, 60_000);
    const pid = readIndexdPid(workspace);

    // Trigger cleanup: cancelIndexing → stopLive → stopLiveIndexing terminates
    // the indexd process by pid (exercises pid-based termination incl. pid-only
    // handles) within 5s. Fire-and-poll for the same hang-avoidance reason;
    // the pid-kill in stopLive runs synchronously before any UI feedback.
    void Promise.resolve(
      vscode.commands.executeCommand("cognis.cancelIndexing")
    ).catch(() => {
      /* outcome is polled below */
    });

    // Core invariant (R10.1, R12.9): within ≤5s no live (active + alive) indexd
    // daemon remains for this workspace — no orphan survives cleanup.
    const noOrphan = await waitFor(() => !hasLiveIndexd(workspace), 5_000, 200);
    assert.ok(
      noOrphan,
      `a live indexd daemon still runs 5s after cancelIndexing — orphaned daemon ` +
        `(status: ${JSON.stringify(readIndexdStatus(workspace))})`
    );

    // If a concrete pid was observed, assert that specific process is dead too
    // (process.kill(pid,0) throws ESRCH), exercising the pid-based termination.
    if (typeof pid === "number" && pid > 0) {
      const dead = await waitFor(() => isDead(pid), 5_000, 200);
      assert.ok(
        dead,
        `indexd pid ${pid} still alive 5s after cancelIndexing — orphaned daemon`
      );
    }
  });
});
