// IMPORTANT: the harness import must come first. It installs the `vscode`
// require-hook and the `child_process.spawn` stub, and that has to happen
// before any production module (workspace/indexd/mcpConfig) is required.
import {
  FRESH_INDEXING,
  HEALTHY,
  getDaemonSpawns,
  getSpawnRecords,
  killLiveDaemons,
  noCancelToken,
  resetHarness,
  silentProgress,
} from "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { setupResultGuidance } from "../guidance";
import { getState } from "../state";
import { isWorkspaceConfigured, setupWorkspace } from "../workspace";

function makeFreshRepo(): string {
  // A brand-new user: an empty workspace folder with no `.cognis` directory,
  // exactly what the extension sees the first time it activates after install.
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-fresh-"));
}

function cleanup(repoRoot: string): void {
  killLiveDaemons();
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    // Best effort: temp dirs are reclaimed by the OS anyway.
  }
}

function findDaemonArgValue(args: string[], flag: string): string | undefined {
  const index = args.indexOf(flag);
  return index >= 0 ? args[index + 1] : undefined;
}

test("Set Up Workspace provisions a brand-new workspace end to end", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    assert.equal(isWorkspaceConfigured(repoRoot), false, "precondition: not configured");

    const progress = silentProgress();
    const result = await setupWorkspace(progress, noCancelToken());

    // Config was materialized by `cognis init`.
    assert.equal(isWorkspaceConfigured(repoRoot), true, "config.yaml should exist after setup");

    // Initial cold index runs synchronously (`index --full .`) and live file
    // watching (`cognis indexd`) starts right afterward.
    assert.equal(result.liveIndexingStarted, true);
    assert.equal(result.liveIndexingError, undefined);
    assert.equal(result.indexingInBackground, false);

    // MCP was wired up and the config file written inside the repo.
    assert.ok(result.mcpConfigPath, "mcpConfigPath should be set");
    assert.equal(result.mcpError, undefined);
    assert.equal(
      result.mcpConfigPath,
      path.join(repoRoot, ".cursor", "mcp.json")
    );
    assert.equal(fs.existsSync(result.mcpConfigPath!), true);

    // Workspace state reflects a fully managed, AI-ready workspace.
    const state = getState(repoRoot);
    assert.equal(state.liveIndexing, true);
    assert.equal(state.mcpEnabled, true);
    assert.equal(state.autoManaged, true);
  } finally {
    cleanup(repoRoot);
  }
});

test("fresh setup runs full index rebuild then starts cognis-indexd watcher", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    await setupWorkspace(silentProgress(), noCancelToken());

    const records = getSpawnRecords();
    const fullIndexIdx = records.findIndex(
      (r) => r.args.includes("index") && r.args.includes("--full")
    );
    const daemonIdx = records.findIndex((r) => r.isDaemon);
    assert.ok(fullIndexIdx >= 0, "fresh setup must run `cognis index --full .`");
    assert.ok(daemonIdx > fullIndexIdx, "watcher daemon starts after full index");

    const daemonSpawns = getDaemonSpawns();
    assert.equal(daemonSpawns.length, 1, "exactly one indexer daemon should start");

    const args = daemonSpawns[0].args;
    assert.equal(
      args[0],
      "indexd",
      "daemon runs the binary's indexd surface (<binary> indexd …)"
    );
    assert.equal(
      findDaemonArgValue(args, "--repo-root"),
      repoRoot,
      "daemon points at the workspace repo"
    );
    assert.equal(
      findDaemonArgValue(args, "--db-path"),
      path.join(repoRoot, ".cognis", "uckg.db"),
      "daemon writes the UCKG db inside .cognis"
    );
  } finally {
    cleanup(repoRoot);
  }
});

test("fresh setup orders init before indexing so the daemon sees a config", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    await setupWorkspace(silentProgress(), noCancelToken());

    const records = getSpawnRecords();
    const initIndex = records.findIndex((r) => r.args.includes("init"));
    const daemonIndex = records.findIndex((r) => r.isDaemon);

    assert.ok(initIndex >= 0, "init should run");
    assert.ok(daemonIndex >= 0, "daemon should start");
    assert.ok(
      initIndex < daemonIndex,
      "init must run before the indexer daemon starts"
    );
  } finally {
    cleanup(repoRoot);
  }
});

test("fresh setup surfaces workspace-ready guidance when synchronous index completes", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: HEALTHY });

  try {
    const result = await setupWorkspace(silentProgress(), noCancelToken());
    const guidance = setupResultGuidance(result);

    assert.ok(guidance, "guidance should be produced");
    assert.equal(guidance!.title, "Workspace ready");
    assert.equal(guidance!.severity, "info");
  } finally {
    cleanup(repoRoot);
  }
});

test("setup on an already-healthy configured workspace does not force a rebuild", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: HEALTHY });

  // Pre-provision the workspace as if a previous setup already ran.
  fs.mkdirSync(path.join(repoRoot, ".cognis"), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, ".cognis", "config.yaml"), "version: 1\n", "utf8");

  try {
    const result = await setupWorkspace(silentProgress(), noCancelToken());

    assert.equal(result.indexingInBackground, false);
    assert.equal(result.liveIndexingStarted, true);

    const daemonSpawns = getDaemonSpawns();
    assert.equal(daemonSpawns.length, 1);
    assert.equal(
      daemonSpawns[0].args.includes("--full-rebuild"),
      false,
      "configured healthy workspace should not force a full rebuild"
    );
  } finally {
    cleanup(repoRoot);
  }
});
