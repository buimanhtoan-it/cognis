import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import * as path from "node:path";
import { promisify } from "node:util";
import { envMatchesRepo } from "./mcpEnv";

const execFileAsync = promisify(execFile);

/**
 * Env var that marks a cognis-mcpd process as a model-free thin stdio proxy
 * (Requirements 2.8, 2.11). Kept here (not imported from mcpServer) so the
 * runtime probe stays free of the VS Code API surface and unit-testable in
 * plain Node. Mirrored in `bins/cognis-mcpd/src/proxy.rs` and `mcpServer.ts`.
 */
export const THIN_PROXY_ENV = "COGNIS_MCP_PROXY";

const MCPD_MARKER = "cognis_mcpd";
const CACHE_MS = 4000;

export interface CognisMcpdRuntime {
  /** PIDs of live ``cognis mcpd`` processes (editor-spawned stdio servers). */
  pids: number[];
  count: number;
  /**
   * True when ``pids`` is filtered to the requested repo (env-verified).
   *
   * False means a machine-wide best-effort count: the OS would not let us read
   * each process's environment (notably Windows via built-in tooling), so we
   * cannot prove the running server is bound to *this* repo's database.
   */
  repoScoped: boolean;
  /**
   * PIDs classified as model-free thin proxies (``COGNIS_MCP_PROXY=1`` or
   * command line contains ``--proxy``). Empty when classification is
   * unavailable (e.g. Windows without command-line detail). Requirement 2.11.
   */
  thinProxyPids: number[];
  /**
   * PIDs classified as heavy daemons (not thin proxies). On platforms where
   * classification is unavailable this equals ``pids`` (conservative: treat
   * unknown as heavy). Requirement 2.11.
   */
  heavyPids: number[];
  /** Count of thin proxies (``thinProxyPids.length``). */
  thinProxyCount: number;
  /** Count of heavy daemons (``heavyPids.length``). */
  heavyCount: number;
}

/** One candidate ``cognis_mcpd`` process, with its env when the OS exposes it. */
export interface McpdProcess {
  pid: number;
  env?: Record<string, string>;
  /**
   * Optional command line (Windows / ps args) used to classify thin-proxy
   * mode when the process environment is not readable.
   */
  commandLine?: string;
}

const cache = new Map<string, { at: number; runtime: CognisMcpdRuntime }>();

function parsePids(stdout: string): number[] {
  return stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => Number.parseInt(line, 10))
    .filter((pid) => Number.isInteger(pid) && pid > 0);
}

/** Parse a NUL-separated ``/proc/<pid>/environ`` blob into a key/value map. */
export function parseEnviron(blob: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const entry of blob.split("\0")) {
    if (!entry) {
      continue;
    }
    const eq = entry.indexOf("=");
    if (eq <= 0) {
      continue;
    }
    env[entry.slice(0, eq)] = entry.slice(eq + 1);
  }
  return env;
}

/**
 * Classify a single mcpd process as a thin proxy (model-free) or a heavy
 * daemon. Prefers env ``COGNIS_MCP_PROXY=1``; falls back to command-line
 * markers (``--proxy`` / ``--transport proxy``). When neither is available
 * the process is treated as heavy (conservative for the heavy budget).
 */
export function isThinProxyProcess(proc: McpdProcess): boolean {
  if (proc.env && (proc.env[THIN_PROXY_ENV] === "1" || proc.env[THIN_PROXY_ENV] === "true")) {
    return true;
  }
  const cmd = proc.commandLine ?? "";
  if (!cmd) {
    return false;
  }
  // Word-ish matches so we don't false-positive on unrelated flags.
  if (/(?:^|[\s"])--proxy(?:[\s"=]|$)/.test(cmd)) {
    return true;
  }
  if (/--transport(?:=|\s+)proxy\b/i.test(cmd)) {
    return true;
  }
  if (new RegExp(`${THIN_PROXY_ENV}=1`).test(cmd)) {
    return true;
  }
  return false;
}

/**
 * Decide the repo-scoped runtime from a raw candidate list.
 *
 * Pure and side-effect free so it can be unit-tested with injected processes.
 * We only claim ``repoScoped`` when a repo was requested *and every candidate*
 * exposed its environment — otherwise an unreadable process could hide a real
 * match (false negative) or mask a foreign one (false positive), so we fall
 * back to the honest machine-wide count.
 *
 * Thin-proxy vs heavy classification (Requirement 2.11) is applied after
 * scoping so the panel / measurement can report ``thinProxies ≤ H`` and
 * ``heavy ≤ A`` independently.
 */
export function scopeRuntime(
  procs: McpdProcess[],
  repoRoot?: string
): CognisMcpdRuntime {
  const canScope =
    !!repoRoot && procs.length > 0 && procs.every((p) => p.env !== undefined);
  const scoped = canScope && repoRoot
    ? procs.filter((p) => envMatchesRepo(repoRoot, p.env ?? {}))
    : procs;
  const pids = scoped.map((p) => p.pid);
  const thin = scoped.filter(isThinProxyProcess);
  const heavy = scoped.filter((p) => !isThinProxyProcess(p));
  return {
    pids,
    count: pids.length,
    repoScoped: Boolean(canScope && repoRoot),
    thinProxyPids: thin.map((p) => p.pid),
    heavyPids: heavy.map((p) => p.pid),
    thinProxyCount: thin.length,
    heavyCount: heavy.length,
  };
}

async function readProcessEnv(pid: number): Promise<Record<string, string> | undefined> {
  try {
    if (process.platform === "linux") {
      const blob = await readFile(`/proc/${pid}/environ`, "utf8");
      return parseEnviron(blob);
    }
    if (process.platform === "darwin") {
      // ``ps eww`` appends the environment after the command. Values can contain
      // spaces, so we only trust the COGNIS_* keys envMatchesRepo needs.
      const { stdout } = await execFileAsync(
        "ps",
        ["eww", "-o", "command=", "-p", String(pid)],
        { timeout: 8000 }
      );
      const env: Record<string, string> = {};
      const match = stdout.match(/\bCOGNIS_DB_PATH=(\S+)/);
      if (match) {
        env.COGNIS_DB_PATH = match[1];
      }
      const rootMatch = stdout.match(/\bCOGNIS_REPO_ROOT=(\S+)/);
      if (rootMatch) {
        env.COGNIS_REPO_ROOT = rootMatch[1];
      }
      const proxyMatch = stdout.match(new RegExp(`\\b${THIN_PROXY_ENV}=(\\S+)`));
      if (proxyMatch) {
        env[THIN_PROXY_ENV] = proxyMatch[1];
      }
      return match || proxyMatch ? env : undefined;
    }
  } catch {
    return undefined;
  }
  return undefined;
}

async function listCognisMcpdProcessesWindows(): Promise<McpdProcess[]> {
  // Match by command line (not Name) so venv/py.exe launchers are still caught.
  // Capture the command line so we can classify thin-proxy vs heavy even though
  // Windows does not expose another process's environment via built-in tooling.
  const script = [
    "Get-CimInstance Win32_Process",
    `| Where-Object { $_.CommandLine -match '${MCPD_MARKER}' }`,
    "| Select-Object ProcessId, CommandLine",
    "| ConvertTo-Json -Compress",
  ].join(" ");
  const { stdout } = await execFileAsync(
    "powershell",
    ["-NoProfile", "-NonInteractive", "-Command", script],
    { timeout: 8000, windowsHide: true }
  );
  const trimmed = stdout.trim();
  if (!trimmed) {
    return [];
  }
  try {
    const parsed = JSON.parse(trimmed) as
      | { ProcessId?: number; CommandLine?: string }
      | Array<{ ProcessId?: number; CommandLine?: string }>;
    const rows = Array.isArray(parsed) ? parsed : [parsed];
    const out: McpdProcess[] = [];
    for (const row of rows) {
      const pid = Number(row.ProcessId);
      if (!Number.isInteger(pid) || pid <= 0) {
        continue;
      }
      const proc: McpdProcess = { pid };
      if (typeof row.CommandLine === "string") {
        proc.commandLine = row.CommandLine;
      }
      out.push(proc);
    }
    return out;
  } catch {
    // Fall back to the pid-only listing if JSON parse fails.
    return parsePids(trimmed).map((pid) => ({ pid }));
  }
}

async function listCognisMcpdProcessesPosix(): Promise<McpdProcess[]> {
  const { stdout } = await execFileAsync("ps", ["-eo", "pid=,args="], {
    timeout: 8000,
  });
  const rows: Array<{ pid: number; commandLine: string }> = [];
  for (const line of stdout.split(/\r?\n/)) {
    if (!line.includes(MCPD_MARKER)) {
      continue;
    }
    const trimmed = line.trim();
    const sp = trimmed.indexOf(" ");
    const pidStr = sp === -1 ? trimmed : trimmed.slice(0, sp);
    const args = sp === -1 ? "" : trimmed.slice(sp + 1);
    const pid = Number.parseInt(pidStr, 10);
    if (!Number.isInteger(pid) || pid <= 0) {
      continue;
    }
    rows.push({ pid, commandLine: args });
  }

  const procs: McpdProcess[] = [];
  for (const row of rows) {
    procs.push({
      pid: row.pid,
      commandLine: row.commandLine,
      env: await readProcessEnv(row.pid),
    });
  }
  return procs;
}

/**
 * Count editor-managed Cognis MCP stdio processes, similar to how Cursor's MCP
 * settings show a live server. Best-effort: returns an empty list on failure.
 *
 * Pass ``repoRoot`` to scope the count to one repo where the OS allows reading
 * process environments (Linux/macOS). On Windows the result is machine-wide and
 * ``repoScoped`` is false; thin-proxy classification still works from the
 * command line (``--proxy`` / ``COGNIS_MCP_PROXY=1``).
 */
export async function getCognisMcpdRuntime(
  repoRoot?: string
): Promise<CognisMcpdRuntime> {
  const key = repoRoot ? path.resolve(repoRoot).toLowerCase() : "*";
  const now = Date.now();
  const hit = cache.get(key);
  if (hit && now - hit.at < CACHE_MS) {
    return hit.runtime;
  }

  let procs: McpdProcess[] = [];
  try {
    procs =
      process.platform === "win32"
        ? await listCognisMcpdProcessesWindows()
        : await listCognisMcpdProcessesPosix();
  } catch {
    procs = [];
  }

  const runtime = scopeRuntime(procs, repoRoot);
  cache.set(key, { at: now, runtime });
  return runtime;
}

/** Test hook: clear the process-list cache between assertions. */
export function resetMcpRuntimeCacheForTests(): void {
  cache.clear();
}
