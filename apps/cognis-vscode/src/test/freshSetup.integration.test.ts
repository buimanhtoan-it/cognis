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

import { CognisGuidanceError, setupResultGuidance } from "../guidance";
import { getState } from "../state";
import { isWorkspaceConfigured, setupForAi } from "../workspace";

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

test("Set Up for AI provisions a brand-new workspace end to end", async () => {
  const repoRoot = makeFreshRepo();
  // Realistic fresh-user state: the index is still being built, so health
  // reports a warn until the cold index finishes.
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    assert.equal(isWorkspaceConfigured(repoRoot), false, "precondition: not configured");

    const progress = silentProgress();
    const result = await setupForAi(progress, noCancelToken());

    // Config was materialized by `cognis init`.
    assert.equal(isWorkspaceConfigured(repoRoot), true, "config.yaml should exist after setup");

    // Live indexing started, and because this is a fresh repo the work runs in
    // the background (cold rebuild) rather than completing inline.
    assert.equal(result.liveIndexingStarted, true);
    assert.equal(result.liveIndexingError, undefined);
    assert.equal(result.indexingInBackground, true);

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

test("fresh setup starts cognis-indexd with a full rebuild", async () => {
  // This is the core regression guard: on a first-time setup the indexer must
  // be launched as a managed *full* rebuild, otherwise the semantic index is
  // never populated and search silently returns nothing.
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    await setupForAi(silentProgress(), noCancelToken());

    const daemonSpawns = getDaemonSpawns();
    assert.equal(daemonSpawns.length, 1, "exactly one indexer daemon should start");

    const args = daemonSpawns[0].args;
    assert.ok(args.includes("-m"), "daemon launched as a python module");
    assert.ok(
      args.includes("cognis_indexd.main"),
      "daemon runs the cognis_indexd entrypoint"
    );
    assert.ok(
      args.includes("--full-rebuild"),
      "fresh setup must force a full index rebuild"
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
  // Ordering matters: if the daemon spawns before `init` writes config.yaml,
  // the watcher has nothing to anchor on. Assert init runs first.
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    await setupForAi(silentProgress(), noCancelToken());

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

test("fresh setup surfaces background-indexing guidance to the new user", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: FRESH_INDEXING });

  try {
    const result = await setupForAi(silentProgress(), noCancelToken());
    const guidance = setupResultGuidance(result);

    assert.ok(guidance, "guidance should be produced");
    assert.equal(guidance!.title, "Indexing in background");
    assert.equal(guidance!.severity, "info");
  } finally {
    cleanup(repoRoot);
  }
});

test("setup on an already-healthy configured workspace does not force a rebuild", async () => {
  // A returning user with a populated, healthy index should get an incremental
  // watcher, not a destructive full rebuild.
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { health: HEALTHY });

  // Pre-provision the workspace as if a previous setup already ran.
  fs.mkdirSync(path.join(repoRoot, ".cognis"), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, ".cognis", "config.yaml"), "version: 1\n", "utf8");

  try {
    const result = await setupForAi(silentProgress(), noCancelToken());

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

test("setup aborts with guidance when Python is unavailable for a fresh user", async () => {
  // If the new user's Python environment can't run the CLI, setup must fail
  // loudly with guidance and must NOT spawn the indexer or touch the workspace.
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { pathsExitCode: 1 });

  try {
    await assert.rejects(
      () => setupForAi(silentProgress(), noCancelToken()),
      (err: unknown) => {
        assert.ok(err instanceof CognisGuidanceError, "should throw guidance error");
        return true;
      }
    );

    assert.equal(
      getDaemonSpawns().length,
      0,
      "no indexer should start when Python preflight fails"
    );
    assert.equal(
      isWorkspaceConfigured(repoRoot),
      false,
      "workspace config should not be created on preflight failure"
    );
  } finally {
    cleanup(repoRoot);
  }
});
