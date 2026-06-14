import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import * as path from "node:path";
import { promisify } from "node:util";
import { envMatchesRepo } from "./mcpEnv";

const execFileAsync = promisify(execFile);

const MCPD_MARKER = "cognis_mcpd";
const CACHE_MS = 4000;

export interface CognisMcpdRuntime {
  /** PIDs of live ``python -m cognis_mcpd.main`` processes (editor-spawned stdio servers). */
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
}

/** One candidate ``cognis_mcpd`` process, with its env when the OS exposes it. */
export interface McpdProcess {
  pid: number;
  env?: Record<string, string>;
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
 * Decide the repo-scoped runtime from a raw candidate list.
 *
 * Pure and side-effect free so it can be unit-tested with injected processes.
 * We only claim ``repoScoped`` when a repo was requested *and every candidate*
 * exposed its environment — otherwise an unreadable process could hide a real
 * match (false negative) or mask a foreign one (false positive), so we fall
 * back to the honest machine-wide count.
 */
export function scopeRuntime(
  procs: McpdProcess[],
  repoRoot?: string
): CognisMcpdRuntime {
  const canScope =
    !!repoRoot && procs.length > 0 && procs.every((p) => p.env !== undefined);
  if (canScope && repoRoot) {
    const matched = procs.filter((p) => envMatchesRepo(repoRoot, p.env ?? {}));
    return {
      pids: matched.map((p) => p.pid),
      count: matched.length,
      repoScoped: true,
    };
  }
  return {
    pids: procs.map((p) => p.pid),
    count: procs.length,
    repoScoped: false,
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
      return match ? env : undefined;
    }
  } catch {
    return undefined;
  }
  return undefined;
}

async function listCognisMcpdProcessesWindows(): Promise<McpdProcess[]> {
  // Match by command line (not Name) so venv/py.exe launchers are still caught.
  // Windows does not expose another process's environment via built-in tooling,
  // so these stay env-less and the runtime reports machine-wide (repoScoped=false).
  const script = [
    "Get-CimInstance Win32_Process",
    `| Where-Object { $_.CommandLine -match '${MCPD_MARKER}' }`,
    "| Select-Object -ExpandProperty ProcessId",
  ].join(" ");
  const { stdout } = await execFileAsync(
    "powershell",
    ["-NoProfile", "-NonInteractive", "-Command", script],
    { timeout: 8000, windowsHide: true }
  );
  return parsePids(stdout).map((pid) => ({ pid }));
}

async function listCognisMcpdProcessesPosix(): Promise<McpdProcess[]> {
  const { stdout } = await execFileAsync("ps", ["-eo", "pid=,args="], {
    timeout: 8000,
  });
  const pids = stdout
    .split(/\r?\n/)
    .filter((line) => line.includes(MCPD_MARKER))
    .map((line) => Number.parseInt(line.trim().split(/\s+/)[0] ?? "", 10))
    .filter((pid) => Number.isInteger(pid) && pid > 0);

  const procs: McpdProcess[] = [];
  for (const pid of pids) {
    procs.push({ pid, env: await readProcessEnv(pid) });
  }
  return procs;
}

/**
 * Count editor-managed Cognis MCP stdio processes, similar to how Cursor's MCP
 * settings show a live server. Best-effort: returns an empty list on failure.
 *
 * Pass ``repoRoot`` to scope the count to one repo where the OS allows reading
 * process environments (Linux/macOS). On Windows the result is machine-wide and
 * ``repoScoped`` is false.
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
