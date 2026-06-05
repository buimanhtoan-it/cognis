import assert from "node:assert/strict";
import * as path from "node:path";
import test from "node:test";
import {
  getGlobalMcpConfigPath,
  getWorkspaceMcpConfigPath,
  resolveMcpConfigPath,
} from "../mcpConfigPaths";
import { envMatchesExpected, envMatchesRepo } from "../mcpEnv";
import {
  deriveMcpServerName,
  isCognisMcpServerName,
} from "../mcpServerName";
import { buildRepairPlan } from "../repairPlan";
import type { HealthReport } from "../types";

function makeHealth(
  overall: "ok" | "warn" | "fail",
  overrides?: Partial<HealthReport["checks"]>
): HealthReport {
  const ok = { status: "ok", message: "ok" };
  return {
    runtime_version: "0.3.1",
    overall,
    checks: {
      config: ok,
      db: ok,
      index: ok,
      vector: ok,
      embedder: ok,
      version: ok,
      ...overrides,
    },
  };
}

test("deriveMcpServerName builds cognis-<slug>-<hash> from repo folder", () => {
  // Human-readable slug + a stable short hash of the full path.
  assert.match(deriveMcpServerName("D:/PROGRAMING/cognis"), /^cognis-cognis-[0-9a-f]{6}$/);
  assert.match(
    deriveMcpServerName("D:/PROGRAMING/edittruyentranh/edittruyentranh"),
    /^cognis-edittruyentranh-[0-9a-f]{6}$/
  );
  assert.match(deriveMcpServerName("D:/work/My App"), /^cognis-my-app-[0-9a-f]{6}$/);
});

test("deriveMcpServerName disambiguates same-named repos at different paths", () => {
  const a = deriveMcpServerName("D:/work/api");
  const b = deriveMcpServerName("D:/personal/api");
  // Same slug, different hash → no collision in a shared global MCP config.
  assert.notEqual(a, b);
  assert.ok(a.startsWith("cognis-api-"));
  assert.ok(b.startsWith("cognis-api-"));
});

test("deriveMcpServerName is stable and path-normalized", () => {
  // Backslashes vs forward slashes and casing must not change the key, so the
  // extension and CLI always agree and re-runs don't create duplicate entries.
  assert.equal(
    deriveMcpServerName("D:/work/api"),
    deriveMcpServerName("D:\\work\\api")
  );
});

test("isCognisMcpServerName recognizes legacy and named servers", () => {
  assert.equal(isCognisMcpServerName("cognis"), true);
  assert.equal(isCognisMcpServerName("cognis-cognis"), true);
  assert.equal(isCognisMcpServerName("cognis-api-3f9a2c"), true);
  assert.equal(isCognisMcpServerName("brave-search"), false);
});

test("getWorkspaceMcpConfigPath targets repo-local Cursor and VS Code files", () => {
  const repoRoot = "D:/repo";
  // Build expected paths with path.join so the assertion holds on any OS
  // (Windows uses backslashes, POSIX uses forward slashes).
  assert.equal(
    getWorkspaceMcpConfigPath(repoRoot, "cursor"),
    path.join(repoRoot, ".cursor", "mcp.json")
  );
  assert.equal(
    getWorkspaceMcpConfigPath(repoRoot, "vscode"),
    path.join(repoRoot, ".vscode", "mcp.json")
  );
  assert.equal(getWorkspaceMcpConfigPath(repoRoot, "claude"), undefined);
});

test("resolveMcpConfigPath prefers workspace scope for Cursor", () => {
  const repoRoot = "D:/repo";
  const homeDir = "C:/Users/test";
  assert.equal(
    resolveMcpConfigPath("cursor", repoRoot, "workspace", homeDir),
    path.join(repoRoot, ".cursor", "mcp.json")
  );
  assert.equal(
    resolveMcpConfigPath("cursor", repoRoot, "global", homeDir),
    getGlobalMcpConfigPath("cursor", homeDir)
  );
});

test("envMatchesRepo accepts minimal env with only COGNIS_DB_PATH", () => {
  const repoRoot = "D:/repo";
  assert.equal(
    envMatchesRepo(repoRoot, {
      COGNIS_DB_PATH: "D:/repo/.cognis/uckg.db",
    }),
    true
  );
});

test("envMatchesRepo rejects missing or mismatched repo wiring", () => {
  const repoRoot = "D:/repo";
  assert.equal(
    envMatchesRepo(repoRoot, {
      COGNIS_REPO_ROOT: "D:/other",
      COGNIS_DB_PATH: "D:/repo/.cognis/uckg.db",
    }),
    false
  );
  assert.equal(
    envMatchesRepo(repoRoot, {
      COGNIS_DB_PATH: "D:/repo/.cognis/other.db",
    }),
    false
  );
});

test("envMatchesExpected compares only the expected subset", () => {
  assert.equal(
    envMatchesExpected(
      {
        COGNIS_DB_PATH: "D:/repo/.cognis/uckg.db",
        COGNIS_MCP_SOFT_TIMEOUT_S: "30",
      },
      {
        COGNIS_DB_PATH: "D:/repo/.cognis/uckg.db",
      }
    ),
    true
  );
});

test("buildRepairPlan flags bootstrap and MCP repair for missing setup", () => {
  const plan = buildRepairPlan({
    configExists: false,
    mcpConfigured: false,
    health: undefined,
    stateLiveIndexing: true,
    liveIndexingRunning: false,
  });

  assert.deepEqual(plan, {
    needsBootstrap: true,
    needsReindex: false,
    needsMcp: true,
    needsLiveIndexing: true,
    health: undefined,
  });
});

test("buildRepairPlan stays quiet for a healthy configured workspace", () => {
  const plan = buildRepairPlan({
    configExists: true,
    mcpConfigured: true,
    health: makeHealth("ok"),
    stateLiveIndexing: false,
    liveIndexingRunning: false,
  });

  assert.deepEqual(plan, {
    needsBootstrap: false,
    needsReindex: false,
    needsMcp: false,
    needsLiveIndexing: false,
    health: makeHealth("ok"),
  });
});
