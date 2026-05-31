import * as fs from "fs";
import { spawn, type ChildProcessWithoutNullStreams } from "child_process";
import * as path from "path";
import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { resolvePythonExecutable } from "./python";
import type { IndexStatusReport } from "./types";

const INDEXD_MODULE = "cognis_indexd.main";

interface LiveIndexingHandle {
  proc?: ChildProcessWithoutNullStreams;
  statusPath: string;
  statusWatcher?: fs.FSWatcher;
  pid?: number;
  external?: boolean;
}

export interface StartLiveIndexingOptions {
  forceFullRebuild?: boolean;
}

const processes = new Map<string, LiveIndexingHandle>();
const statuses = new Map<string, IndexStatusReport | undefined>();
const statusEmitter = new vscode.EventEmitter<{
  repoRoot: string;
  status: IndexStatusReport | undefined;
}>();

export const onDidChangeIndexStatus = statusEmitter.event;

function defaultStatusPath(repoRoot: string): string {
  return path.join(repoRoot, ".cognis", "indexd-status.json");
}

function normalizeIndexStatus(raw: unknown): IndexStatusReport | undefined {
  if (!raw || typeof raw !== "object") {
    return undefined;
  }
  const payload = raw as Record<string, unknown>;
  return {
    pid: typeof payload.pid === "number" ? payload.pid : undefined,
    active: Boolean(payload.active),
    phase: typeof payload.phase === "string" ? payload.phase : "starting",
    message:
      typeof payload.message === "string"
        ? payload.message
        : "Starting live indexing…",
    progressPercent:
      typeof payload.progress_percent === "number"
        ? payload.progress_percent
        : undefined,
    pendingCount:
      typeof payload.pending_count === "number" ? payload.pending_count : 0,
    pendingFiles: Array.isArray(payload.pending_files)
      ? payload.pending_files
          .filter((value): value is string => typeof value === "string")
          .slice(0, 8)
      : [],
    inflightCount:
      typeof payload.inflight_count === "number" ? payload.inflight_count : 0,
    inflightFiles: Array.isArray(payload.inflight_files)
      ? payload.inflight_files
          .filter((value): value is string => typeof value === "string")
          .slice(0, 8)
      : [],
    recentFiles: Array.isArray(payload.recent_files)
      ? payload.recent_files
          .filter((value): value is string => typeof value === "string")
          .slice(0, 8)
      : [],
    updatedAt:
      typeof payload.updated_at === "number"
        ? payload.updated_at
        : Date.now() / 1000,
    lastError:
      typeof payload.last_error === "string" ? payload.last_error : undefined,
  };
}

function isPidAlive(pid: number | undefined): boolean {
  if (!pid || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function isHandleRunning(handle: LiveIndexingHandle | undefined): boolean {
  if (!handle) {
    return false;
  }
  if (handle.proc) {
    return !handle.proc.killed;
  }
  return isPidAlive(handle.pid);
}

function isStatusProcessAlive(
  status: IndexStatusReport | undefined
): status is IndexStatusReport & { pid: number } {
  return Boolean(status?.active && status.pid && isPidAlive(status.pid));
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForPidExit(
  pid: number | undefined,
  timeoutMs = 5000
): Promise<void> {
  if (!pid || pid <= 0) {
    return;
  }
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!isPidAlive(pid)) {
      return;
    }
    await sleep(100);
  }
}

async function waitForProcessExit(
  proc: ChildProcessWithoutNullStreams,
  timeoutMs = 5000
): Promise<void> {
  if (proc.exitCode !== null || proc.signalCode !== null) {
    return;
  }
  await new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, timeoutMs);
    proc.once("close", () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

function publishIndexStatus(
  repoRoot: string,
  status: IndexStatusReport | undefined
): void {
  statuses.set(repoRoot, status);
  statusEmitter.fire({ repoRoot, status });
}

function readIndexStatus(statusPath: string): IndexStatusReport | undefined {
  try {
    if (!fs.existsSync(statusPath)) {
      return undefined;
    }
    const raw = fs.readFileSync(statusPath, "utf8").trim();
    if (!raw) {
      return undefined;
    }
    return normalizeIndexStatus(JSON.parse(raw));
  } catch {
    return undefined;
  }
}

function refreshIndexStatus(repoRoot: string): void {
  const handle = processes.get(repoRoot);
  if (!handle) {
    return;
  }
  const status = readIndexStatus(handle.statusPath);
  if (status) {
    handle.pid = status.pid ?? handle.pid;
    publishIndexStatus(repoRoot, status);
    if (!status.active && !handle.proc) {
      handle.statusWatcher?.close();
      processes.delete(repoRoot);
    }
    return;
  }
  if (!handle.proc && !isPidAlive(handle.pid)) {
    handle.statusWatcher?.close();
    processes.delete(repoRoot);
    const previous = statuses.get(repoRoot);
    publishIndexStatus(
      repoRoot,
      makeEphemeralStatus({
        active: false,
        phase: "stopped",
        message: "Live indexing stopped.",
        progressPercent: 0,
        recentFiles: previous?.recentFiles ?? [],
      })
    );
  }
}

function watchStatusFile(repoRoot: string, statusPath: string): fs.FSWatcher {
  fs.mkdirSync(path.dirname(statusPath), { recursive: true });
  const statusFileName = path.basename(statusPath);
  return fs.watch(path.dirname(statusPath), (_eventType, filename) => {
    const observed = typeof filename === "string" ? filename : "";
    if (!observed || !observed.startsWith(statusFileName)) {
      return;
    }
    setTimeout(() => refreshIndexStatus(repoRoot), 50);
  });
}

function makeEphemeralStatus(
  overrides: Partial<IndexStatusReport>
): IndexStatusReport {
  return {
    active: true,
    phase: "starting",
    message: "Starting live indexing…",
    progressPercent: 5,
    pendingCount: 0,
    pendingFiles: [],
    inflightCount: 0,
    inflightFiles: [],
    recentFiles: [],
    updatedAt: Date.now() / 1000,
    ...overrides,
  };
}

export function isLiveIndexing(repoRoot: string): boolean {
  const handle = processes.get(repoRoot);
  if (isHandleRunning(handle)) {
    return true;
  }
  const status = readIndexStatus(defaultStatusPath(repoRoot));
  return isStatusProcessAlive(status);
}

export function getLiveIndexStatus(
  repoRoot: string
): IndexStatusReport | undefined {
  return statuses.get(repoRoot);
}

function attachToExistingIndexd(
  repoRoot: string,
  statusPath: string,
  status: IndexStatusReport
): void {
  const existing = processes.get(repoRoot);
  existing?.statusWatcher?.close();
  const handle: LiveIndexingHandle = {
    statusPath,
    statusWatcher: watchStatusFile(repoRoot, statusPath),
    pid: status.pid,
    external: true,
  };
  processes.set(repoRoot, handle);
  publishIndexStatus(repoRoot, status);
}

export async function startLiveIndexing(
  repoRoot: string,
  dbPath: string,
  statusPath = defaultStatusPath(repoRoot),
  options?: StartLiveIndexingOptions
): Promise<void> {
  const forceFullRebuild = options?.forceFullRebuild === true;
  if (forceFullRebuild) {
    await stopLiveIndexing(repoRoot);
  } else if (isLiveIndexing(repoRoot)) {
    const existingStatus = readIndexStatus(statusPath);
    if (isStatusProcessAlive(existingStatus)) {
      attachToExistingIndexd(repoRoot, statusPath, existingStatus);
    }
    refreshIndexStatus(repoRoot);
    return;
  }
  if (!forceFullRebuild) {
    const existingStatus = readIndexStatus(statusPath);
    if (isStatusProcessAlive(existingStatus)) {
      attachToExistingIndexd(repoRoot, statusPath, existingStatus);
      return;
    }
  }
  const python = resolvePythonExecutable();
  const channel = getOutputChannel();
  const args = [
    "-m",
    INDEXD_MODULE,
    "--repo-root",
    repoRoot,
    "--db-path",
    dbPath,
  ];
  if (forceFullRebuild) {
    args.push("--full-rebuild");
  }
  channel.appendLine(`$ ${python} ${args.join(" ")}`);

  const proc = spawn(python, args, {
    cwd: repoRoot,
    env: {
      ...process.env,
      COGNIS_DB_PATH: dbPath,
      COGNIS_INDEXD_STATUS_PATH: statusPath,
    },
  });
  const handle: LiveIndexingHandle = {
    proc,
    statusPath,
    pid: proc.pid,
    external: false,
  };
  processes.set(repoRoot, handle);
  handle.statusWatcher = watchStatusFile(repoRoot, statusPath);
  publishIndexStatus(
    repoRoot,
    makeEphemeralStatus({
      active: true,
      phase: forceFullRebuild ? "cold_index" : "starting",
      message: forceFullRebuild
        ? "Starting managed full index rebuild…"
        : "Starting live indexing daemon…",
      progressPercent: forceFullRebuild ? 10 : 5,
    })
  );

  proc.stdout.on("data", (chunk: Buffer) => channel.append(chunk.toString()));
  proc.stderr.on("data", (chunk: Buffer) => channel.append(chunk.toString()));
  proc.on("close", (code) => {
    handle.statusWatcher?.close();
    processes.delete(repoRoot);
    channel.appendLine(`[indexd] exited ${code ?? "?"}`);
    const previous = statuses.get(repoRoot);
    publishIndexStatus(
      repoRoot,
      makeEphemeralStatus({
        active: false,
        phase: "stopped",
        message: "Live indexing stopped.",
        progressPercent: 0,
        recentFiles: previous?.recentFiles ?? [],
      })
    );
  });
  proc.on("error", (err) => {
    handle.statusWatcher?.close();
    processes.delete(repoRoot);
    channel.appendLine(`[indexd] error: ${err.message}`);
    const previous = statuses.get(repoRoot);
    publishIndexStatus(
      repoRoot,
      makeEphemeralStatus({
        active: false,
        phase: "error",
        message: "Live indexing failed to start.",
        progressPercent: 0,
        recentFiles: previous?.recentFiles ?? [],
        lastError: err.message,
      })
    );
  });
}

export async function stopLiveIndexing(repoRoot: string): Promise<void> {
  const handle = processes.get(repoRoot);
  if (!handle) {
    const status = readIndexStatus(defaultStatusPath(repoRoot));
    if (status?.pid && isPidAlive(status.pid)) {
      try {
        process.kill(status.pid);
      } catch {
        // Ignore: another host may have already stopped it.
      }
      await waitForPidExit(status.pid);
    }
    return;
  }
  handle.statusWatcher?.close();
  if (handle.proc) {
    handle.proc.kill();
    await waitForProcessExit(handle.proc);
  } else if (handle.pid && isPidAlive(handle.pid)) {
    try {
      process.kill(handle.pid);
    } catch {
      // Ignore: another host may have already stopped it.
    }
    await waitForPidExit(handle.pid);
  }
  processes.delete(repoRoot);
  const previous = statuses.get(repoRoot);
  publishIndexStatus(
    repoRoot,
    makeEphemeralStatus({
      active: false,
      phase: "stopped",
      message: "Live indexing stopped.",
      progressPercent: 0,
      recentFiles: previous?.recentFiles ?? [],
    })
  );
}

export function stopAllIndexing(): void {
  for (const [root, handle] of processes) {
    handle.statusWatcher?.close();
    processes.delete(root);
    if (!handle.proc) {
      continue;
    }
    handle.proc.kill();
    const previous = statuses.get(root);
    publishIndexStatus(
      root,
      makeEphemeralStatus({
        active: false,
        phase: "stopped",
        message: "Live indexing stopped.",
        progressPercent: 0,
        recentFiles: previous?.recentFiles ?? [],
      })
    );
  }
}
