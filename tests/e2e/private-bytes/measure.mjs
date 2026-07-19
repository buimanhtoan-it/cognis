#!/usr/bin/env node
/**
 * Private-byte / process-cardinality measurement (task 9.2).
 *
 * Platform-correct private-byte sampling over the Cognis process tree, with
 * isolated temp repos/config homes that never touch the real `.cognis` or host
 * MCP config (Requirements 2.11, 2.14; preservation 3.10, 3.11).
 *
 * Usage (from repo root):
 *   node tests/e2e/private-bytes/measure.mjs --runs 5 --binary target/release/cognis.exe
 *
 * See README.md in this directory for the full procedure.
 */

import { spawn, execFile, execFileSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
  existsSync,
} from "node:fs";
import { tmpdir, hostname, platform, arch, release } from "node:os";
import { join, resolve, dirname, basename, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { randomBytes } from "node:crypto";

const execFileAsync = promisify(execFile);
const __dirname = dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// Constants (Requirement 2.11)
// ---------------------------------------------------------------------------

const BASELINE_PRIVATE_BYTES_GIB = 1.23;
const TARGET_MEDIAN_PRIVATE_BYTES_GIB = 0.615;
const MIN_RUNS = 5;
/** Slightly above crates/cognis-core DEFAULT_LEASE_TTL (30s). */
const DEFAULT_GRACE_PERIOD_S = 35;
const DEFAULT_SETTLE_S = 8;
const DEFAULT_REPOS = 3;
const GIB = 1024 ** 3;
const SCHEMA_VERSION = 1;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const out = {
    runs: MIN_RUNS,
    binary: "",
    topology: "baseline-idle",
    repos: DEFAULT_REPOS,
    settleS: DEFAULT_SETTLE_S,
    graceS: DEFAULT_GRACE_PERIOD_S,
    out: "",
    filterPath: "",
    activeLoad: false,
    fanOutHosts: 1,
    warmSemantic: "0",
    keepIsolation: false,
    dryRun: false,
    selfTest: false,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const next = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`missing value for ${a}`);
      return v;
    };
    switch (a) {
      case "--runs":
        out.runs = Math.max(1, Number.parseInt(next(), 10) || MIN_RUNS);
        break;
      case "--binary":
        out.binary = next();
        break;
      case "--topology":
        out.topology = next();
        break;
      case "--repos":
        out.repos = Math.max(1, Number.parseInt(next(), 10) || DEFAULT_REPOS);
        break;
      case "--settle-s":
        out.settleS = Math.max(0, Number.parseFloat(next()) || 0);
        break;
      case "--grace-s":
        out.graceS = Math.max(0, Number.parseFloat(next()) || 0);
        break;
      case "--out":
        out.out = next();
        break;
      case "--filter-path":
        out.filterPath = next();
        break;
      case "--active-load":
        out.activeLoad = true;
        break;
      case "--fan-out-hosts":
        out.fanOutHosts = Math.max(1, Number.parseInt(next(), 10) || 1);
        break;
      case "--warm-semantic":
        out.warmSemantic = next();
        break;
      case "--keep-isolation":
        out.keepIsolation = true;
        break;
      case "--dry-run":
        out.dryRun = true;
        break;
      case "--self-test":
        out.selfTest = true;
        break;
      case "-h":
      case "--help":
        out.help = true;
        break;
      default:
        if (a.startsWith("-")) {
          throw new Error(`unknown flag: ${a}`);
        }
    }
  }
  return out;
}

function printHelp() {
  console.log(`measure.mjs — private-byte / process-cardinality harness (task 9.2)

Usage:
  node tests/e2e/private-bytes/measure.mjs [options]

Options:
  --runs N              clean runs for median (default ${MIN_RUNS}, gate needs ≥${MIN_RUNS})
  --binary PATH         cognis multi-call binary (required unless --topology sample-only)
  --topology NAME       baseline-idle | fixed-idle | sample-only (default baseline-idle)
  --repos N             synthetic repos under isolation root (default ${DEFAULT_REPOS})
  --settle-s S          idle settle window before sampling (default ${DEFAULT_SETTLE_S})
  --grace-s S           post-stop orphan grace period (default ${DEFAULT_GRACE_PERIOD_S})
  --fan-out-hosts N     spawn N heavy mcpd per repo (defect fan-out; default 1)
  --warm-semantic 0|1   COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP (default 0)
  --active-load         also drive a short active peak sample (reported separately)
  --filter-path PATH    sample-only: only count processes referencing this path
  --out PATH            write JSON report (default: isolation root / report.json)
  --keep-isolation      do not delete the isolation root after the run
  --dry-run             print plan only; do not spawn or sample
  --self-test           run pure helper assertions (median, classify, isolation)
  -h, --help            this help

Isolation: every run uses a temp home under os.tmpdir() and never touches the
real developer .cognis or host MCP config (preservation 3.10).

Evidence: results are empirical for named hardware/build/topology (3.11).
Target median ≤ ${TARGET_MEDIAN_PRIVATE_BYTES_GIB} GiB vs baseline ceiling ${BASELINE_PRIVATE_BYTES_GIB} GiB
is a gate, not an assumed achievement.
`);
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function median(nums) {
  if (nums.length === 0) return null;
  const s = [...nums].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 === 0 ? (s[mid - 1] + s[mid]) / 2 : s[mid];
}

function minOf(nums) {
  return nums.length ? Math.min(...nums) : null;
}

function maxOf(nums) {
  return nums.length ? Math.max(...nums) : null;
}

function bytesToGib(bytes) {
  return bytes / GIB;
}

function round3(n) {
  return Math.round(n * 1000) / 1000;
}

function round6(n) {
  return Math.round(n * 1e6) / 1e6;
}

function nowIso() {
  return new Date().toISOString();
}

function runId() {
  return randomBytes(4).toString("hex");
}

function normalizePathKey(p) {
  if (!p) return "";
  try {
    return resolve(p).toLowerCase().replace(/\\/g, "/");
  } catch {
    return String(p).toLowerCase().replace(/\\/g, "/");
  }
}

function pathIsUnder(child, root) {
  const c = normalizePathKey(child);
  const r = normalizePathKey(root);
  if (!c || !r) return false;
  return c === r || c.startsWith(r.endsWith("/") ? r : `${r}/`);
}

function findDefaultBinary() {
  const root = resolve(__dirname, "..", "..", "..");
  const names =
    platform() === "win32"
      ? ["cognis.exe", "cognis-mcpd.exe"]
      : ["cognis", "cognis-mcpd"];
  const candidates = [];
  for (const name of names) {
    candidates.push(join(root, "target", "release", name));
    candidates.push(join(root, "target", "debug", name));
  }
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  // PATH fallback
  try {
    const which = platform() === "win32" ? "where" : "which";
    const out = execFileSync(which, ["cognis"], { encoding: "utf8" }).trim();
    const first = out.split(/\r?\n/).filter(Boolean)[0];
    if (first && existsSync(first)) return first;
  } catch {
    // ignore
  }
  return "";
}

// ---------------------------------------------------------------------------
// Isolation root (preservation 3.10)
// ---------------------------------------------------------------------------

/**
 * Create a throwaway isolation root. All repos, config homes, and Cognis state
 * live under this directory. Never points at the real user home or real host
 * MCP config paths.
 */
function createIsolationRoot() {
  const root = mkdtempSync(join(tmpdir(), "cognis-pb-measure-"));
  const home = join(root, "home");
  const appData = join(home, "AppData", "Roaming");
  const localAppData = join(home, "AppData", "Local");
  const xdgConfig = join(home, ".config");
  const repos = join(root, "repos");
  const outDir = join(root, "out");
  for (const d of [home, appData, localAppData, xdgConfig, repos, outDir]) {
    mkdirSync(d, { recursive: true });
  }
  // Stub host MCP config locations under the fake home so any accidental
  // relative resolution still cannot touch the real host files.
  for (const rel of [
    join(".cursor", "mcp.json"),
    join(".vscode", "mcp.json"),
    join("AppData", "Roaming", "Cursor", "User", "globalStorage", "mcp.json"),
  ]) {
    const p = join(home, rel);
    mkdirSync(dirname(p), { recursive: true });
    if (!existsSync(p)) {
      writeFileSync(p, JSON.stringify({ mcpServers: {} }, null, 2), "utf8");
    }
  }
  return {
    root,
    home,
    appData,
    localAppData,
    xdgConfig,
    repos,
    outDir,
  };
}

function isolationEnv(iso) {
  // Strip host MCP / Cognis paths that could leak into child processes.
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
    // Never inherit a real developer's model path unless the isolation root
    // itself contains one; callers can still set COGNIS_ONNX_MODEL_DIR inside
    // the isolation-aware spawn helper.
  };
}

function materializeRepo(iso, index) {
  const name = `repo-${String(index + 1).padStart(2, "0")}`;
  const repoRoot = join(iso.repos, name);
  const cognisDir = join(repoRoot, ".cognis");
  mkdirSync(join(repoRoot, "src"), { recursive: true });
  mkdirSync(cognisDir, { recursive: true });
  writeFileSync(
    join(repoRoot, "src", "main.py"),
    `# synthetic fixture for private-byte measurement\n` +
      `def greet(name: str) -> str:\n    return f"hello {name}"\n` +
      `def verify(token: str) -> bool:\n    return bool(token)\n`,
    "utf8"
  );
  writeFileSync(
    join(repoRoot, "README.md"),
    `# ${name}\n\nDisposable isolation fixture. Not a real workspace.\n`,
    "utf8"
  );
  // Minimal config so daemons that look for .cognis/config.yaml do not fall
  // back to a host path.
  writeFileSync(
    join(cognisDir, "config.yaml"),
    `repo_root: ${JSON.stringify(repoRoot)}\n`,
    "utf8"
  );
  const dbPath = join(cognisDir, "uckg.db");
  const auditLog = join(cognisDir, "audit.log");
  const statusPath = join(cognisDir, "indexd-status.json");
  return {
    name,
    repoRoot,
    cognisDir,
    dbPath,
    auditLog,
    statusPath,
    canonicalKey: normalizePathKey(repoRoot),
  };
}

// ---------------------------------------------------------------------------
// Process classification (aligned with mcpRuntime.ts / mcpServer.ts)
// ---------------------------------------------------------------------------

const THIN_PROXY_ENV = "COGNIS_MCP_PROXY";

/**
 * @typedef {"heavy_mcpd" | "thin_proxy" | "indexd" | "other_cognis" | "unknown"} ProcessClass
 */

/**
 * @param {{ pid: number, commandLine?: string, name?: string }} proc
 * @returns {ProcessClass | null} null when not a cognis daemon of interest
 */
function classifyProcess(proc) {
  const cmd = `${proc.name ?? ""} ${proc.commandLine ?? ""}`.toLowerCase();
  if (!cmd.includes("cognis") && !cmd.includes("mcpd") && !cmd.includes("indexd")) {
    return null;
  }
  // Ignore this measurement script / node harness itself.
  if (cmd.includes("measure.mjs") || cmd.includes("private-bytes")) {
    return null;
  }

  const isThin =
    /(?:^|[\s"])--proxy(?:[\s"=]|$)/.test(cmd) ||
    /--transport(?:=|\s+)proxy\b/i.test(cmd) ||
    new RegExp(`${THIN_PROXY_ENV}=1`).test(cmd);

  const isMcpd =
    /\bmcpd\b/.test(cmd) ||
    cmd.includes("cognis-mcpd") ||
    cmd.includes("cognis_mcpd") ||
    /\bserve\b/.test(cmd) && cmd.includes("cognis");

  const isIndexd =
    /\bindexd\b/.test(cmd) ||
    cmd.includes("cognis-indexd") ||
    (/\b(daemon|watch)\b/.test(cmd) && cmd.includes("cognis"));

  if (isThin && (isMcpd || cmd.includes("cognis"))) {
    return "thin_proxy";
  }
  if (isIndexd) {
    return "indexd";
  }
  if (isMcpd) {
    return "heavy_mcpd";
  }
  if (cmd.includes("cognis")) {
    return "other_cognis";
  }
  return null;
}

/**
 * Heavy for private-byte aggregation: heavy mcpd or indexd (both may map ONNX
 * / hold a DB). The cardinality gate `heavy ≤ A` uses heavy mcpd only.
 */
function isHeavyClass(c) {
  return c === "heavy_mcpd" || c === "indexd";
}

function processReferencesPath(proc, filterRoot) {
  if (!filterRoot) return true;
  const cmd = proc.commandLine ?? "";
  const key = normalizePathKey(filterRoot);
  // Command lines often use native separators; compare both forms.
  const native = filterRoot.replace(/\//g, sep);
  const alt = filterRoot.replace(/\\/g, "/");
  if (cmd.includes(filterRoot) || cmd.includes(native) || cmd.includes(alt)) {
    return true;
  }
  // Loose: basename of isolation root (unique mkdtemp prefix).
  const base = basename(filterRoot);
  if (base && cmd.includes(base)) return true;
  // Normalized lower-case search for Windows case variance.
  return cmd.toLowerCase().includes(key) || cmd.toLowerCase().includes(base.toLowerCase());
}

// ---------------------------------------------------------------------------
// Platform process enumeration + private bytes
// ---------------------------------------------------------------------------

/**
 * List candidate Cognis-related processes with command lines.
 * @returns {Promise<Array<{ pid: number, ppid: number, name: string, commandLine: string }>>}
 */
async function listCandidateProcesses() {
  if (platform() === "win32") {
    return listCandidateProcessesWindows();
  }
  return listCandidateProcessesPosix();
}

async function listCandidateProcessesWindows() {
  // Prefer CIM for command line + parent; filter broadly then classify in JS.
  // Use semicolons (not bare newlines) so `-Command` remains one valid script.
  const script = [
    "$ErrorActionPreference = 'SilentlyContinue'",
    "Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'cognis' -or ($_.CommandLine -and ($_.CommandLine -match 'cognis|mcpd|indexd')) } | Select-Object ProcessId, ParentProcessId, Name, CommandLine | ConvertTo-Json -Compress -Depth 3",
  ].join("; ");
  try {
    const { stdout } = await execFileAsync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-Command", script],
      { timeout: 20000, windowsHide: true, maxBuffer: 8 * 1024 * 1024 }
    );
    const trimmed = stdout.trim();
    if (!trimmed) return [];
    const parsed = JSON.parse(trimmed);
    const rows = Array.isArray(parsed) ? parsed : [parsed];
    return rows
      .map((r) => ({
        pid: Number(r.ProcessId),
        ppid: Number(r.ParentProcessId) || 0,
        name: String(r.Name ?? ""),
        commandLine: String(r.CommandLine ?? ""),
      }))
      .filter((r) => Number.isInteger(r.pid) && r.pid > 0);
  } catch (err) {
    console.warn(`[measure] Windows process list failed: ${err.message ?? err}`);
    return [];
  }
}

async function listCandidateProcessesPosix() {
  try {
    const { stdout } = await execFileAsync("ps", ["-eo", "pid=,ppid=,comm=,args="], {
      timeout: 15000,
      maxBuffer: 8 * 1024 * 1024,
    });
    const out = [];
    for (const line of stdout.split(/\r?\n/)) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      // pid ppid comm args... — comm has no spaces usually; args may.
      const m = trimmed.match(/^(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/);
      if (!m) continue;
      const pid = Number.parseInt(m[1], 10);
      const ppid = Number.parseInt(m[2], 10);
      const name = m[3];
      const commandLine = m[4];
      const blob = `${name} ${commandLine}`.toLowerCase();
      if (
        !blob.includes("cognis") &&
        !blob.includes("mcpd") &&
        !blob.includes("indexd")
      ) {
        continue;
      }
      out.push({ pid, ppid, name, commandLine });
    }
    return out;
  } catch (err) {
    console.warn(`[measure] POSIX process list failed: ${err.message ?? err}`);
    return [];
  }
}

/**
 * Private bytes (or platform proxy) for a set of PIDs.
 * @param {number[]} pids
 * @returns {Promise<Map<number, number>>} pid → bytes
 */
async function privateBytesForPids(pids) {
  const unique = [...new Set(pids.filter((p) => Number.isInteger(p) && p > 0))];
  if (unique.length === 0) return new Map();
  if (platform() === "win32") {
    return privateBytesWindows(unique);
  }
  if (platform() === "linux") {
    return privateBytesLinux(unique);
  }
  return privateBytesDarwin(unique);
}

async function privateBytesWindows(pids) {
  // Get-Process PrivateMemorySize64 = private bytes (authoritative on Windows).
  const list = pids.join(",");
  const script = [
    `$ErrorActionPreference = 'SilentlyContinue'`,
    `$pids = @(${list})`,
    `$rows = foreach ($p in $pids) { $proc = Get-Process -Id $p -ErrorAction SilentlyContinue; if ($null -ne $proc) { [pscustomobject]@{ pid = $p; private_bytes = [int64]$proc.PrivateMemorySize64 } } }`,
    `$rows | ConvertTo-Json -Compress`,
  ].join("; ");
  try {
    const { stdout } = await execFileAsync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-Command", script],
      { timeout: 20000, windowsHide: true, maxBuffer: 4 * 1024 * 1024 }
    );
    const trimmed = stdout.trim();
    if (!trimmed) return new Map();
    const parsed = JSON.parse(trimmed);
    const rows = Array.isArray(parsed) ? parsed : [parsed];
    const map = new Map();
    for (const r of rows) {
      const pid = Number(r.pid);
      const bytes = Number(r.private_bytes);
      if (Number.isInteger(pid) && Number.isFinite(bytes) && bytes >= 0) {
        map.set(pid, bytes);
      }
    }
    return map;
  } catch (err) {
    console.warn(`[measure] Windows private-byte sample failed: ${err.message ?? err}`);
    return new Map();
  }
}

async function privateBytesLinux(pids) {
  const map = new Map();
  for (const pid of pids) {
    try {
      const status = readFileSync(`/proc/${pid}/status`, "utf8");
      // Prefer RssAnon (anonymous private-ish); fall back to VmRSS.
      let kb = null;
      const anon = status.match(/^RssAnon:\s+(\d+)\s+kB/m);
      if (anon) {
        kb = Number.parseInt(anon[1], 10);
      } else {
        const rss = status.match(/^VmRSS:\s+(\d+)\s+kB/m);
        if (rss) kb = Number.parseInt(rss[1], 10);
      }
      if (kb !== null && Number.isFinite(kb)) {
        map.set(pid, kb * 1024);
      }
    } catch {
      // process may have exited
    }
  }
  return map;
}

async function privateBytesDarwin(pids) {
  const map = new Map();
  for (const pid of pids) {
    try {
      const { stdout } = await execFileAsync("ps", ["-o", "rss=", "-p", String(pid)], {
        timeout: 5000,
      });
      const kb = Number.parseInt(stdout.trim(), 10);
      if (Number.isFinite(kb) && kb >= 0) {
        map.set(pid, kb * 1024);
      }
    } catch {
      // ignore
    }
  }
  return map;
}

/**
 * Expand root PIDs to the full process tree (children by ppid), deduped.
 * @param {number[]} roots
 * @param {Array<{ pid: number, ppid: number }>} all
 */
function expandProcessTree(roots, all) {
  const byParent = new Map();
  for (const p of all) {
    if (!byParent.has(p.ppid)) byParent.set(p.ppid, []);
    byParent.get(p.ppid).push(p.pid);
  }
  const out = new Set();
  const stack = [...roots];
  while (stack.length) {
    const pid = stack.pop();
    if (out.has(pid)) continue;
    out.add(pid);
    for (const child of byParent.get(pid) ?? []) {
      stack.push(child);
    }
  }
  return [...out];
}

/**
 * Full platform process table (for tree expansion). On Windows we need more
 * than just cognis candidates so children of cognis are included even if their
 * command line lacks the marker.
 */
async function listAllProcessesForTree() {
  if (platform() === "win32") {
    const script = [
      "$ErrorActionPreference = 'SilentlyContinue'",
      "Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId | ConvertTo-Json -Compress",
    ].join("; ");
    try {
      const { stdout } = await execFileAsync(
        "powershell",
        ["-NoProfile", "-NonInteractive", "-Command", script],
        { timeout: 30000, windowsHide: true, maxBuffer: 16 * 1024 * 1024 }
      );
      const trimmed = stdout.trim();
      if (!trimmed) return [];
      const parsed = JSON.parse(trimmed);
      const rows = Array.isArray(parsed) ? parsed : [parsed];
      return rows
        .map((r) => ({
          pid: Number(r.ProcessId),
          ppid: Number(r.ParentProcessId) || 0,
        }))
        .filter((r) => Number.isInteger(r.pid) && r.pid > 0);
    } catch {
      return [];
    }
  }
  // POSIX: reuse ps listing without cognis filter.
  try {
    const { stdout } = await execFileAsync("ps", ["-eo", "pid=,ppid="], {
      timeout: 15000,
      maxBuffer: 8 * 1024 * 1024,
    });
    const out = [];
    for (const line of stdout.split(/\r?\n/)) {
      const m = line.trim().match(/^(\d+)\s+(\d+)$/);
      if (!m) continue;
      out.push({
        pid: Number.parseInt(m[1], 10),
        ppid: Number.parseInt(m[2], 10),
      });
    }
    return out;
  } catch {
    return [];
  }
}

function metricMeta() {
  if (platform() === "win32") {
    return {
      os: "win32",
      metric: "private_bytes",
      source: "Get-Process.PrivateMemorySize64",
      authoritative: true,
      note: "Windows private bytes over process tree (authoritative for 2.11 gate)",
    };
  }
  if (platform() === "linux") {
    return {
      os: "linux",
      metric: "rss_anon_or_vmrss",
      source: "/proc/<pid>/status RssAnon|VmRSS",
      authoritative: false,
      note: "Linux proxy — not interchangeable with Windows private bytes",
    };
  }
  return {
    os: platform(),
    metric: "rss",
    source: "ps -o rss=",
    authoritative: false,
    note: "macOS/other RSS proxy — not interchangeable with Windows private bytes",
  };
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/**
 * Sample Cognis daemons related to the isolation root (or filter path).
 *
 * When `knownPids` is provided (spawned by this harness), those PIDs are always
 * included — required because HTTP mcpd command lines may not embed the repo
 * path and Windows cannot read another process's environment.
 */
async function sampleTopology({
  filterRoot,
  declaredA,
  declaredH,
  declaredI,
  knownHandles,
}) {
  const candidates = await listCandidateProcesses();
  const knownPids = new Set(
    (knownHandles ?? [])
      .map((h) => h.pid)
      .filter((p) => Number.isInteger(p) && p > 0)
  );
  const knownClassByPid = new Map();
  for (const h of knownHandles ?? []) {
    if (h.pid) knownClassByPid.set(h.pid, h.class);
  }

  const scoped = candidates.filter((p) => {
    if (knownPids.has(p.pid)) return true;
    const cls = classifyProcess(p);
    if (!cls || cls === "other_cognis") {
      if (cls === "other_cognis") return false;
      if (!cls) return false;
    }
    return processReferencesPath(p, filterRoot);
  });

  // If filter is too strict and we have no known PIDs, fall back to machine-wide
  // classified cognis daemons and mark scope as machine-wide.
  let used = scoped;
  let repoScoped = true;
  if (filterRoot && scoped.length === 0 && knownPids.size === 0) {
    used = candidates.filter((p) => {
      const cls = classifyProcess(p);
      return cls === "heavy_mcpd" || cls === "thin_proxy" || cls === "indexd";
    });
    repoScoped = false;
  }

  // Merge known handles that may have exited from the CIM list but still need
  // classification. Only keep known PIDs that still have private-byte samples
  // (i.e. are still alive) — dead spawn PIDs must not count as orphans.
  const byPid = new Map();
  for (const p of used) {
    byPid.set(p.pid, p);
  }
  // Probe aliveness of known PIDs missing from the OS list.
  const missingKnown = [...knownPids].filter((pid) => !byPid.has(pid));
  if (missingKnown.length > 0) {
    const aliveBytes = await privateBytesForPids(missingKnown);
    for (const pid of missingKnown) {
      if (aliveBytes.has(pid)) {
        byPid.set(pid, {
          pid,
          ppid: 0,
          name: "cognis",
          commandLine: knownClassByPid.get(pid) ?? "cognis",
        });
      }
    }
  }

  const classified = [...byPid.values()]
    .map((p) => {
      let cls = classifyProcess(p);
      if ((!cls || cls === "other_cognis") && knownClassByPid.has(p.pid)) {
        cls = knownClassByPid.get(p.pid);
      }
      return { ...p, class: cls };
    })
    .filter(
      (p) =>
        p.class === "heavy_mcpd" ||
        p.class === "thin_proxy" ||
        p.class === "indexd"
    );

  const rootPids = classified.map((p) => p.pid);
  const allForTree = await listAllProcessesForTree();
  const treePids = expandProcessTree(
    rootPids,
    allForTree.length ? allForTree : classified
  );
  const bytesMap = await privateBytesForPids(treePids);

  let aggregateBytes = 0;
  for (const pid of treePids) {
    aggregateBytes += bytesMap.get(pid) ?? 0;
  }

  const heavyMcpd = classified.filter((p) => p.class === "heavy_mcpd");
  const thin = classified.filter((p) => p.class === "thin_proxy");
  const indexd = classified.filter((p) => p.class === "indexd");
  // "Heavy repository daemons" for the ≤ A gate are heavy mcpd owners
  // (one per canonical repo). indexd is gated separately as ≤ I and ≤ 1/repo
  // (Requirement 2.11). Both are still "heavy" for private-byte aggregation.
  const heavyDaemons = heavyMcpd;

  const processes = classified.map((p) => {
    const tree = expandProcessTree(
      [p.pid],
      allForTree.length ? allForTree : classified
    );
    let treeBytes = 0;
    for (const t of tree) treeBytes += bytesMap.get(t) ?? 0;
    return {
      pid: p.pid,
      ppid: p.ppid,
      class: p.class,
      name: p.name,
      command_line: (p.commandLine ?? "").slice(0, 500),
      private_bytes: bytesMap.get(p.pid) ?? 0,
      tree_private_bytes: treeBytes,
      tree_private_bytes_gib: round6(bytesToGib(treeBytes)),
    };
  });

  const A = declaredA;
  const H = declaredH ?? heavyMcpd.length + thin.length;
  const I = declaredI ?? indexd.length;

  return {
    sampled_at: nowIso(),
    repo_scoped: repoScoped,
    filter_root: filterRoot ?? null,
    known_pids: [...knownPids],
    A,
    H,
    I,
    counts: {
      heavy_mcpd: heavyMcpd.length,
      thin_proxy: thin.length,
      indexd: indexd.length,
      // Alias used by ≤ A gate (heavy mcpd only; indexd is separate).
      heavy_repository_daemons: heavyDaemons.length,
      tree_pids: treePids.length,
    },
    inequalities: {
      heavy_le_A: heavyDaemons.length <= A,
      indexd_le_I: indexd.length <= I,
      indexd_le_1_per_repo: indexd.length <= A,
      thin_le_H: thin.length <= H,
    },
    aggregate_private_bytes: aggregateBytes,
    aggregate_private_bytes_gib: round6(bytesToGib(aggregateBytes)),
    processes,
  };
}

// ---------------------------------------------------------------------------
// Spawn topology under isolation
// ---------------------------------------------------------------------------

/** Ephemeral loopback port (0 → OS assigns; we pick a free high port). */
function pickPort() {
  // Deterministic-enough free port from pid + entropy; collisions retried by caller.
  return 20000 + (process.pid % 10000) + Math.floor(Math.random() * 20000);
}

/**
 * @returns {{ child: import('node:child_process').ChildProcess, pid: number, class: string, repo: object }}
 */
function spawnDaemon({ binary, args, env, className, repo, stdioMode }) {
  // stdio heavy mcpd exits when stdin is closed; for long-lived idle samples we
  // prefer HTTP. When stdio is required (thin proxy / active-load), keep stdin
  // open as a pipe so the process stays alive until we kill it.
  const stdio =
    stdioMode === "pipe-stdin"
      ? ["pipe", "ignore", "pipe"]
      : ["ignore", "ignore", "pipe"];
  const child = spawn(binary, args, {
    env,
    stdio,
    windowsHide: true,
    detached: false,
  });
  let stderr = "";
  child.stderr?.on("data", (buf) => {
    stderr += buf.toString("utf8");
    if (stderr.length > 4000) stderr = stderr.slice(-4000);
  });
  child.on("error", (err) => {
    console.warn(`[measure] spawn error (${className}): ${err.message}`);
  });
  return {
    child,
    pid: child.pid ?? 0,
    class: className,
    repo,
    getStderr: () => stderr,
  };
}

function repoEnv(iso, repo, opts) {
  return {
    ...isolationEnv(iso),
    COGNIS_REPO_ROOT: repo.repoRoot,
    COGNIS_DB_PATH: repo.dbPath,
    COGNIS_AUDIT_LOG: repo.auditLog,
    COGNIS_INDEXD_STATUS_PATH: repo.statusPath,
    COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: opts.warmSemantic,
    // Fixture mode avoids requiring a pre-built UCKG for cardinality/RAM
    // sampling of process shape. Real-DB measurement can set COGNIS_PB_USE_REAL_DB=1
    // after placing a DB under the isolation root.
    COGNIS_MCP_FIXTURE: process.env.COGNIS_PB_USE_REAL_DB === "1" ? "0" : "1",
  };
}

/**
 * Start a measured topology. Returns handles for shutdown.
 *
 * Heavy mcpd is started with `--transport http` on loopback so the process
 * stays alive without an editor holding stdio (required for stabilized-idle
 * private-byte sampling). Thin proxies keep stdio with an open stdin pipe.
 */
function startTopology(iso, repos, opts) {
  const handles = [];
  if (opts.topology === "sample-only") {
    return handles;
  }
  if (!opts.binary || !existsSync(opts.binary)) {
    throw new Error(
      `--binary is required for topology ${opts.topology} (got ${opts.binary || "(empty)"})`
    );
  }

  const useProxy = opts.topology === "fixed-idle";
  const hosts = opts.fanOutHosts;

  for (const repo of repos) {
    // One indexd per repo (I). Command line includes repo path so sampling can
    // scope to the isolation root (preservation 3.10).
    const indexEnv = repoEnv(iso, repo, opts);
    handles.push(
      spawnDaemon({
        binary: opts.binary,
        args: ["indexd", repo.repoRoot],
        env: indexEnv,
        className: "indexd",
        repo,
      })
    );

    // H client-facing mcpd processes per repo (or thin proxies in fixed-idle).
    // fan-out-hosts > 1 reproduces the defect-style host×repo heavy multiplicity.
    for (let h = 0; h < hosts; h += 1) {
      const env = repoEnv(iso, repo, opts);
      let args;
      let className;
      let stdioMode = "ignore";
      if (useProxy) {
        // Thin proxy is model-free stdio; keep stdin open so it does not EOF-exit.
        env[THIN_PROXY_ENV] = "1";
        args = ["mcpd", "--proxy"];
        className = "thin_proxy";
        stdioMode = "pipe-stdin";
      } else {
        // Long-lived heavy daemon via loopback HTTP (command line still matches
        // mcpd classification; path in COGNIS_* env / status for scoping).
        const port = pickPort();
        const token = randomBytes(16).toString("hex");
        env.COGNIS_MCP_ROUTE_TOKEN = token;
        args = [
          "mcpd",
          "--transport",
          "http",
          "--host",
          "127.0.0.1",
          "--port",
          String(port),
        ];
        className = "heavy_mcpd";
      }
      handles.push(
        spawnDaemon({
          binary: opts.binary,
          args,
          env,
          className,
          repo,
          stdioMode,
        })
      );
    }
  }

  return handles;
}

async function stopHandles(handles, graceS) {
  for (const h of handles) {
    try {
      if (h.child && !h.child.killed) {
        if (platform() === "win32" && h.pid) {
          try {
            execFileSync("taskkill", ["/PID", String(h.pid), "/T", "/F"], {
              stdio: "ignore",
              windowsHide: true,
            });
          } catch {
            try {
              h.child.kill();
            } catch {
              // ignore
            }
          }
        } else {
          h.child.kill("SIGTERM");
        }
      }
    } catch {
      // ignore
    }
  }
  // Brief wait for voluntary exit, then grace period for orphan check.
  await sleep(1000);
  for (const h of handles) {
    try {
      if (h.child && !h.child.killed && h.child.exitCode === null) {
        h.child.kill("SIGKILL");
      }
    } catch {
      // ignore
    }
  }
  if (graceS > 0) {
    await sleep(Math.round(graceS * 1000));
  }
}

/**
 * Optional active-load: issue a trivial stdin JSON-RPC initialize/tools path
 * against one heavy mcpd if possible. Best-effort; failures are non-fatal.
 */
async function driveActiveLoad(handles, binary, iso, repos) {
  // Spawn a short-lived mcpd with fixture and send one initialize + tools/list
  // over stdio to force a bit of work; sample peak is taken by the caller
  // around this window.
  if (!binary || !existsSync(binary) || repos.length === 0) {
    return { driven: false, reason: "no binary/repos" };
  }
  const repo = repos[0];
  const env = repoEnv(iso, repo, { warmSemantic: "0" });
  const child = spawn(binary, ["mcpd"], {
    env,
    stdio: ["pipe", "pipe", "ignore"],
    windowsHide: true,
  });
  const payload = (id, method, params) =>
    JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
  try {
    await sleep(500);
    child.stdin.write(
      payload(1, "initialize", {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "pb-measure", version: "0.0.0" },
      })
    );
    child.stdin.write(payload(2, "tools/list", {}));
    await sleep(1500);
  } catch {
    // ignore
  } finally {
    try {
      child.kill();
    } catch {
      // ignore
    }
  }
  return { driven: true };
}

// ---------------------------------------------------------------------------
// One clean run
// ---------------------------------------------------------------------------

async function runOnce(runIndex, opts, sharedMeta) {
  const iso = createIsolationRoot();
  const repos = [];
  for (let i = 0; i < opts.repos; i += 1) {
    repos.push(materializeRepo(iso, i));
  }

  const A = repos.length;
  // Declared H: active MCP client connections we intend to open.
  const H =
    opts.topology === "sample-only"
      ? 0
      : A * opts.fanOutHosts;
  // Declared I: we start one indexd per repo for non-sample topologies.
  const I = opts.topology === "sample-only" ? 0 : A;

  const filterRoot =
    opts.topology === "sample-only"
      ? opts.filterPath || iso.root
      : iso.root;

  /** @type {any[]} */
  let handles = [];
  let spawnError = null;
  try {
    if (opts.topology !== "sample-only") {
      handles = startTopology(iso, repos, opts);
      await sleep(Math.round(opts.settleS * 1000));
    } else {
      // External topology: just settle briefly so the operator can attach.
      await sleep(Math.round(Math.min(opts.settleS, 2) * 1000));
    }

    const idleSample = await sampleTopology({
      filterRoot,
      declaredA: A,
      declaredH: Math.max(H, 1),
      declaredI: Math.max(I, 0),
      knownHandles: handles,
    });

    // For sample-only, re-derive A/H/I from observation when declared zeros.
    if (opts.topology === "sample-only") {
      idleSample.A = Math.max(idleSample.counts.heavy_repository_daemons, 1);
      idleSample.H = Math.max(
        idleSample.counts.heavy_mcpd + idleSample.counts.thin_proxy,
        1
      );
      idleSample.I = Math.max(idleSample.counts.indexd, 0);
      idleSample.inequalities = {
        heavy_le_A:
          idleSample.counts.heavy_repository_daemons <= idleSample.A,
        indexd_le_I:
          idleSample.counts.indexd <=
          Math.max(idleSample.I, idleSample.counts.indexd),
        indexd_le_1_per_repo: idleSample.counts.indexd <= idleSample.A,
        thin_le_H: idleSample.counts.thin_proxy <= idleSample.H,
      };
    }

    let activePeak = null;
    if (opts.activeLoad) {
      await driveActiveLoad(handles, opts.binary, iso, repos);
      const peakSample = await sampleTopology({
        filterRoot,
        declaredA: idleSample.A,
        declaredH: idleSample.H,
        declaredI: idleSample.I,
        knownHandles: handles,
      });
      activePeak = {
        aggregate_private_bytes: peakSample.aggregate_private_bytes,
        aggregate_private_bytes_gib: peakSample.aggregate_private_bytes_gib,
        counts: peakSample.counts,
      };
    }

    const stoppedPids = handles.map((h) => h.pid).filter(Boolean);
    await stopHandles(handles, opts.graceS);
    handles = [];

    // Orphan check: only count Cognis daemons still alive that either
    // (a) were in our spawn set, or (b) reference the isolation root.
    // Never attribute the developer's real machine-wide cognis processes
    // as orphans of this run (preservation 3.10).
    const postGrace = await sampleTopology({
      filterRoot,
      declaredA: idleSample.A,
      declaredH: idleSample.H,
      declaredI: idleSample.I,
      knownHandles: stoppedPids.map((pid) => ({ pid, class: "heavy_mcpd" })),
    });
    const orphanProcs = (postGrace.processes ?? []).filter((p) => {
      if (stoppedPids.includes(p.pid)) return true;
      return processReferencesPath(
        { commandLine: p.command_line, name: p.name },
        filterRoot
      );
    });
    const orphans = orphanProcs.length;

    const run = {
      run_index: runIndex,
      isolation_root: iso.root,
      topology: opts.topology,
      A: idleSample.A,
      H: idleSample.H,
      I: idleSample.I,
      idle: idleSample,
      active_load_peak: activePeak,
      post_grace: {
        orphan_count: orphans,
        orphan_pids: orphanProcs.map((p) => p.pid),
        counts: {
          heavy_mcpd: orphanProcs.filter((p) => p.class === "heavy_mcpd").length,
          thin_proxy: orphanProcs.filter((p) => p.class === "thin_proxy").length,
          indexd: orphanProcs.filter((p) => p.class === "indexd").length,
        },
        zero_orphans: orphans === 0,
        raw_sample_counts: postGrace.counts,
      },
      spawn_error: spawnError,
      gates: {
        below_baseline:
          idleSample.aggregate_private_bytes_gib <= BASELINE_PRIVATE_BYTES_GIB,
        heavy_le_A: idleSample.inequalities.heavy_le_A,
        indexd_le_I: idleSample.inequalities.indexd_le_I,
        thin_le_H: idleSample.inequalities.thin_le_H,
        zero_orphans_after_grace: orphans === 0,
      },
    };

    // Persist per-run snapshot under isolation out/ (and optional keep).
    try {
      writeFileSync(
        join(iso.outDir, `run-${runIndex}.json`),
        JSON.stringify(run, null, 2),
        "utf8"
      );
    } catch {
      // ignore
    }

    return { run, iso };
  } catch (err) {
    spawnError = String(err?.message ?? err);
    await stopHandles(handles, 0);
    return {
      run: {
        run_index: runIndex,
        isolation_root: iso.root,
        topology: opts.topology,
        error: spawnError,
        A,
        H,
        I,
      },
      iso,
    };
  } finally {
    if (!opts.keepIsolation) {
      try {
        rmSync(iso.root, { recursive: true, force: true });
      } catch {
        // Best effort; OS reclaims temp.
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Report aggregation
// ---------------------------------------------------------------------------

function buildReport(opts, runs, meta) {
  const idleGibs = runs
    .map((r) => r.idle?.aggregate_private_bytes_gib)
    .filter((n) => typeof n === "number" && Number.isFinite(n));
  const heavyCounts = runs.map((r) => r.idle?.counts?.heavy_repository_daemons ?? null);
  const thinCounts = runs.map((r) => r.idle?.counts?.thin_proxy ?? null);
  const indexdCounts = runs.map((r) => r.idle?.counts?.indexd ?? null);
  const orphanFlags = runs.map((r) => r.post_grace?.zero_orphans === true);

  const med = median(idleGibs);
  const summary = {
    n: runs.length,
    idle_aggregate_private_bytes_gib: {
      median: med === null ? null : round6(med),
      min: minOf(idleGibs) === null ? null : round6(minOf(idleGibs)),
      max: maxOf(idleGibs) === null ? null : round6(maxOf(idleGibs)),
      samples: idleGibs.map(round6),
    },
    cardinality: {
      heavy_repository_daemons: {
        median: median(heavyCounts.filter((x) => x !== null)),
        samples: heavyCounts,
      },
      thin_proxies: {
        median: median(thinCounts.filter((x) => x !== null)),
        samples: thinCounts,
      },
      indexd: {
        median: median(indexdCounts.filter((x) => x !== null)),
        samples: indexdCounts,
      },
    },
    gates: {
      enough_runs: runs.length >= MIN_RUNS,
      median_at_or_below_target:
        med !== null && med <= TARGET_MEDIAN_PRIVATE_BYTES_GIB,
      no_run_above_baseline: idleGibs.every((g) => g <= BASELINE_PRIVATE_BYTES_GIB),
      heavy_le_A: runs.every((r) => r.gates?.heavy_le_A !== false),
      indexd_le_I: runs.every((r) => r.gates?.indexd_le_I !== false),
      thin_le_H: runs.every((r) => r.gates?.thin_le_H !== false),
      zero_orphans_after_grace: orphanFlags.every(Boolean),
    },
  };

  // Target is not claimed achieved unless the median gate passes — expose
  // that explicitly for docs/changelog consumers (task 9.3).
  summary.target_status =
    summary.gates.median_at_or_below_target && summary.gates.enough_runs
      ? "median_meets_target_on_this_topology"
      : "target_not_claimed_achieved";

  return {
    schema_version: SCHEMA_VERSION,
    evidence_tier: "empirical",
    generated_at: nowIso(),
    procedure: "tests/e2e/private-bytes/README.md",
    platform: metricMeta(),
    hardware_label: meta.hardware_label,
    build: meta.build,
    constants: {
      baseline_private_bytes_gib: BASELINE_PRIVATE_BYTES_GIB,
      target_median_private_bytes_gib: TARGET_MEDIAN_PRIVATE_BYTES_GIB,
      min_runs: MIN_RUNS,
      grace_period_s: opts.graceS,
      settle_s: opts.settleS,
    },
    topology: {
      name: opts.topology,
      repos_requested: opts.repos,
      fan_out_hosts: opts.fanOutHosts,
      warm_semantic: opts.warmSemantic,
      active_load: opts.activeLoad,
      A: runs[0]?.A ?? opts.repos,
      H: runs[0]?.H ?? opts.repos * opts.fanOutHosts,
      I: runs[0]?.I ?? (opts.topology === "sample-only" ? 0 : opts.repos),
    },
    runs,
    summary,
    disclaimer:
      "Finite-sample measurement for the named hardware/build/model/topology. " +
      "Not a universal guarantee. Do not present the 0.615 GiB target as achieved " +
      "unless summary.gates.median_at_or_below_target is true on this report.",
  };
}

function printSummary(report) {
  const s = report.summary;
  const idle = s.idle_aggregate_private_bytes_gib;
  console.log("");
  console.log("=== Private-byte / process-cardinality report ===");
  console.log(`evidence_tier:     ${report.evidence_tier}`);
  console.log(
    `platform:          ${report.platform.os} metric=${report.platform.metric} authoritative=${report.platform.authoritative}`
  );
  console.log(`hardware:          ${report.hardware_label}`);
  console.log(`topology:          ${report.topology.name} A=${report.topology.A} H=${report.topology.H} I=${report.topology.I}`);
  console.log(`runs:              n=${s.n} (gate needs ≥${MIN_RUNS})`);
  console.log(
    `idle private GiB:  median=${idle.median} min=${idle.min} max=${idle.max}`
  );
  console.log(
    `baseline ceiling:  ${BASELINE_PRIVATE_BYTES_GIB} GiB  target median: ${TARGET_MEDIAN_PRIVATE_BYTES_GIB} GiB`
  );
  console.log(`target_status:     ${s.target_status}`);
  console.log("gates:");
  for (const [k, v] of Object.entries(s.gates)) {
    console.log(`  ${v ? "PASS" : "FAIL"}  ${k}`);
  }
  console.log("");
}

// ---------------------------------------------------------------------------
// Self-test (pure helpers — no daemon spawn)
// ---------------------------------------------------------------------------

function assert(cond, msg) {
  if (!cond) throw new Error(`self-test failed: ${msg}`);
}

function runSelfTest() {
  // median
  assert(median([]) === null, "median empty");
  assert(median([3]) === 3, "median single");
  assert(median([1, 3, 2]) === 2, "median odd");
  assert(median([1, 2, 3, 4]) === 2.5, "median even");

  // classify
  assert(
    classifyProcess({
      pid: 1,
      commandLine: "C:\\\\tools\\\\cognis.exe mcpd --db x",
    }) === "heavy_mcpd",
    "heavy mcpd"
  );
  assert(
    classifyProcess({
      pid: 2,
      commandLine: "cognis mcpd --proxy",
    }) === "thin_proxy",
    "thin proxy --proxy"
  );
  assert(
    classifyProcess({
      pid: 3,
      commandLine: "cognis mcpd --transport proxy",
    }) === "thin_proxy",
    "thin proxy transport"
  );
  assert(
    classifyProcess({
      pid: 4,
      commandLine: "cognis indexd D:\\\\tmp\\\\repo",
    }) === "indexd",
    "indexd"
  );
  assert(
    classifyProcess({
      pid: 5,
      commandLine: "node tests/e2e/private-bytes/measure.mjs",
    }) === null,
    "ignore harness"
  );
  assert(
    classifyProcess({ pid: 6, commandLine: "notepad.exe" }) === null,
    "ignore unrelated"
  );

  // process tree expand
  const tree = expandProcessTree(
    [10],
    [
      { pid: 10, ppid: 1 },
      { pid: 11, ppid: 10 },
      { pid: 12, ppid: 11 },
      { pid: 99, ppid: 1 },
    ]
  );
  assert(tree.includes(10) && tree.includes(11) && tree.includes(12), "tree children");
  assert(!tree.includes(99), "tree excludes siblings");

  // isolation root never under real home by construction (temp prefix)
  const iso = createIsolationRoot();
  try {
    assert(iso.root.includes("cognis-pb-measure-"), "iso prefix");
    assert(pathIsUnder(join(iso.repos, "x"), iso.root), "repos under root");
    const env = isolationEnv(iso);
    assert(env.HOME === iso.home, "HOME redirected");
    assert(env.USERPROFILE === iso.home, "USERPROFILE redirected");
    assert(!env.COGNIS_DB_PATH, "no leaked COGNIS_DB_PATH");
    // Real developer home must not equal isolation home.
    const realHome = process.env.USERPROFILE || process.env.HOME || "";
    if (realHome) {
      assert(
        normalizePathKey(iso.home) !== normalizePathKey(realHome),
        "isolation home ≠ real home"
      );
    }
    const repo = materializeRepo(iso, 0);
    assert(existsSync(join(repo.repoRoot, "src", "main.py")), "fixture file");
    assert(pathIsUnder(repo.cognisDir, iso.root), ".cognis under isolation");
  } finally {
    rmSync(iso.root, { recursive: true, force: true });
  }

  // gate arithmetic sample
  const fakeRuns = [
    {
      idle: {
        aggregate_private_bytes_gib: 1.0,
        counts: {
          heavy_repository_daemons: 3,
          thin_proxy: 0,
          indexd: 2,
        },
      },
      post_grace: { zero_orphans: true },
      gates: {
        heavy_le_A: true,
        indexd_le_I: true,
        thin_le_H: true,
      },
      A: 3,
      H: 3,
      I: 2,
    },
    {
      idle: {
        aggregate_private_bytes_gib: 0.9,
        counts: {
          heavy_repository_daemons: 3,
          thin_proxy: 0,
          indexd: 2,
        },
      },
      post_grace: { zero_orphans: true },
      gates: {
        heavy_le_A: true,
        indexd_le_I: true,
        thin_le_H: true,
      },
      A: 3,
      H: 3,
      I: 2,
    },
  ];
  const rep = buildReport(
    {
      graceS: 35,
      settleS: 8,
      topology: "baseline-idle",
      repos: 3,
      fanOutHosts: 1,
      warmSemantic: "0",
      activeLoad: false,
    },
    fakeRuns,
    { hardware_label: "self-test", build: { binary: null, version: "self-test" } }
  );
  assert(rep.summary.n === 2, "report n");
  assert(rep.summary.idle_aggregate_private_bytes_gib.median === 0.95, "report median");
  assert(rep.summary.target_status === "target_not_claimed_achieved", "target not claimed");
  assert(rep.summary.gates.enough_runs === false, "enough_runs false for n=2");
  assert(rep.evidence_tier === "empirical", "empirical tier");
  assert(rep.constants.baseline_private_bytes_gib === 1.23, "baseline const");
  assert(rep.constants.target_median_private_bytes_gib === 0.615, "target const");

  console.log("[measure] self-test OK");
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (err) {
    console.error(String(err.message ?? err));
    process.exitCode = 2;
    return;
  }
  if (opts.help) {
    printHelp();
    return;
  }
  if (opts.selfTest) {
    try {
      runSelfTest();
    } catch (err) {
      console.error(err);
      process.exitCode = 1;
    }
    return;
  }

  if (!opts.binary) {
    opts.binary = findDefaultBinary();
  }
  if (opts.binary) {
    opts.binary = resolve(opts.binary);
  }

  if (opts.dryRun) {
    console.log(
      JSON.stringify(
        {
          dry_run: true,
          opts,
          constants: {
            BASELINE_PRIVATE_BYTES_GIB,
            TARGET_MEDIAN_PRIVATE_BYTES_GIB,
            MIN_RUNS,
            DEFAULT_GRACE_PERIOD_S,
          },
          isolation: "os.tmpdir()/cognis-pb-measure-*",
          metric: metricMeta(),
        },
        null,
        2
      )
    );
    return;
  }

  if (opts.topology !== "sample-only" && (!opts.binary || !existsSync(opts.binary))) {
    console.error(
      "error: --binary path is required and must exist for topology " +
        `${opts.topology}. Build with: cargo build -p cognis --release`
    );
    process.exitCode = 2;
    return;
  }

  let version = "unknown";
  if (opts.binary && existsSync(opts.binary)) {
    try {
      const { stdout } = await execFileAsync(opts.binary, ["--version"], {
        timeout: 5000,
        windowsHide: true,
      });
      version = stdout.trim().split(/\r?\n/)[0] || "unknown";
    } catch {
      try {
        const { stdout } = await execFileAsync(opts.binary, ["cli", "--version"], {
          timeout: 5000,
          windowsHide: true,
        });
        version = stdout.trim().split(/\r?\n/)[0] || "unknown";
      } catch {
        version = "unknown";
      }
    }
  }

  const meta = {
    hardware_label: `${hostname()} ${platform()}-${arch()} ${release()}`,
    build: {
      binary: opts.binary || null,
      version,
      git_sha: process.env.GITHUB_SHA || process.env.COGNIS_BUILD_SHA || null,
    },
  };

  console.log(
    `[measure] starting n=${opts.runs} topology=${opts.topology} binary=${opts.binary || "(sample-only)"}`
  );
  console.log(`[measure] isolation under ${tmpdir()} (never touches real .cognis / host MCP config)`);
  console.log(
    `[measure] metric=${metricMeta().metric} authoritative=${metricMeta().authoritative}`
  );

  const runs = [];
  for (let i = 0; i < opts.runs; i += 1) {
    console.log(`[measure] run ${i + 1}/${opts.runs} …`);
    const { run } = await runOnce(i + 1, opts, meta);
    if (run.error) {
      console.warn(`[measure] run ${i + 1} error: ${run.error}`);
    } else {
      console.log(
        `[measure] run ${i + 1}: idle=${run.idle?.aggregate_private_bytes_gib} GiB` +
          ` heavy=${run.idle?.counts?.heavy_repository_daemons}` +
          ` thin=${run.idle?.counts?.thin_proxy}` +
          ` indexd=${run.idle?.counts?.indexd}` +
          ` orphans=${run.post_grace?.orphan_count}`
      );
    }
    runs.push(run);
  }

  const report = buildReport(opts, runs, meta);
  printSummary(report);

  const outPath = opts.out
    ? resolve(opts.out)
    : resolve(__dirname, "out", `report-${runId()}.json`);
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, JSON.stringify(report, null, 2), "utf8");
  console.log(`[measure] wrote ${outPath}`);

  // Exit non-zero only on hard procedure failures (not on failing resource
  // gates — those are evidence). Hard failures: zero successful samples.
  const okSamples = runs.filter((r) => r.idle && !r.error).length;
  if (okSamples === 0 && opts.topology !== "sample-only") {
    console.error("[measure] no successful samples");
    process.exitCode = 1;
  }
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
