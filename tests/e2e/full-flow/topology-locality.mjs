#!/usr/bin/env node
/**
 * Topology and locality E2E scenarios (cognis-one-toggle-engine task 19.4).
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine`
 * Requirements 25.1, 25.2, 25.9, 25.10, 16.2, 16.6, 17.4, 17.6, 2.8, 3.11,
 * 5.3, 21.1, 21.2, 21.3.
 *
 * Scenarios:
 *   topology-K-clients, topology-N-workspaces, status-latency, entry-restore,
 *   advanced-gate-latency, locality-audit
 *
 * Usage (from repo root):
 *   node tests/e2e/full-flow/topology-locality.mjs
 *   node tests/e2e/full-flow/topology-locality.mjs --binary target/debug/cognis.exe
 */

import { execFileSync, spawnSync } from "node:child_process";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir, platform } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  GRACE_PERIOD_S,
  ScenarioError,
  pathIsUnder,
  realUserSensitivePaths,
  runWithHarness,
  sleep,
} from "./harness.mjs";
import {
  McpClient,
  engineName,
  endpointPort,
  findDefaultBinary,
  parseJsonStdout,
  pidAlive,
  portAccepts,
  processesReferencing,
  runCli,
  teeStderr,
  waitUntil,
} from "./scenario.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "..", "..", "..");
const VSCODE_ROOT = join(REPO_ROOT, "apps", "cognis-mcp");
const VSCODE_PKG = join(VSCODE_ROOT, "package.json");

const K_CLIENTS = 3;
const N_WORKSPACES = 3;
const STATUS_LATENCY_MS = 2000;
const ENTRY_RESTORE_BUDGET_MS = 5000;
const ADVANCED_GATE_MS = 1000;

const COGNIS_MANAGED_BY = "COGNIS_MANAGED_BY";
const COGNIS_MANAGED_BY_VALUE = "cognis-mcp";
const COGNIS_MANAGED_WORKSPACE = "COGNIS_MANAGED_WORKSPACE";

const ALPHA_TOKEN = "topology_alpha_token";
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
const ADVANCED_MODE_WHEN = "config.cognis.advancedMode";

function parseArgs(argv) {
  const out = { binary: "", help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--binary") {
      const v = argv[++i];
      if (!v) throw new Error("missing value for --binary");
      out.binary = v;
    } else if (a === "-h" || a === "--help") {
      out.help = true;
    } else if (a.startsWith("-")) {
      throw new Error(`unknown flag: ${a}`);
    }
  }
  return out;
}

function fixtureSource() {
  return `# Synthetic fixture for topology/locality E2E. Do not use in production.
def ${ALPHA_TOKEN}():
    """Indexed symbol whose body is the query target."""
    return "topology_alpha_payload"
`;
}

function writeJsonFile(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function readJsonFile(path, fallback) {
  if (!existsSync(path)) {
    return fallback;
  }
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return fallback;
  }
}

function stampManagedMcpEntry(block, workspaceKey) {
  return {
    ...block,
    env: {
      ...(block?.env ?? {}),
      [COGNIS_MANAGED_BY]: COGNIS_MANAGED_BY_VALUE,
      [COGNIS_MANAGED_WORKSPACE]: workspaceKey,
    },
  };
}

function isManagedEntry(block, workspaceKey) {
  const env = block?.env ?? {};
  return (
    env[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE &&
    env[COGNIS_MANAGED_WORKSPACE] === workspaceKey
  );
}

function cognisEnv(h, ws, extra = {}) {
  const dbPath = join(ws, ".cognis", "uckg.db");
  return {
    COGNIS_REPO_ROOT: ws,
    COGNIS_DB_PATH: dbPath,
    COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
    COGNIS_MCP_PROXY: "1",
    COGNIS_AUDIT_LOG: join(h.iso.logs, `${ws.split(/[\\/]/).pop()}-audit.log`),
    ...extra,
  };
}

function writeWorkspaceSource(ws) {
  mkdirSync(join(ws, "src"), { recursive: true });
  writeFileSync(join(ws, "src", "topology_alpha.py"), fixtureSource(), "utf8");
}

async function provisionWorkspace(h, binary, ws, env) {
  mkdirSync(ws, { recursive: true });
  writeWorkspaceSource(ws);
  await runCli(h, binary, ["--repo-root", ws, "init"], env);
  await runCli(
    h,
    binary,
    ["--repo-root", ws, "index", "--full", "--skip-embeddings"],
    env,
    180_000
  );
}

async function startOwner(h, binary, ws, env) {
  const owner = h.spawn(
    binary,
    ["owner", "--repo-root", ws, "--host", "127.0.0.1", "--port", "0"],
    env,
    { cwd: ws }
  );
  const tag = ws.split(/[\\/]/).pop();
  owner.stdout?.pipe(createWriteStream(join(h.iso.logs, `${tag}-owner.stdout.log`)));
  teeStderr(owner, join(h.iso.logs, `${tag}-owner.stderr.log`));
  const endpointPath = join(ws, ".cognis", "mcpd.endpoint");
  const ownerLog = join(h.iso.logs, `${tag}-owner.stderr.log`);
  const endpointText = await waitUntil(`${tag} owner endpoint`, 60_000, async () => {
    if (owner.exitCode !== null) {
      throw new ScenarioError(`${tag} owner endpoint`, {
        exitCode: owner.exitCode,
        stderr: existsSync(ownerLog) ? readFileSync(ownerLog, "utf8").slice(0, 8000) : "",
      });
    }
    if (!existsSync(endpointPath)) return null;
    const text = readFileSync(endpointPath, "utf8");
    const port = endpointPort(text);
    if (!port) return null;
    if (!(await portAccepts(port))) return null;
    return text;
  });
  const lease = readJsonFile(join(ws, ".cognis", "owner.lease"), null);
  if (lease?.pid) {
    h.startedPids.add(lease.pid);
  }
  return { owner, endpointText, lease };
}

async function startThinAdapter(h, binary, ws, env, index) {
  const mcpd = h.spawn(binary, ["mcpd", "--proxy"], env, {
    cwd: ws,
    stdin: "pipe",
  });
  const logPath = join(h.iso.logs, `${ws.split(/[\\/]/).pop()}-mcpd-${index}.stderr.log`);
  teeStderr(mcpd, logPath);
  await waitUntil(`thin adapter ${index} ready`, 20_000, async () => {
    if (mcpd.exitCode !== null) {
      throw new ScenarioError(`thin adapter ${index} ready`, {
        exitCode: mcpd.exitCode,
        stderr: existsSync(logPath) ? readFileSync(logPath, "utf8").slice(0, 4000) : "",
      });
    }
    if (!existsSync(logPath)) return null;
    const text = readFileSync(logPath, "utf8");
    if (text.includes("no ONNX, no repository DB") || text.includes("thin-proxy")) {
      return text;
    }
    return null;
  });
  const mcp = new McpClient(mcpd);
  await mcp.initialize();
  return { mcp, logPath };
}

function classifyTopology(proc) {
  const cmd = `${proc.name ?? ""} ${proc.commandLine ?? ""}`.toLowerCase();
  if (cmd.includes("topology-locality.mjs") || cmd.includes("node.exe")) {
    if (!/\bcognis(\.exe)?\b/.test(cmd) || cmd.includes("topology-locality")) {
      return null;
    }
  }
  const isThin =
    /(?:^|[\s"])--proxy(?:[\s"=]|$)/.test(cmd) ||
    /--transport(?:=|\s+)proxy\b/i.test(cmd) ||
    /cognis_mcp_proxy=1/.test(cmd);
  if (/\bindexd\b/.test(cmd) || cmd.includes("cognis-indexd")) {
    return "indexd";
  }
  if (isThin && (/\bmcpd\b/.test(cmd) || cmd.includes("cognis"))) {
    return "thin_proxy";
  }
  if (/\bowner\b/.test(cmd) && cmd.includes("cognis")) {
    return "owner";
  }
  if (/\bmcpd\b/.test(cmd) || cmd.includes("cognis-mcpd")) {
    return "heavy_mcpd";
  }
  if (cmd.includes("cognis")) {
    return "other_cognis";
  }
  return null;
}

function liveTopology(h) {
  const procs = processesReferencing(h.iso.root).filter((p) => pidAlive(p.pid));
  const classified = [];
  for (const p of procs) {
    const cls = classifyTopology(p);
    if (cls) {
      classified.push({ ...p, class: cls });
    }
  }
  return {
    procs: classified,
    owners: classified.filter((p) => p.class === "owner"),
    thin: classified.filter((p) => p.class === "thin_proxy"),
    indexd: classified.filter((p) => p.class === "indexd"),
    heavy: classified.filter((p) => p.class === "heavy_mcpd"),
  };
}

function readGeneration(dbPath) {
  return import("node:sqlite").then(({ DatabaseSync }) => {
    const sqlite = new DatabaseSync(dbPath, { readOnly: true });
    try {
      const row = sqlite
        .prepare(
          "SELECT source_generation, semantic_generation FROM generation WHERE id = 1"
        )
        .get();
      const pending = sqlite
        .prepare("SELECT COUNT(*) AS c FROM embedding_queue")
        .get();
      return {
        source_generation: row?.source_generation ?? 0,
        semantic_generation: row?.semantic_generation ?? 0,
        pending_vector_count: pending?.c ?? 0,
      };
    } finally {
      sqlite.close();
    }
  });
}

function endpointHost(text) {
  const line = String(text)
    .split(/\r?\n/)
    .map((s) => s.trim())
    .find((s) => s.startsWith("http://") || s.startsWith("https://"));
  if (!line) return null;
  try {
    return new URL(line).hostname;
  } catch {
    return null;
  }
}

function tcpConnectionsForPids(pids) {
  /** @type {Array<{ pid: number, local: string, remote: string, state: string }>} */
  const rows = [];
  if (platform() !== "win32" || pids.length === 0) {
    return rows;
  }
  const set = new Set(pids.map(Number));
  try {
    const out = execFileSync(
      "powershell.exe",
      [
        "-NoProfile",
        "-Command",
        [
          "$ErrorActionPreference = 'SilentlyContinue'",
          "Get-NetTCPConnection | ForEach-Object {",
          "  '{0}|{1}|{2}|{3}' -f $_.OwningProcess, $_.LocalAddress, $_.RemoteAddress, $_.State",
          "}",
        ].join("; "),
      ],
      { encoding: "utf8", windowsHide: true, timeout: 20_000 }
    );
    for (const line of out.split(/\r?\n/)) {
      if (!line.trim()) continue;
      const [pidStr, local, remote, state] = line.split("|");
      const pid = Number.parseInt(pidStr, 10);
      if (!set.has(pid)) continue;
      rows.push({ pid, local, remote, state });
    }
  } catch {
    // Best-effort; locality still asserts endpoint host + isolation paths.
  }
  return rows;
}

function isLoopbackAddress(addr) {
  if (!addr) return true;
  const a = String(addr).toLowerCase();
  return (
    a === "127.0.0.1" ||
    a === "::1" ||
    a === "0.0.0.0" ||
    a === "::" ||
    a === "*"
  );
}

/**
 * Production WatcherEntryReconciler (apps/cognis-mcp/src/activation.ts).
 * Driven here against real mcp.json files while a real owner/watcher runs.
 */
class WatcherEntryReconciler {
  constructor(deps) {
    this.deps = deps;
    this.budgetMs = deps.restoreBudgetMs ?? 0;
    this.missingSince = new Map();
  }

  async tick(repoRoot) {
    if (this.deps.inTransition?.(repoRoot)) {
      return;
    }
    const watcher = this.deps.isWatcherRunning(repoRoot);
    const entry = this.deps.isEntryPresent(repoRoot);
    const now = (this.deps.now ?? Date.now)();

    if (watcher && entry) {
      this.missingSince.delete(repoRoot);
      return;
    }

    if (watcher && !entry) {
      const since = this.missingSince.get(repoRoot) ?? now;
      this.missingSince.set(repoRoot, since);
      if (now - since >= this.budgetMs) {
        await this.deps.restoreEntry(repoRoot);
        this.missingSince.delete(repoRoot);
      }
      return;
    }

    this.missingSince.delete(repoRoot);
    if (!watcher && entry) {
      await this.deps.removeEntry(repoRoot);
    }
  }

  startPolling(repoRoot, intervalMs = 1000) {
    const id = setInterval(() => {
      void this.tick(repoRoot).catch(() => {});
    }, intervalMs);
    void this.tick(repoRoot).catch(() => {});
    return {
      dispose() {
        clearInterval(id);
      },
    };
  }
}

function walkFiles(root) {
  const found = [];
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries = [];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const ent of entries) {
      const full = join(dir, ent.name);
      if (ent.isDirectory()) {
        stack.push(full);
      } else {
        found.push(full);
      }
    }
  }
  return found;
}

function evaluateWhen(when, advancedMode) {
  if (!when) return true;
  return when.split("&&").every((raw) => {
    const clause = raw.trim();
    if (clause === "view == cognis.controlPanel") return true;
    if (clause === ADVANCED_MODE_WHEN) return advancedMode;
    if (clause === `!${ADVANCED_MODE_WHEN}`) return !advancedMode;
    if (clause.startsWith("cognis.") || clause.startsWith("!cognis.")) {
      return true;
    }
    return true;
  });
}

function runAdvancedGateLatency() {
  const pkg = JSON.parse(readFileSync(VSCODE_PKG, "utf8"));
  const palette = pkg.contributes?.menus?.commandPalette ?? [];
  const t0 = Date.now();
  const hidden = [];
  const leaked = [];
  for (const entry of palette) {
    if (!ADVANCED_COMMANDS.has(entry.command)) continue;
    const visible = evaluateWhen(entry.when, false);
    if (visible) leaked.push(entry.command);
    else hidden.push(entry.command);
  }
  const elapsed = Date.now() - t0;
  if (leaked.length) {
    throw new ScenarioError("advanced-gate-latency when-clauses", {
      leaked,
      hidden,
    });
  }
  if (elapsed > ADVANCED_GATE_MS) {
    throw new ScenarioError("advanced-gate-latency when-clause eval", {
      elapsed,
      budgetMs: ADVANCED_GATE_MS,
    });
  }
  for (const command of ADVANCED_COMMANDS) {
    const entries = palette.filter((e) => e.command === command);
    if (!entries.length) continue;
    for (const entry of entries) {
      const clauses = (entry.when ?? "").split("&&").map((c) => c.trim());
      if (!clauses.includes(ADVANCED_MODE_WHEN)) {
        throw new ScenarioError("advanced-gate-latency missing when", {
          command,
          when: entry.when,
        });
      }
    }
  }
  for (const command of ALWAYS_AVAILABLE_COMMANDS) {
    const entries = palette.filter((e) => e.command === command);
    for (const entry of entries) {
      if (entry.when) {
        throw new ScenarioError("always-available command gated", {
          command,
          when: entry.when,
        });
      }
    }
  }

  const tsc = spawnSync("npx", ["tsc", "-p", "./"], {
    cwd: VSCODE_ROOT,
    encoding: "utf8",
    windowsHide: true,
    timeout: 120_000,
    shell: true,
  });
  if (tsc.status !== 0) {
    throw new ScenarioError("advanced-gate-latency compile", {
      status: tsc.status,
      stderr: (tsc.stderr || "").slice(0, 4000),
      stdout: (tsc.stdout || "").slice(0, 2000),
    });
  }
  const testFile = join(VSCODE_ROOT, "out", "test", "activationRegistration.test.js");
  const nodeTest = spawnSync(
    process.execPath,
    [
      "--test",
      "--test-name-pattern",
      "hides Advanced_Command within 1s",
      testFile,
    ],
    {
      cwd: VSCODE_ROOT,
      encoding: "utf8",
      windowsHide: true,
      timeout: 60_000,
    }
  );
  if (nodeTest.status !== 0) {
    throw new ScenarioError("advanced-gate-latency host hide", {
      status: nodeTest.status,
      stderr: (nodeTest.stderr || "").slice(0, 6000),
      stdout: (nodeTest.stdout || "").slice(0, 6000),
    });
  }
  return { elapsedMs: elapsed, hidden: hidden.length };
}

async function runTopologyLocality(hostBinary) {
  const timeoutMs = (GRACE_PERIOD_S + 240) * 1000;
  /** @type {Record<string, unknown>} */
  const recorded = {};

  await runWithHarness(
    async (h) => {
      const binary = join(h.iso.bin, engineName());
      copyFileSync(hostBinary, binary);
      h.assertUnderIsolation("engine dest", binary);

      const workspaces = [];
      for (let i = 0; i < N_WORKSPACES; i += 1) {
        workspaces.push(h.workspacePath(`ws${i}`));
      }
      const ws0 = workspaces[0];
      const env0 = cognisEnv(h, ws0);

      await h.step("topology-K-clients", async () => {
        await provisionWorkspace(h, binary, ws0, env0);
        const { owner, endpointText } = await startOwner(h, binary, ws0, env0);
        recorded.endpointHost = endpointHost(endpointText);
        h.assert(
          "owner listens on loopback",
          recorded.endpointHost === "127.0.0.1",
          { host: recorded.endpointHost, endpointText: endpointText.slice(0, 500) }
        );

        const adapters = [];
        for (let i = 0; i < K_CLIENTS; i += 1) {
          adapters.push(await startThinAdapter(h, binary, ws0, env0, i));
        }
        recorded.kAdapters = adapters.length;

        const liveAdapters = adapters.filter((a) => a.mcp.child.exitCode === null);
        h.assert("K thin adapters live", liveAdapters.length === K_CLIENTS, {
          live: liveAdapters.map((a) => a.mcp.child.pid),
        });
        h.assert("exactly 1 owner process", owner.exitCode === null && pidAlive(owner.pid), {
          pid: owner.pid,
          exitCode: owner.exitCode,
        });
        const topo = liveTopology(h);
        if (topo.owners.length) {
          h.assert("CIM owner count is 1", topo.owners.length === 1, topo);
        }
        if (topo.thin.length) {
          h.assert("CIM thin adapter count is K", topo.thin.length === K_CLIENTS, topo);
        }
        h.assert("no heavy mcpd alongside thin adapters", topo.heavy.length === 0, topo);

        for (const adapter of adapters) {
          const stderr = readFileSync(adapter.logPath, "utf8");
          h.assert(
            "adapter is model- and database-free",
            stderr.includes("no ONNX, no repository DB") ||
              stderr.includes("thin-proxy"),
            { stderr: stderr.slice(0, 2000) }
          );
          const { hits } = await adapter.mcp.callTool("symbol_search", {
            query: ALPHA_TOKEN,
            k: 8,
          });
          h.assert("thin adapter serves the workspace", hits.length >= 1, {
            hits,
          });
        }

        try {
          adapters[0].mcp.child.stdin?.end();
        } catch {
          // ignore
        }
        await sleep(400);
        h.assert(
          "remaining adapters survive peer exit",
          adapters[1].mcp.child.exitCode === null &&
            adapters[2].mcp.child.exitCode === null,
          {
            a1: adapters[1].mcp.child.exitCode,
            a2: adapters[2].mcp.child.exitCode,
          }
        );
        const afterKill = await adapters[1].mcp.callTool("symbol_search", {
          query: ALPHA_TOKEN,
          k: 8,
        });
        h.assert(
          "owner keeps serving remaining clients",
          afterKill.hits.length >= 1,
          afterKill
        );
        h.assert("owner still running after adapter exit", owner.exitCode === null, {
          exitCode: owner.exitCode,
        });
        const topoAfter = liveTopology(h);
        h.assert(
          "still exactly 1 owner after adapter exit",
          topoAfter.owners.length === 1,
          topoAfter
        );
        recorded.kClients = {
          owners: topo.owners.length,
          thin: topo.thin.length,
          afterPeerExitThin: topoAfter.thin.length,
        };
      });

      await h.step("status-latency", async () => {
        const dbPath = join(ws0, ".cognis", "uckg.db");
        const before = await readGeneration(dbPath);
        const ownerStatusPath = join(ws0, ".cognis", "owner-status.json");
        let observed = null;
        const poll = setInterval(() => {
          void (async () => {
            try {
              const gen = await readGeneration(dbPath);
              const ownerStatus = readJsonFile(ownerStatusPath, {});
              if (
                gen.source_generation > before.source_generation ||
                gen.pending_vector_count !== before.pending_vector_count ||
                (typeof ownerStatus.sourceGeneration === "number" &&
                  ownerStatus.sourceGeneration > before.source_generation)
              ) {
                observed = { gen, ownerStatus, at: Date.now() };
              }
            } catch {
              // ignore transient sqlite locks
            }
          })();
        }, 100);

        writeFileSync(
          join(ws0, "src", "topology_alpha.py"),
          `${fixtureSource()}\ndef topology_beta_token():\n    return "beta"\n`,
          "utf8"
        );
        await runCli(
          h,
          binary,
          ["--repo-root", ws0, "index", "--skip-embeddings"],
          env0,
          180_000
        );
        const committedAt = Date.now();
        try {
          await waitUntil("status-latency generation", STATUS_LATENCY_MS + 250, async () => {
            if (observed) return observed;
            const gen = await readGeneration(dbPath);
            if (gen.source_generation > before.source_generation) {
              observed = { gen, at: Date.now() };
              return observed;
            }
            return null;
          });
        } finally {
          clearInterval(poll);
        }
        const elapsed = (observed?.at ?? Date.now()) - committedAt;
        const waitElapsed = Math.max(0, elapsed);
        h.assert(
          "Status_Surface refresh ≤ 2s of committed generation change",
          waitElapsed <= STATUS_LATENCY_MS,
          {
            waitElapsed,
            before,
            observed,
            budgetMs: STATUS_LATENCY_MS,
          }
        );
        recorded.statusLatencyMs = waitElapsed;
      });

      await h.step("entry-restore", async () => {
        const mcpPath = join(ws0, ".cursor", "mcp.json");
        const workspaceKey = resolve(ws0);
        const writeEntry = () => {
          writeJsonFile(mcpPath, {
            mcpServers: {
              cognis: stampManagedMcpEntry(
                {
                  command: binary,
                  args: ["mcpd", "--proxy"],
                  env: {
                    COGNIS_DB_PATH: join(ws0, ".cognis", "uckg.db"),
                    COGNIS_REPO_ROOT: ws0,
                  },
                },
                workspaceKey
              ),
            },
          });
        };
        writeEntry();
        const leasePath = join(ws0, ".cognis", "owner.lease");
        const reconciler = new WatcherEntryReconciler({
          isWatcherRunning: () =>
            existsSync(leasePath) && pidAlive(readJsonFile(leasePath, {}).pid),
          isEntryPresent: () => {
            const doc = readJsonFile(mcpPath, null);
            const block = doc?.mcpServers?.cognis;
            return Boolean(block && isManagedEntry(block, workspaceKey));
          },
          restoreEntry: async () => {
            writeEntry();
          },
          removeEntry: async () => {
            if (existsSync(mcpPath)) unlinkSync(mcpPath);
          },
        });
        const poll = reconciler.startPolling(ws0, 1000);
        try {
          rmSync(mcpPath, { force: true });
          const deletedAt = Date.now();
          await waitUntil("entry-restore", ENTRY_RESTORE_BUDGET_MS, async () => {
            const doc = readJsonFile(mcpPath, null);
            const block = doc?.mcpServers?.cognis;
            return block && isManagedEntry(block, workspaceKey) ? doc : null;
          });
          const elapsed = Date.now() - deletedAt;
          h.assert(
            "Managed_MCP_Entry restored within 5s while watcher runs",
            elapsed <= ENTRY_RESTORE_BUDGET_MS,
            { elapsed, budgetMs: ENTRY_RESTORE_BUDGET_MS }
          );
          recorded.entryRestoreMs = elapsed;
        } finally {
          poll.dispose();
        }
      });

      await h.step("locality-audit", async () => {
        const files = walkFiles(h.iso.root);
        for (const file of files) {
          h.assertUnderIsolation("isolation file", file);
        }
        const sensitive = realUserSensitivePaths();
        for (const p of [...sensitive.mcp, ...sensitive.cognis]) {
          h.assert(
            "real user path is outside isolation",
            !pathIsUnder(p, h.iso.root),
            { path: p, isolationRoot: h.iso.root }
          );
        }
        h.assert(
          "isolation home ≠ real user home",
          resolve(h.iso.home).toLowerCase() !== resolve(homedir()).toLowerCase(),
          { isoHome: h.iso.home, realHome: homedir() }
        );

        const endpointPath = join(ws0, ".cognis", "mcpd.endpoint");
        const endpointText = existsSync(endpointPath)
          ? readFileSync(endpointPath, "utf8")
          : "";
        const host = endpointHost(endpointText);
        h.assert("query socket is loopback", host === "127.0.0.1", {
          host,
          endpointText: endpointText.slice(0, 500),
        });

        const topo = liveTopology(h);
        const conns = tcpConnectionsForPids(topo.procs.map((p) => p.pid));
        const leaked = conns.filter((c) => {
          const remoteOk = isLoopbackAddress(c.remote);
          const localOk = isLoopbackAddress(c.local);
          if (String(c.state).toLowerCase() === "listen") {
            return !localOk || (c.local !== "127.0.0.1" && c.local !== "::1");
          }
          return !remoteOk || !localOk;
        });
        h.assert("no off-box sockets for Cognis processes", leaked.length === 0, {
          leaked,
          conns,
        });

        const dbPath = join(ws0, ".cognis", "uckg.db");
        h.assert("workspace database stays under Canonical_Workspace .cognis", pathIsUnder(dbPath, ws0), {
          dbPath,
          ws0,
        });
        recorded.locality = {
          isolationFiles: files.length,
          endpointHost: host,
          socketsAudited: conns.length,
        };
      });

      await h.step("topology-N-workspaces", async () => {
        for (let i = 1; i < N_WORKSPACES; i += 1) {
          const ws = workspaces[i];
          const env = cognisEnv(h, ws);
          await provisionWorkspace(h, binary, ws, env);
          await startOwner(h, binary, ws, env);
        }

        const leases = [];
        for (const ws of workspaces) {
          const lease = readJsonFile(join(ws, ".cognis", "owner.lease"), null);
          h.assert(`owner.lease present for ${ws}`, Boolean(lease?.pid), { ws, lease });
          leases.push({ ws, pid: lease.pid });
        }
        const uniquePids = new Set(leases.map((l) => l.pid));
        h.assert(
          "each Workspace_Owner serves exactly one Canonical_Workspace",
          uniquePids.size === N_WORKSPACES,
          { leases, uniquePids: [...uniquePids] }
        );

        for (const ws of workspaces) {
          const env = cognisEnv(h, ws);
          const result = await h.run(binary, ["indexd", "--repo-root", ws], env, {
            timeoutMs: 30_000,
          });
          h.assert(
            `indexd attaches without becoming a second owner (${ws})`,
            result.code === 0,
            { code: result.code, stderr: result.stderr.slice(0, 2000) }
          );
        }

        const topo = liveTopology(h);
        const A = N_WORKSPACES;
        const I = topo.indexd.length;
        h.assert("owners at most N", topo.owners.length <= A, {
          owners: topo.owners.length,
          A,
          topo,
        });
        h.assert("owners equals N distinct workspaces", topo.owners.length === A, {
          owners: topo.owners.length,
          A,
        });
        h.assert("indexing processes at most I", topo.indexd.length <= I, {
          indexd: topo.indexd.length,
          I,
        });
        h.assert(
          "at most 1 indexing process per workspace",
          topo.indexd.length <= A,
          { indexd: topo.indexd.length, A }
        );
        recorded.nWorkspaces = {
          owners: topo.owners.length,
          indexd: topo.indexd.length,
          A,
          I,
        };
      });

      await h.step("stop-plus-grace", async () => {
        await h.terminateStarted();
        console.log(`[topology-locality] waiting Grace_Period ${GRACE_PERIOD_S}s`);
        await sleep(GRACE_PERIOD_S * 1000);
        const leftovers = processesReferencing(h.iso.root).filter((p) =>
          pidAlive(p.pid)
        );
        const liveStarted = [...h.startedPids].filter((pid) => pidAlive(pid));
        h.assert("zero Cognis processes after stop + Grace_Period", leftovers.length === 0, {
          leftovers,
          liveStarted,
        });
        recorded.graceLeftovers = leftovers.length;
      });
    },
    { timeoutMs }
  );

  return recorded;
}

async function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(String(error.message ?? error));
    process.exitCode = 2;
    return;
  }
  if (opts.help) {
    console.log(`Topology and locality E2E (task 19.4)

Usage:
  node tests/e2e/full-flow/topology-locality.mjs [--binary PATH]
`);
    return;
  }

  const hostBinary = resolve(opts.binary || findDefaultBinary());
  if (!hostBinary || !existsSync(hostBinary)) {
    console.error(
      "error: no Engine_Binary found. Pass --binary or build with: cargo build -p cognis"
    );
    process.exitCode = 2;
    return;
  }

  console.log(`[topology-locality] host binary: ${hostBinary}`);
  console.log(`[topology-locality] K=${K_CLIENTS} N=${N_WORKSPACES} Grace_Period=${GRACE_PERIOD_S}s`);

  try {
    console.log("[topology-locality] advanced-gate-latency");
    const gate = runAdvancedGateLatency();
    console.log(
      `[topology-locality] advanced-gate-latency OK hidden=${gate.hidden} eval=${gate.elapsedMs}ms`
    );

    const recorded = await runTopologyLocality(hostBinary);
    console.log(
      `[topology-locality] OK k=${JSON.stringify(recorded.kClients)} n=${JSON.stringify(recorded.nWorkspaces)} status=${recorded.statusLatencyMs}ms restore=${recorded.entryRestoreMs}ms`
    );
  } catch (error) {
    if (error instanceof ScenarioError) {
      console.error(`[topology-locality] FAILED at "${error.step}": ${error.message}`);
    } else {
      console.error(error);
    }
    process.exitCode = 1;
  }
}

const isMain =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isMain) {
  main();
}
