#!/usr/bin/env node
/**
 * E2E_Harness isolation primitives for the Full_Flow_Scenario
 * (cognis-one-toggle-engine task 19.1).
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 23.1, 23.8–23.11).
 *
 * Every workspace, database, configuration home, and log lives under an
 * Isolation_Root in the OS temp directory. Child processes inherit a scrubbed
 * env that cannot see the real user HOME / MCP config / `.cognis`. Cleanup
 * runs in `finally` (including assertion failure and timeout) and terminates
 * only PIDs this run started.
 *
 * Usage (from repo root):
 *   node tests/e2e/full-flow/harness.mjs --self-test
 *   node tests/e2e/full-flow/scenario.mjs
 *
 * The Full_Flow_Scenario itself is task 19.2 (`scenario.mjs`).
 */

import { spawn, execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir, homedir, platform } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

/** Lease TTL is 30 s; Grace_Period is slightly above that (Requirement 4.6 / 23.6). */
export const GRACE_PERIOD_S = 35;

const ISO_PREFIX = "cognis-full-flow-";

export class ScenarioError extends Error {
  /**
   * @param {string} step
   * @param {unknown} observed
   */
  constructor(step, observed) {
    super(
      `Full_Flow_Scenario failed at step "${step}": ${formatObserved(observed)}`
    );
    this.name = "ScenarioError";
    this.step = step;
    this.observed = observed;
  }
}

function formatObserved(observed) {
  if (observed instanceof Error) {
    return observed.message;
  }
  try {
    return JSON.stringify(observed);
  } catch {
    return String(observed);
  }
}

export function normalizePathKey(p) {
  if (!p) {
    return "";
  }
  try {
    return resolve(p).toLowerCase().replace(/\\/g, "/");
  } catch {
    return String(p).toLowerCase().replace(/\\/g, "/");
  }
}

export function pathIsUnder(child, root) {
  const c = normalizePathKey(child);
  const r = normalizePathKey(root);
  if (!c || !r) {
    return false;
  }
  return c === r || c.startsWith(r.endsWith("/") ? r : `${r}/`);
}

/** Real user locations the harness must never read or write (Requirement 23.9). */
export function realUserSensitivePaths() {
  const home = homedir();
  const appData = process.env.APPDATA || join(home, "AppData", "Roaming");
  return {
    home,
    mcp: [
      join(home, ".cursor", "mcp.json"),
      join(home, ".vscode", "mcp.json"),
      join(appData, "Cursor", "User", "globalStorage", "mcp.json"),
      join(appData, "Code", "User", "globalStorage", "mcp.json"),
    ],
    cognis: [join(home, ".cognis")],
  };
}

function snapshotSensitive(paths) {
  /** @type {Map<string, { exists: boolean, mtimeMs?: number, size?: number }>} */
  const snap = new Map();
  for (const p of paths) {
    try {
      if (!existsSync(p)) {
        snap.set(p, { exists: false });
        continue;
      }
      const st = statSync(p);
      snap.set(p, { exists: true, mtimeMs: st.mtimeMs, size: st.size });
    } catch {
      snap.set(p, { exists: false });
    }
  }
  return snap;
}

function snapshotsEqual(before, after) {
  if (before.size !== after.size) {
    return false;
  }
  for (const [key, a] of before) {
    const b = after.get(key);
    if (!b) {
      return false;
    }
    if (a.exists !== b.exists || a.mtimeMs !== b.mtimeMs || a.size !== b.size) {
      return false;
    }
  }
  return true;
}

/**
 * Create a throwaway Isolation_Root. All workspaces, DBs, config homes, and
 * logs live here. Engine_Binary / Model_Assets / Workspace_Database are left
 * absent so a later Full_Flow_Scenario can start from a fresh install.
 */
export function createIsolationRoot() {
  const root = mkdtempSync(join(tmpdir(), ISO_PREFIX));
  const home = join(root, "home");
  const appData = join(home, "AppData", "Roaming");
  const localAppData = join(home, "AppData", "Local");
  const xdgConfig = join(home, ".config");
  const workspaces = join(root, "workspaces");
  const logs = join(root, "logs");
  const bin = join(root, "bin");
  for (const dir of [home, appData, localAppData, xdgConfig, workspaces, logs, bin]) {
    mkdirSync(dir, { recursive: true });
  }
  for (const rel of [
    join(".cursor", "mcp.json"),
    join(".vscode", "mcp.json"),
    join("AppData", "Roaming", "Cursor", "User", "globalStorage", "mcp.json"),
  ]) {
    const file = join(home, rel);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, `${JSON.stringify({ mcpServers: {} }, null, 2)}\n`, "utf8");
  }
  return {
    root,
    home,
    appData,
    localAppData,
    xdgConfig,
    workspaces,
    logs,
    bin,
  };
}

export function isolationEnv(iso) {
  const scrub = { ...process.env };
  for (const key of Object.keys(scrub)) {
    if (
      key.startsWith("COGNIS_") ||
      key === "USERPROFILE" ||
      key === "HOME" ||
      key === "APPDATA" ||
      key === "LOCALAPPDATA" ||
      key === "XDG_CONFIG_HOME"
    ) {
      delete scrub[key];
    }
  }
  return {
    ...scrub,
    HOME: iso.home,
    USERPROFILE: iso.home,
    APPDATA: iso.appData,
    LOCALAPPDATA: iso.localAppData,
    XDG_CONFIG_HOME: iso.xdgConfig,
  };
}

function killPid(pid) {
  if (!pid || pid === process.pid) {
    return;
  }
  try {
    if (platform() === "win32") {
      execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], {
        stdio: "ignore",
        windowsHide: true,
      });
    } else {
      process.kill(pid, "SIGTERM");
    }
  } catch {
    // Already gone, or not killable — never escalate to an unrelated pid.
  }
}

export class Harness {
  constructor() {
    this.iso = createIsolationRoot();
    /** @type {Set<number>} */
    this.startedPids = new Set();
    /** @type {Array<import("node:child_process").ChildProcess>} */
    this.children = [];
    /** @type {string[]} */
    this.steps = [];
    /** @type {{ step: string, observed: unknown } | null} */
    this.failure = null;
    this.sensitiveBefore = snapshotSensitive([
      ...realUserSensitivePaths().mcp,
      ...realUserSensitivePaths().cognis,
    ]);
    this.removed = false;
  }

  workspacePath(name = "fixture") {
    return join(this.iso.workspaces, name);
  }

  /**
   * Spawn a real child over a real process boundary, inheriting the isolated
   * env. The pid is recorded so cleanup cannot terminate anyone else.
   *
   * @param {string} command
   * @param {string[]} args
   * @param {Record<string, string>} [extraEnv]
   * @param {{ cwd?: string, stdin?: "pipe" | "ignore" }} [options]
   */
  spawn(command, args, extraEnv = {}, options = {}) {
    const stdin = options.stdin === "pipe" ? "pipe" : "ignore";
    const child = spawn(command, args, {
      env: { ...isolationEnv(this.iso), ...extraEnv },
      cwd: options.cwd,
      stdio: [stdin, "pipe", "pipe"],
      windowsHide: true,
      detached: false,
    });
    if (child.pid) {
      this.startedPids.add(child.pid);
    }
    this.children.push(child);
    return child;
  }

  /**
   * Spawn, capture stdout/stderr, and wait for exit. Used for CLI surfaces
   * (`init`, `index`, `health`, `digest`, `mcp-config`).
   *
   * @param {string} command
   * @param {string[]} args
   * @param {Record<string, string>} [extraEnv]
   * @param {{ cwd?: string, timeoutMs?: number }} [options]
   */
  async run(command, args, extraEnv = {}, options = {}) {
    const child = this.spawn(command, args, extraEnv, {
      cwd: options.cwd,
      stdin: "ignore",
    });
    let stdout = "";
    let stderr = "";
    child.stdout?.setEncoding("utf8");
    child.stderr?.setEncoding("utf8");
    child.stdout?.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk;
    });
    const { code, signal } = await waitExit(child, options.timeoutMs ?? 120_000);
    return { code, signal, stdout, stderr, pid: child.pid };
  }

  /**
   * @param {string} name
   * @param {() => unknown | Promise<unknown>} work
   */
  async step(name, work) {
    this.steps.push(name);
    try {
      return await work();
    } catch (error) {
      const observed =
        error instanceof ScenarioError
          ? error.observed
          : { message: error instanceof Error ? error.message : String(error) };
      const wrapped =
        error instanceof ScenarioError
          ? error
          : new ScenarioError(name, observed);
      this.failure = { step: wrapped.step, observed: wrapped.observed };
      throw wrapped;
    }
  }

  /**
   * @param {string} step
   * @param {boolean} condition
   * @param {unknown} observed
   */
  assert(step, condition, observed) {
    if (!condition) {
      throw new ScenarioError(step, observed);
    }
  }

  assertUnderIsolation(step, targetPath) {
    this.assert(step, pathIsUnder(targetPath, this.iso.root), {
      path: targetPath,
      isolationRoot: this.iso.root,
    });
  }

  async terminateStarted() {
    const pids = [...this.startedPids];
    for (const pid of pids) {
      killPid(pid);
    }
    for (const child of this.children) {
      try {
        if (!child.killed && child.exitCode === null && child.pid) {
          if (this.startedPids.has(child.pid)) {
            child.kill();
          }
        }
      } catch {
        // ignore
      }
    }
  }

  removeIsolationRoot() {
    if (this.removed) {
      return;
    }
    try {
      rmSync(this.iso.root, { recursive: true, force: true });
      this.removed = true;
    } catch {
      // OS reclaims temp dirs; do not throw out of finally.
    }
  }

  async cleanup() {
    await this.terminateStarted();
    this.removeIsolationRoot();
    const after = snapshotSensitive([
      ...realUserSensitivePaths().mcp,
      ...realUserSensitivePaths().cognis,
    ]);
    if (!snapshotsEqual(this.sensitiveBefore, after)) {
      // Surface as a harness bug, not a silent leak into the user profile.
      throw new ScenarioError("cleanup: real user MCP/.cognis mutated", {
        before: Object.fromEntries(this.sensitiveBefore),
        after: Object.fromEntries(after),
      });
    }
  }
}

/**
 * Run `body` against a live harness. Isolation_Root is always removed, and
 * only started PIDs are terminated — including on assertion failure or timeout.
 *
 * @param {(harness: Harness) => unknown | Promise<unknown>} body
 * @param {{ timeoutMs?: number }} [options]
 */
export async function runWithHarness(body, options = {}) {
  const harness = new Harness();
  let timer;
  try {
    const work = Promise.resolve(body(harness));
    if (options.timeoutMs && options.timeoutMs > 0) {
      const timeout = new Promise((_, reject) => {
        timer = setTimeout(() => {
          reject(
            new ScenarioError("timeout", {
              timeoutMs: options.timeoutMs,
              steps: harness.steps,
            })
          );
        }, options.timeoutMs);
      });
      await Promise.race([work, timeout]);
    } else {
      await work;
    }
    return harness;
  } catch (error) {
    if (!harness.failure) {
      const step = error instanceof ScenarioError ? error.step : "run";
      const observed =
        error instanceof ScenarioError
          ? error.observed
          : { message: error instanceof Error ? error.message : String(error) };
      harness.failure = { step, observed };
    }
    throw error;
  } finally {
    if (timer) {
      clearTimeout(timer);
    }
    await harness.cleanup();
  }
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function waitExit(child, timeoutMs = 10_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`child pid ${child.pid} did not exit within ${timeoutMs}ms`));
    }, timeoutMs);
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

async function selfTest() {
  const { ok, equal } = await import("node:assert/strict");

  equal(GRACE_PERIOD_S, 35);

  const sensitive = realUserSensitivePaths();
  ok(sensitive.home, "real home is known so we can avoid it");

  // Isolation root is under tmpdir, not the real home.
  const iso = createIsolationRoot();
  try {
    ok(iso.root.includes(ISO_PREFIX), "isolation prefix");
    ok(pathIsUnder(iso.workspaces, iso.root), "workspaces under root");
    ok(pathIsUnder(iso.logs, iso.root), "logs under root");
    ok(pathIsUnder(iso.bin, iso.root), "bin under root");
    ok(!existsSync(join(iso.bin, "cognis")), "Engine_Binary absent at start");
    ok(!existsSync(join(iso.bin, "cognis.exe")), "Engine_Binary absent at start (win)");
    const env = isolationEnv(iso);
    equal(env.HOME, iso.home);
    equal(env.USERPROFILE, iso.home);
    ok(!env.COGNIS_DB_PATH, "no leaked COGNIS_DB_PATH");
    ok(
      normalizePathKey(iso.home) !== normalizePathKey(sensitive.home),
      "isolation home ≠ real user home"
    );
    for (const mcp of sensitive.mcp) {
      ok(!pathIsUnder(mcp, iso.root) || mcp.length === 0, "real MCP path is outside isolation");
    }
  } finally {
    rmSync(iso.root, { recursive: true, force: true });
  }

  // Real process over a real process boundary, writing only under isolation.
  /** @type {string | undefined} */
  let liveRoot;
  await runWithHarness(async (h) => {
    liveRoot = h.iso.root;
    const marker = join(h.iso.logs, "child-ok.txt");
    h.assertUnderIsolation("child log path", marker);
    const child = h.spawn(process.execPath, [
      "-e",
      "require('fs').writeFileSync(process.env.COGNIS_AUDIT_LOG, 'ok', 'utf8')",
    ], { COGNIS_AUDIT_LOG: marker });
    ok(child.pid && h.startedPids.has(child.pid), "spawned pid is tracked");
    await waitExit(child);
    h.assert("child wrote isolation log", existsSync(marker), { marker });
  });
  ok(liveRoot && !existsSync(liveRoot), "Isolation_Root removed after success");

  // Assertion failure records the step + observed values and still removes the root.
  /** @type {string | undefined} */
  let failedRoot;
  /** @type {unknown} */
  let caught;
  try {
    await runWithHarness(async (h) => {
      failedRoot = h.iso.root;
      await h.step("probe-ready", async () => {
        h.assert("probe-ready", false, { state: "Working", want: "Ready_Full" });
      });
    });
  } catch (error) {
    caught = error;
  }
  ok(caught instanceof ScenarioError, "assertion becomes ScenarioError");
  equal(/** @type {ScenarioError} */ (caught).step, "probe-ready");
  equal(
    /** @type {{ state: string }} */ (/** @type {ScenarioError} */ (caught).observed).state,
    "Working"
  );
  ok(failedRoot && !existsSync(failedRoot), "Isolation_Root removed after failed assertion");

  // Timeout still removes the Isolation_Root.
  /** @type {string | undefined} */
  let timedRoot;
  try {
    await runWithHarness(
      async (h) => {
        timedRoot = h.iso.root;
        await sleep(200);
      },
      { timeoutMs: 30 }
    );
  } catch (error) {
    caught = error;
  }
  ok(caught instanceof ScenarioError, "timeout is a ScenarioError");
  equal(/** @type {ScenarioError} */ (caught).step, "timeout");
  ok(timedRoot && !existsSync(timedRoot), "Isolation_Root removed after timeout");

  // Cleanup terminates only started PIDs (the harness pid is never a target).
  await runWithHarness(async (h) => {
    const child = h.spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"]);
    ok(child.pid);
    ok(!h.startedPids.has(process.pid), "harness pid is not in the kill set");
    for (const pid of h.startedPids) {
      ok(pid !== process.pid, "cleanup must not target the harness process");
    }
  });

  console.log("[full-flow harness] self-test OK");
}

const isMain = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isMain) {
  const self = process.argv.includes("--self-test");
  if (!self) {
    console.log(`E2E_Harness primitives (task 19.1). Run with --self-test.
Isolation_Root prefix: ${ISO_PREFIX}
Grace_Period: ${GRACE_PERIOD_S}s
Repo: ${resolve(__dirname, "..", "..", "..")}`);
    process.exit(0);
  }
  selfTest().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}
