// Harness first: installs the vscode stub before mcpConfig.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { resetHarness } from "./testHarness";
import {
  enableMcpForWorkspace,
  hasExpectedMcpConfigForRepo,
  isCognisMcpServerName,
} from "../mcpConfig";
import { ALL_MCP_TOOLS } from "../contract";
import { isHttpServerBlock, isThinProxyServerBlock } from "../mcpServer";

function generatedWarmPolicy(configPath: string): string | undefined {
  const config = JSON.parse(fs.readFileSync(configPath, "utf8")) as {
    mcpServers?: Record<string, { env?: Record<string, string> }>;
  };
  const server = Object.values(config.mcpServers ?? {})[0];
  return server?.env?.COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP;
}

test("shipped extension default emits explicit lazy warm policy while eager stays opt-in", async () => {
  const packageJson = JSON.parse(
    fs.readFileSync(path.resolve(__dirname, "..", "..", "package.json"), "utf8")
  ) as {
    contributes: {
      configuration: {
        properties: Record<string, { default?: unknown }>;
      };
    };
  };
  assert.equal(
    packageJson.contributes.configuration.properties[
      "cognis.mcpWarmSemanticOnStartup"
    ]?.default,
    false,
    "the shipped extension default must be Lazy"
  );

  const lazyRepo = mkRepo("lazy-default");
  const eagerRepo = mkRepo("eager-opt-in");
  try {
    resetHarness(lazyRepo);
    await enableMcpForWorkspace(lazyRepo);
    assert.equal(
      generatedWarmPolicy(path.join(lazyRepo, ".cursor", "mcp.json")),
      "0",
      "extension-generated default must be explicit 0/Lazy"
    );

    resetHarness(eagerRepo, {
      config: { cognis: { mcpWarmSemanticOnStartup: true } },
    });
    await enableMcpForWorkspace(eagerRepo);
    assert.equal(
      generatedWarmPolicy(path.join(eagerRepo, ".cursor", "mcp.json")),
      "1",
      "explicit opt-in must remain 1/Eager"
    );
  } finally {
    fs.rmSync(lazyRepo, { recursive: true, force: true });
    fs.rmSync(eagerRepo, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// Bug facet #1 — Config fan-out (Requirements 1.1, 2.1, 2.11).
//
// This is a BUG-CONDITION EXPLORATION test. It encodes the *expected* (fixed)
// behavior — a single-repo window must start at most one heavy cognis mcpd —
// and therefore MUST FAIL on the unfixed code, where a global mcp.json
// accumulates one heavy stdio `cognis-<slug>` entry per indexed repository and
// every MCP host starts all of them (host × repository fan-out).
// ---------------------------------------------------------------------------

/**
 * Point os.homedir() at a throwaway directory for the duration of a test so the
 * production global-config writes never touch the developer's real
 * ~/.cursor/mcp.json (preservation clause 3.10). Node's os.homedir() reads
 * USERPROFILE on Windows and HOME on POSIX at call time.
 */
function withTempHome(): string {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-fanout-home-"));
  process.env.USERPROFILE = home;
  process.env.HOME = home;
  return home;
}

function mkRepo(tag: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), `cognis-fanout-${tag}-`));
}

/**
 * How many *heavy* cognis daemons a single MCP host would start from a config
 * file: every cognis-named stdio (command) server block that is NOT a
 * model-free thin proxy counts as a heavy `mcpd` (maps the model / holds the
 * repo DB). Thin-proxy blocks (`--proxy` / `COGNIS_MCP_PROXY=1`) are excluded
 * so host×repository connections cost a thin process, not a heavy one
 * (Requirements 2.8, 2.11).
 */
function heavyCognisDaemonCount(configPath: string): number {
  // A config file that was never written contributes no startable daemons.
  if (!fs.existsSync(configPath)) {
    return 0;
  }
  const raw = JSON.parse(fs.readFileSync(configPath, "utf8")) as {
    mcpServers?: Record<string, unknown>;
  };
  const servers = raw.mcpServers ?? {};
  return Object.entries(servers).filter(
    ([name, block]) =>
      isCognisMcpServerName(name) &&
      !isHttpServerBlock(block) &&
      !isThinProxyServerBlock(block)
  ).length;
}

test("a single-repo window starts at most one heavy cognis mcpd (no host×repo fan-out)", async () => {
  const home = withTempHome();
  const repoA = mkRepo("a");
  const repoB = mkRepo("b");
  const repoC = mkRepo("c");

  // With the fix, the default scope is workspace (package.json
  // cognis.mcpConfigScope default = "workspace"): each indexed repo writes its
  // single cognis-<slug> entry into its own repo-local mcp.json, so the shared
  // global config never accumulates host×repo fan-out. We enable three repos
  // exactly as three separately-opened windows would.
  resetHarness(repoA, {
    appName: "Cursor",
    config: { cognis: { mcpHost: "cursor", mcpConfigScope: "workspace" } },
  });
  await enableMcpForWorkspace(repoA);

  resetHarness(repoB, {
    appName: "Cursor",
    config: { cognis: { mcpHost: "cursor", mcpConfigScope: "workspace" } },
  });
  await enableMcpForWorkspace(repoB);

  resetHarness(repoC, {
    appName: "Cursor",
    config: { cognis: { mcpHost: "cursor", mcpConfigScope: "workspace" } },
  });
  await enableMcpForWorkspace(repoC);

  // The single-repo window is repoA: only its own workspace mcp.json feeds the
  // host. Count heavy daemons startable for that window (its workspace config
  // plus any shared global config the host would also read).
  const globalPath = path.join(home, ".cursor", "mcp.json");
  const workspacePath = path.join(repoA, ".cursor", "mcp.json");
  const count =
    heavyCognisDaemonCount(workspacePath) + heavyCognisDaemonCount(globalPath);

  // EXPECTED (fixed): ≤ 1 heavy cognis mcpd for a single active canonical repo.
  // On unfixed code (global default) this was 3 — the host started a heavy
  // daemon for every repo configured globally, regardless of which repo the
  // window had open.
  assert.ok(
    count <= 1,
    `expected ≤1 heavy cognis mcpd for a single-repo window, found ${count} startable (unbounded host×repo fan-out)`
  );

  for (const dir of [home, repoA, repoB, repoC]) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});


test("Kiro workspace MCP config auto-approves the complete advertised Cognis tool set", async () => {
  const kiroRepo = mkRepo("kiro-autoapprove");
  const cursorRepo = mkRepo("cursor-no-autoapprove");
  try {
    resetHarness(kiroRepo, {
      appName: "Kiro",
      config: { cognis: { mcpHost: "kiro", mcpConfigScope: "workspace" } },
    });
    await enableMcpForWorkspace(kiroRepo);
    // Repeated enable must be idempotent and re-canonicalize an existing block.
    await enableMcpForWorkspace(kiroRepo);
    const kiroConfig = JSON.parse(
      fs.readFileSync(path.join(kiroRepo, ".kiro", "settings", "mcp.json"), "utf8")
    ) as { mcpServers: Record<string, { autoApprove?: string[] }> };
    const kiroBlock = Object.values(kiroConfig.mcpServers)[0];
    assert.deepEqual(kiroBlock.autoApprove, [...ALL_MCP_TOOLS]);
    assert.equal(new Set(kiroBlock.autoApprove).size, 8, "autoApprove must contain eight unique tools");

    resetHarness(cursorRepo, {
      appName: "Cursor",
      config: { cognis: { mcpHost: "cursor", mcpConfigScope: "workspace" } },
    });
    await enableMcpForWorkspace(cursorRepo);
    const cursorConfig = JSON.parse(
      fs.readFileSync(path.join(cursorRepo, ".cursor", "mcp.json"), "utf8")
    ) as { mcpServers: Record<string, { autoApprove?: string[] }> };
    const cursorBlock = Object.values(cursorConfig.mcpServers)[0];
    assert.equal(
      cursorBlock.autoApprove,
      undefined,
      "Kiro-specific autoApprove must not leak into another host's config"
    );
  } finally {
    fs.rmSync(kiroRepo, { recursive: true, force: true });
    fs.rmSync(cursorRepo, { recursive: true, force: true });
  }
});


test("Kiro detects and backfills an existing partial autoApprove list", async () => {
  const repo = mkRepo("kiro-autoapprove-backfill");
  try {
    resetHarness(repo, {
      appName: "Kiro",
      config: { cognis: { mcpHost: "kiro", mcpConfigScope: "workspace" } },
    });
    await enableMcpForWorkspace(repo);
    const configPath = path.join(repo, ".kiro", "settings", "mcp.json");
    const config = JSON.parse(fs.readFileSync(configPath, "utf8")) as {
      mcpServers: Record<string, { autoApprove?: string[] }>;
    };
    const block = Object.values(config.mcpServers)[0];
    block.autoApprove = ["symbol_search"];
    fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, "utf8");

    assert.equal(
      await hasExpectedMcpConfigForRepo(repo),
      false,
      "a partial list must trigger config refresh"
    );
    await enableMcpForWorkspace(repo);
    assert.equal(await hasExpectedMcpConfigForRepo(repo), true);

    const refreshed = JSON.parse(fs.readFileSync(configPath, "utf8")) as {
      mcpServers: Record<string, { autoApprove?: string[] }>;
    };
    assert.deepEqual(Object.values(refreshed.mcpServers)[0].autoApprove, [...ALL_MCP_TOOLS]);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});