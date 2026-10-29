#!/usr/bin/env node
/**
 * Full_Flow_Scenario (cognis-one-toggle-engine task 19.2).
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 23.2–23.7, 1.2,
 * 1.3, 4.1, 4.2, 4.6, 5.6).
 *
 * Drives real Cognis processes over real process boundaries inside an
 * Isolation_Root: Engine_Binary / Model_Assets / Workspace_Database start
 * absent → Toggle On → a Ready state is recorded → MCP query hits the
 * fixture → source edit → re-query reflects the edit with a strictly greater
 * source_generation → Toggle Off → after the 35 s Grace_Period, zero orphans,
 * zero Managed_MCP_Entry, database present with an unchanged digest.
 *
 * Usage (from repo root):
 *   node tests/e2e/full-flow/scenario.mjs
 *   node tests/e2e/full-flow/scenario.mjs --binary target/release/cognis.exe
 */

import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { homedir, platform } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createConnection } from "node:net";

import {
  GRACE_PERIOD_S,
  Harness,
  ScenarioError,
  runWithHarness,
  sleep,
} from "./harness.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "..", "..", "..");

const ALPHA_TOKEN = "full_flow_alpha_token";
const OMEGA_TOKEN = "full_flow_omega_token";
const ALPHA_PAYLOAD = "full_flow_alpha_payload";
const OMEGA_PAYLOAD = "full_flow_omega_payload";

const COGNIS_MANAGED_BY = "COGNIS_MANAGED_BY";
const COGNIS_MANAGED_BY_VALUE = "cognis-mcp";
const COGNIS_MANAGED_WORKSPACE = "COGNIS_MANAGED_WORKSPACE";

const SOURCE_REL = join("src", "full_flow_alpha.py");

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

function binarySupportsSurfaces(path) {
  try {
    execFileSync(path, ["cli", "--help"], {
      timeout: 15_000,
      stdio: "pipe",
      windowsHide: true,
    });
    return true;
  } catch {
    return false;
  }
}

function findDefaultBinary() {
  const names =
    platform() === "win32"
      ? ["cognis.exe", "cognis-mcpd.exe"]
      : ["cognis", "cognis-mcpd"];
  const candidates = [];
  for (const name of names) {
    candidates.push(join(REPO_ROOT, "target", "release", name));
    candidates.push(join(REPO_ROOT, "target", "debug", name));
  }
  for (const c of candidates) {
    if (existsSync(c) && (nameIsMcpd(c) || binarySupportsSurfaces(c))) return c;
  }
  try {
    const which = platform() === "win32" ? "where" : "which";
    const out = execFileSync(which, ["cognis"], { encoding: "utf8" }).trim();
    const first = out.split(/\r?\n/).filter(Boolean)[0];
    if (first && existsSync(first) && binarySupportsSurfaces(first)) return first;
  } catch {
    // ignore
  }
  return "";
}

function nameIsMcpd(p) {
  return /mcpd/i.test(p);
}

export function engineName() {
  return platform() === "win32" ? "cognis.exe" : "cognis";
}

function fixtureSource(payloadToken, extraDef) {
  return `# Synthetic fixture for the Full_Flow_Scenario. Do not use in production.
def ${ALPHA_TOKEN}():
    """Indexed symbol whose body is the query target."""
    return "${payloadToken}"
${extraDef}
`;
}

function extraOmegaDef() {
  return `
def ${OMEGA_TOKEN}():
    """Appears only after the source edit."""
    return "${OMEGA_PAYLOAD}"
`;
}

export function parseJsonStdout(stdout, label) {
  const text = String(stdout ?? "").trim();
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  if (start < 0 || end < start) {
    throw new ScenarioError(`parse ${label}`, { stdout: text.slice(0, 2000) });
  }
  try {
    return JSON.parse(text.slice(start, end + 1));
  } catch (error) {
    throw new ScenarioError(`parse ${label}`, {
      message: error instanceof Error ? error.message : String(error),
      stdout: text.slice(0, 2000),
    });
  }
}

function checkStatus(health, name) {
  return health?.checks?.[name]?.status;
}

/**
 * Status_Surface derivation used by the scenario (mirrors the extension
 * decision: lexical serving from health checks; Ready_Full only when the
 * designed conjunction holds). Skip-embeddings installs land on Ready_Lexical.
 */
function deriveReadyState(health, ownerStatus) {
  const lexical =
    checkStatus(health, "config") === "ok" &&
    checkStatus(health, "db") === "ok" &&
    checkStatus(health, "index") === "ok";
  if (!lexical) {
    return null;
  }
  const caps = health?.capabilities ?? ownerStatus?.capabilities ?? {};
  const semanticAvailable = health?.semantic_available === true;
  const pending =
    health?.pending_vector_count ?? ownerStatus?.pendingVectorCount ?? 0;
  const source =
    health?.source_generation ?? ownerStatus?.sourceGeneration;
  const semantic =
    health?.semantic_generation ?? ownerStatus?.semanticGeneration;
  const fingerprint =
    health?.fingerprint_match ?? ownerStatus?.fingerprintMatch;
  if (
    semanticAvailable &&
    pending === 0 &&
    source !== undefined &&
    semantic !== undefined &&
    semantic === source &&
    fingerprint === true &&
    caps.semantic_backend === true &&
    caps.accelerated_vector_search === true
  ) {
    return "Ready_Full";
  }
  return "Ready_Lexical";
}

function isCognisMcpServerName(name) {
  return name === "cognis" || String(name).startsWith("cognis-");
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

function isManagedEntry(name, block, workspaceKey) {
  const env = block?.env ?? {};
  if (
    env[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE &&
    env[COGNIS_MANAGED_WORKSPACE] === workspaceKey
  ) {
    return true;
  }
  if (env[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE) {
    return false;
  }
  return isCognisMcpServerName(name);
}

function filterOutManagedMcpEntries(servers, workspaceKey) {
  const removed = [];
  for (const name of Object.keys(servers ?? {})) {
    if (isManagedEntry(name, servers[name], workspaceKey)) {
      delete servers[name];
      removed.push(name);
    }
  }
  return removed;
}

function countManagedEntries(doc, workspaceKey) {
  const servers = doc?.mcpServers ?? {};
  return Object.entries(servers).filter(([name, block]) =>
    isManagedEntry(name, block, workspaceKey)
  ).length;
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

function writeJsonFile(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function listModelAssetPaths(root) {
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
        if (ent.name === "models" || ent.name.toLowerCase().includes("onnx")) {
          found.push(full);
        }
        stack.push(full);
      } else if (/\.(onnx|ort)$/i.test(ent.name)) {
        found.push(full);
      }
    }
  }
  return found;
}

export function pidAlive(pid) {
  if (!pid || pid === process.pid) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error && error.code === "EPERM";
  }
}

export function processesReferencing(root) {
  const needle = resolve(root).toLowerCase().replace(/\\/g, "/");
  /** @type {Array<{ pid: number, name: string, commandLine: string }>} */
  const hits = [];
  try {
    if (platform() === "win32") {
      const script = [
        "$ErrorActionPreference = 'SilentlyContinue'",
        "Get-CimInstance Win32_Process | ForEach-Object {",
        "  if ($_.CommandLine) {",
        "    '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.CommandLine",
        "  }",
        "}",
      ].join("; ");
      const out = execFileSync("powershell.exe", ["-NoProfile", "-Command", script], {
        encoding: "utf8",
        windowsHide: true,
        timeout: 30_000,
      });
      for (const line of out.split(/\r?\n/)) {
        if (!line.trim()) continue;
        const [pidStr, name, ...rest] = line.split("|");
        const commandLine = rest.join("|");
        const hay = `${commandLine}`.toLowerCase().replace(/\\/g, "/");
        if (hay.includes(needle)) {
          hits.push({
            pid: Number.parseInt(pidStr, 10),
            name: name ?? "",
            commandLine,
          });
        }
      }
    } else {
      const out = execFileSync("ps", ["-eo", "pid,args"], { encoding: "utf8" });
      for (const line of out.split(/\n/).slice(1)) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        const sp = trimmed.indexOf(" ");
        const pid = Number.parseInt(trimmed.slice(0, sp), 10);
        const commandLine = trimmed.slice(sp + 1);
        if (commandLine.toLowerCase().includes(needle)) {
          hits.push({ pid, name: "", commandLine });
        }
      }
    }
  } catch {
    // Best-effort: PID liveness of started children is the hard assert.
  }
  return hits.filter((p) => Number.isInteger(p.pid) && p.pid !== process.pid);
}

export function endpointPort(text) {
  const line = String(text)
    .split(/\r?\n/)
    .map((s) => s.trim())
    .find((s) => s.startsWith("http://") || s.startsWith("https://"));
  if (!line) return null;
  const rest = line.replace(/^https?:\/\//, "");
  const hostPort = rest.split("/")[0];
  const port = Number.parseInt(hostPort.split(":")[1], 10);
  return Number.isInteger(port) ? port : null;
}

export function portAccepts(port, timeoutMs = 400) {
  return new Promise((resolveAccept) => {
    if (!port) {
      resolveAccept(false);
      return;
    }
    const sock = createConnection({ host: "127.0.0.1", port });
    const timer = setTimeout(() => {
      sock.destroy();
      resolveAccept(false);
    }, timeoutMs);
    sock.once("connect", () => {
      clearTimeout(timer);
      sock.end();
      resolveAccept(true);
    });
    sock.once("error", () => {
      clearTimeout(timer);
      resolveAccept(false);
    });
  });
}

export async function waitUntil(label, timeoutMs, probe) {
  const start = Date.now();
  let last;
  while (Date.now() - start < timeoutMs) {
    last = await probe();
    if (last) return last;
    await sleep(100);
  }
  throw new ScenarioError(label, {
    timeoutMs,
    last: last && typeof last === "object" ? last : { value: last },
  });
}

export function teeStderr(child, logPath) {
  mkdirSync(dirname(logPath), { recursive: true });
  const stream = createWriteStream(logPath);
  child.stderr?.setEncoding("utf8");
  child.stderr?.pipe(stream);
  return stream;
}

function hitBlob(hit) {
  if (hit == null) return "";
  if (typeof hit === "string") return hit;
  return [
    hit.name,
    hit.qualified_name,
    hit.snippet,
    hit.body_excerpt,
    hit.file_path,
    hit.match_reason,
  ]
    .filter((v) => v != null)
    .join(" ");
}

function extractHits(structured) {
  if (!structured || typeof structured !== "object") return [];
  if (Array.isArray(structured.result)) return structured.result;
  if (Array.isArray(structured)) return structured;
  return [];
}

export class McpClient {
  /**
   * @param {import("node:child_process").ChildProcess} child
   */
  constructor(child) {
    this.child = child;
    this.buf = "";
    this.queue = [];
    this.waiters = [];
    this.nextId = 0;
    child.stdout?.setEncoding("utf8");
    child.stdout?.on("data", (chunk) => {
      this.buf += chunk;
      let nl;
      while ((nl = this.buf.indexOf("\n")) >= 0) {
        const line = this.buf.slice(0, nl).trim();
        this.buf = this.buf.slice(nl + 1);
        if (!line) continue;
        let parsed;
        try {
          parsed = JSON.parse(line);
        } catch {
          continue;
        }
        const waiter = this.waiters.shift();
        if (waiter) waiter.resolve(parsed);
        else this.queue.push(parsed);
      }
    });
    child.once("exit", (code, signal) => {
      while (this.waiters.length) {
        this.waiters.shift().reject(
          new Error(`mcpd exited (code=${code} signal=${signal}) before response`)
        );
      }
    });
  }

  read(timeoutMs = 30_000) {
    if (this.queue.length) {
      return Promise.resolve(this.queue.shift());
    }
    return new Promise((resolveRead, reject) => {
      const timer = setTimeout(() => {
        const idx = this.waiters.indexOf(entry);
        if (idx >= 0) this.waiters.splice(idx, 1);
        reject(new Error(`MCP response timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      const entry = {
        resolve: (value) => {
          clearTimeout(timer);
          resolveRead(value);
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      };
      this.waiters.push(entry);
    });
  }

  send(obj) {
    this.child.stdin.write(`${JSON.stringify(obj)}\n`);
  }

  async request(method, params, timeoutMs = 30_000) {
    this.nextId += 1;
    const id = this.nextId;
    this.send({ jsonrpc: "2.0", id, method, params: params ?? {} });
    const resp = await this.read(timeoutMs);
    if (resp.id !== id) {
      throw new ScenarioError("mcp response id", { want: id, got: resp.id, resp });
    }
    return resp;
  }

  notify(method) {
    this.send({ jsonrpc: "2.0", method });
  }

  async initialize() {
    const resp = await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "full-flow", version: "0" },
    });
    this.notify("notifications/initialized");
    return resp;
  }

  async callTool(name, args) {
    const resp = await this.request("tools/call", { name, arguments: args });
    if (resp.error) {
      throw new ScenarioError(`mcp tools/call ${name}`, resp.error);
    }
    const structured =
      resp.result?.structuredContent ??
      (resp.result?.content?.[0]?.text
        ? JSON.parse(resp.result.content[0].text)
        : resp.result);
    return { resp, structured, hits: extractHits(structured) };
  }
}

export async function runCli(h, binary, args, extraEnv, timeoutMs = 120_000) {
  const result = await h.run(binary, args, extraEnv, { timeoutMs });
  if (result.code !== 0) {
    throw new ScenarioError(`cli ${args.join(" ")}`, {
      code: result.code,
      stdout: result.stdout.slice(0, 4000),
      stderr: result.stderr.slice(0, 4000),
    });
  }
  return result;
}

async function runFullFlow(hostBinary) {
  const timeoutMs = (GRACE_PERIOD_S + 180) * 1000;
  /** @type {{ readyState?: string, digest?: object, generation0?: number, generation1?: number }} */
  const recorded = {};

  await runWithHarness(
    async (h) => {
      const ws = h.workspacePath("fixture");
      const engineDest = join(h.iso.bin, engineName());
      const dbPath = join(ws, ".cognis", "uckg.db");
      const userMcp = join(h.iso.home, ".cursor", "mcp.json");
      const workspaceMcp = join(ws, ".cursor", "mcp.json");
      const workspaceKey = resolve(ws);
      const cognisEnv = {
        COGNIS_REPO_ROOT: ws,
        COGNIS_DB_PATH: dbPath,
        COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
        COGNIS_AUDIT_LOG: join(h.iso.logs, "audit.log"),
      };

      await h.step("assert-absent", async () => {
        h.assert("Engine_Binary absent", !existsSync(engineDest), { engineDest });
        h.assert("Workspace_Database absent", !existsSync(dbPath), { dbPath });
        const models = listModelAssetPaths(h.iso.root);
        h.assert("Model_Assets absent", models.length === 0, { models });
        h.assertUnderIsolation("engine dest", engineDest);
        h.assertUnderIsolation("workspace", ws);
      });

      await h.step("toggle-on", async () => {
        mkdirSync(join(ws, "src"), { recursive: true });
        writeFileSync(
          join(ws, SOURCE_REL),
          fixtureSource(ALPHA_PAYLOAD, ""),
          "utf8"
        );
        writeFileSync(
          join(h.iso.logs, "toggle-state.json"),
          `${JSON.stringify({ state: "On" })}\n`,
          "utf8"
        );

        copyFileSync(hostBinary, engineDest);
        h.assert("Engine_Binary provisioned under isolation", existsSync(engineDest), {
          engineDest,
        });
        h.assertUnderIsolation("provisioned binary", engineDest);

        await runCli(h, engineDest, ["--repo-root", ws, "init"], cognisEnv);
        h.assert(
          "database still absent after init (created by index)",
          !existsSync(dbPath),
          { dbPath }
        );

        const indexed = await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "index", "--full", "--skip-embeddings"],
          cognisEnv,
          180_000
        );
        h.assert("Workspace_Database created", existsSync(dbPath), {
          dbPath,
          indexOut: indexed.stdout.slice(0, 1000),
        });

        const healthOut = await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "health", "--json"],
          cognisEnv
        );
        const health = parseJsonStdout(healthOut.stdout, "health");
        writeJsonFile(join(h.iso.logs, "health.json"), health);

        const mcpOut = await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "mcp-config", "--host", "cursor", "--server-name", "cognis"],
          cognisEnv
        );
        const mcpPayload = parseJsonStdout(mcpOut.stdout, "mcp-config");
        const emitted =
          mcpPayload.config?.mcpServers?.cognis ??
          Object.values(mcpPayload.config?.mcpServers ?? {})[0];
        h.assert("mcp-config emitted a server block", Boolean(emitted), mcpPayload);
        const managed = {
          mcpServers: {
            cognis: stampManagedMcpEntry(
              {
                command: engineDest,
                args: ["mcpd"],
                env: {
                  ...(emitted.env ?? {}),
                  COGNIS_DB_PATH: dbPath,
                  COGNIS_REPO_ROOT: ws,
                  COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
                },
              },
              workspaceKey
            ),
          },
        };
        writeJsonFile(userMcp, managed);
        writeJsonFile(workspaceMcp, managed);
        h.assertUnderIsolation("user mcp.json", userMcp);
        h.assert(
          "Managed_MCP_Entry written",
          countManagedEntries(managed, workspaceKey) >= 1,
          managed
        );

        const owner = h.spawn(
          engineDest,
          ["owner", "--repo-root", ws, "--host", "127.0.0.1", "--port", "0"],
          cognisEnv,
          { cwd: ws }
        );
        owner.stdout?.pipe(createWriteStream(join(h.iso.logs, "owner.stdout.log")));
        teeStderr(owner, join(h.iso.logs, "owner.stderr.log"));
        const endpointPath = join(ws, ".cognis", "mcpd.endpoint");
        const ownerLog = join(h.iso.logs, "owner.stderr.log");
        const endpointText = await waitUntil("owner endpoint", 60_000, async () => {
          if (owner.exitCode !== null) {
            throw new ScenarioError("owner endpoint", {
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
        writeFileSync(join(h.iso.logs, "mcpd.endpoint"), endpointText, "utf8");

        const ownerLease = readJsonFile(join(ws, ".cognis", "owner.lease"), null);
        if (ownerLease?.pid) {
          h.startedPids.add(ownerLease.pid);
        }
        const ownerStatusPath = join(ws, ".cognis", "owner-status.json");
        const ownerStatus = readJsonFile(ownerStatusPath, null);
        const readyState = deriveReadyState(health, ownerStatus);
        h.assert(
          "Ready state reached",
          readyState === "Ready_Full" || readyState === "Ready_Lexical",
          { readyState, health, ownerStatus }
        );
        recorded.readyState = readyState;
        writeJsonFile(join(h.iso.logs, "ready-state.json"), {
          state: readyState,
          health,
          ownerStatus,
        });
        console.log(`[full-flow] Ready state: ${readyState}`);
      });

      /** @type {McpClient | null} */
      let mcp = null;
      await h.step("mcp-query", async () => {
        const mcpd = h.spawn(engineDest, ["mcpd"], cognisEnv, {
          cwd: ws,
          stdin: "pipe",
        });
        teeStderr(mcpd, join(h.iso.logs, "mcpd.stderr.log"));
        await sleep(250);
        for (const p of processesReferencing(h.iso.root)) {
          h.startedPids.add(p.pid);
        }
        mcp = new McpClient(mcpd);
        const init = await mcp.initialize();
        h.assert(
          "MCP initialize ok",
          Boolean(init.result) && !init.error,
          init
        );
        const { structured, hits } = await mcp.callTool("symbol_search", {
          query: ALPHA_TOKEN,
          k: 8,
        });
        const matching = hits.filter((hit) =>
          hitBlob(hit).toLowerCase().includes("full_flow_alpha")
        );
        h.assert("MCP query returns ≥1 fixture-derived result", matching.length >= 1, {
          structured,
          hits,
        });
        const gen = structured?.source_generation;
        h.assert("source_generation present", typeof gen === "number", structured);
        recorded.generation0 = gen;
        writeJsonFile(join(h.iso.logs, "mcp-query-1.json"), structured);
      });

      await h.step("edit-and-requery", async () => {
        writeFileSync(
          join(ws, SOURCE_REL),
          fixtureSource(OMEGA_PAYLOAD, extraOmegaDef()),
          "utf8"
        );
        await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "index", "--skip-embeddings"],
          cognisEnv,
          180_000
        );
        h.assert("mcpd still running", Boolean(mcp && mcp.child.exitCode === null), {
          exitCode: mcp?.child.exitCode,
        });
        const { structured, hits } = await mcp.callTool("symbol_search", {
          query: OMEGA_TOKEN,
          k: 8,
        });
        const blob = hits.map(hitBlob).join("\n").toLowerCase();
        h.assert(
          "re-query reflects edited content",
          blob.includes("omega") || blob.includes(OMEGA_TOKEN.toLowerCase()),
          { structured, hits }
        );
        const gen = structured?.source_generation;
        h.assert(
          "source_generation strictly greater",
          typeof gen === "number" && gen > recorded.generation0,
          { before: recorded.generation0, after: gen, structured }
        );
        recorded.generation1 = gen;
        writeJsonFile(join(h.iso.logs, "mcp-query-2.json"), structured);
      });

      await h.step("toggle-off", async () => {
        const digestOut = await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "digest"],
          cognisEnv
        );
        const digest = parseJsonStdout(digestOut.stdout, "digest");
        h.assert("digest has combined", typeof digest.combined === "string", digest);
        recorded.digest = digest;
        writeJsonFile(join(h.iso.logs, "digest-before-off.json"), digest);

        if (mcp?.child.stdin) {
          try {
            mcp.child.stdin.end();
          } catch {
            // ignore
          }
        }
        await sleep(300);
        await h.terminateStarted();

        for (const mcpPath of [userMcp, workspaceMcp]) {
          const doc = readJsonFile(mcpPath, { mcpServers: {} });
          filterOutManagedMcpEntries(doc.mcpServers ?? {}, workspaceKey);
          writeJsonFile(mcpPath, doc);
        }
        writeFileSync(
          join(h.iso.logs, "toggle-state.json"),
          `${JSON.stringify({ state: "Off" })}\n`,
          "utf8"
        );

        console.log(`[full-flow] waiting Grace_Period ${GRACE_PERIOD_S}s`);
        await sleep(GRACE_PERIOD_S * 1000);

        const liveStarted = [...h.startedPids].filter((pid) => pidAlive(pid));
        const leftovers = processesReferencing(h.iso.root).filter((p) =>
          pidAlive(p.pid)
        );
        h.assert("zero Orphan_Process after Grace_Period", liveStarted.length === 0, {
          liveStarted,
          leftovers,
        });
        h.assert(
          "no isolation-scoped leftover processes",
          leftovers.length === 0,
          leftovers
        );

        const userDoc = readJsonFile(userMcp, { mcpServers: {} });
        const wsDoc = readJsonFile(workspaceMcp, { mcpServers: {} });
        h.assert(
          "zero Managed_MCP_Entry (user mcp.json)",
          countManagedEntries(userDoc, workspaceKey) === 0,
          userDoc
        );
        h.assert(
          "zero Managed_MCP_Entry (workspace mcp.json)",
          countManagedEntries(wsDoc, workspaceKey) === 0,
          wsDoc
        );

        h.assert("Workspace_Database present after Off", existsSync(dbPath), { dbPath });
        const digestAfterOut = await runCli(
          h,
          engineDest,
          ["--repo-root", ws, "digest"],
          cognisEnv
        );
        const digestAfter = parseJsonStdout(digestAfterOut.stdout, "digest-after");
        h.assert("Database_Digest unchanged", digestAfter.combined === digest.combined, {
          before: digest,
          after: digestAfter,
        });
        writeJsonFile(join(h.iso.logs, "digest-after-off.json"), digestAfter);
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
    console.log(`Full_Flow_Scenario (task 19.2)

Usage:
  node tests/e2e/full-flow/scenario.mjs [--binary PATH]

Options:
  --binary PATH   cognis multi-call binary (default: target/release/cognis)
`);
    return;
  }

  const hostBinary = resolve(opts.binary || findDefaultBinary());
  if (!hostBinary || !existsSync(hostBinary)) {
    console.error(
      "error: no Engine_Binary found. Pass --binary or build with: cargo build -p cognis --release"
    );
    process.exitCode = 2;
    return;
  }

  console.log(`[full-flow] host binary: ${hostBinary}`);
  console.log(`[full-flow] Grace_Period: ${GRACE_PERIOD_S}s`);
  try {
    const recorded = await runFullFlow(hostBinary);
    console.log(
      `[full-flow] OK ready=${recorded.readyState} gen ${recorded.generation0}→${recorded.generation1}`
    );
  } catch (error) {
    if (error instanceof ScenarioError) {
      console.error(`[full-flow] FAILED at "${error.step}": ${error.message}`);
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

export { deriveReadyState, findDefaultBinary };
