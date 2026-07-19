import * as fs from "fs";
import {
  spawn,
  spawnSync,
  type ChildProcessWithoutNullStreams,
} from "child_process";
import * as path from "path";
import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { trace } from "./diagnostics";
import { resolveIndexdInvocation } from "./binary";
import {
  reconcileOrphanLease,
  removeLeaseForPid,
  verifyLeaseOwner,
  type OwnerVerification,
} from "./lease";
import { modelEnv } from "./model";
import type { IndexStatusReport } from "./types";

/**
 * Phases the panel knows how to render (see panel.ts deriveIndexingHeadline +
 * deriveIndexSectionView). This is a cross-process contract: cognis-indexd
 * chooses the phase string, the panel switches on it. If the daemon introduces
 * a new phase the panel has no case for, the UI silently falls through to a
 * generic message — exactly the kind of drift that passes e2e but degrades in
 * production. We detect an unknown phase here, at the boundary where the value
 * crosses in, and record it once so it is traceable instead of invisible.
 */
const KNOWN_PHASES: ReadonlySet<string> = new Set([
  "starting",
  "cold_index",
  "rebuild",
  "embedding",
  "sweep",
  "branch_change",
  "incremental",
  "watching",
  "idle",
  "stopped",
  "error",
]);

/** Phases already reported as unknown, so we trace each novel value only once. */
const reportedUnknownPhases = new Set<string>();

interface LiveIndexingHandle {
  proc?: ChildProcessWithoutNullStreams;
  statusPath: string;
  statusWatcher?: fs.FSWatcher;
  pid?: number;
  external?: boolean;
  /**
   * Number of local clients currently holding this daemon open. The daemon is
   * only stopped when the last client releases (reference-aware graceful
   * shutdown — Requirements 2.7). Spawned / attached handles start at 1.
   */
  refCount: number;
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
  const phase =
    typeof payload.phase === "string" ? payload.phase : "starting";
  if (!KNOWN_PHASES.has(phase) && !reportedUnknownPhases.has(phase)) {
    reportedUnknownPhases.add(phase);
    trace.warn("contract", "indexd reported an unknown status phase", {
      phase,
      known: [...KNOWN_PHASES],
    });
  }
  return {
    pid: typeof payload.pid === "number" ? payload.pid : undefined,
    active: Boolean(payload.active),
    phase,
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

/**
 * Terminate a process by pid for handles that have no live `proc` reference
 * (pid-only handles attached via {@link attachToExistingIndexd}). Idempotent:
 * a falsy/invalid pid or an already-dead process is treated as success (R10.6).
 *
 * On Windows the indexd children are NOT spawned detached, so killing just the
 * pid would orphan them; we tree-kill via `taskkill /T` to reap the whole
 * process tree (R10.3). On other platforms a plain `process.kill` suffices.
 *
 * Failures are swallowed and logged (with the offending pid) to the output
 * channel so one stubborn process never blocks cleanup of the rest (R10.5).
 *
 * When a `repoRoot` is supplied, the kill is gated on the repository-scoped
 * lease (Task 6.2): a `"mismatch"` verification means the pid was reused by an
 * unrelated process and we refuse to terminate it (preservation 3.9). A
 * `"match"` or `"unknown"` proceeds (unknown is the legacy best-effort path
 * when no lease exists yet — still never kills a dead pid).
 */
function killByPid(
  pid: number | undefined,
  repoRoot?: string
): void {
  if (!pid || pid <= 0 || !isPidAlive(pid)) {
    return;
  }
  if (repoRoot) {
    const verdict: OwnerVerification = verifyLeaseOwner(
      repoRoot,
      "indexd",
      pid
    );
    if (verdict === "mismatch") {
      getOutputChannel().appendLine(
        `[indexd] refusing to kill pid ${pid}: lease process-start identity ` +
          `does not match (pid reuse); safe non-destruction (3.9)`
      );
      return;
    }
  }
  try {
    if (process.platform === "win32") {
      // tree-kill: indexd's children are not detached, so a pid-only kill
      // would leave them orphaned.
      spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"]);
    } else {
      process.kill(pid);
    }
  } catch (err) {
    getOutputChannel().appendLine(
      `[indexd] failed to terminate pid ${pid}: ${
        err instanceof Error ? err.message : String(err)
      }`
    );
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

/**
 * Record (or refresh) a repository-scoped `indexd.lease` for a live orphan so
 * a reloaded extension can later reclaim it safely. Best-effort — never
 * throws. The Rust daemon also writes this lease on start (Task 6.1); this
 * path covers the case where the extension observes a status-file pid after a
 * reload and the daemon-written lease is missing or expired.
 */
function ensureIndexdLease(repoRoot: string, pid: number | undefined): void {
  reconcileOrphanLease(repoRoot, "indexd", pid);
}

export function isLiveIndexing(repoRoot: string): boolean {
  const handle = processes.get(repoRoot);
  if (isHandleRunning(handle)) {
    return true;
  }
  const status = readIndexStatus(defaultStatusPath(repoRoot));
  if (!isStatusProcessAlive(status)) {
    return false;
  }
  // A live status-file pid after a reload means a live orphan. Reconcile a
  // repository-scoped lease so the owner (pid + process-start identity +
  // nonce) is recorded and can later be reclaimed safely (Requirements 2.7,
  // 2.13; exploration test in indexd.test.ts).
  ensureIndexdLease(repoRoot, status.pid);
  return true;
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
  if (existing && isHandleRunning(existing)) {
    // Another local client is already attached — bump the refcount so the
    // last release (not this one) stops the daemon.
    existing.refCount += 1;
    existing.pid = status.pid ?? existing.pid;
    ensureIndexdLease(repoRoot, existing.pid);
    publishIndexStatus(repoRoot, status);
    return;
  }
  existing?.statusWatcher?.close();
  const handle: LiveIndexingHandle = {
    statusPath,
    statusWatcher: watchStatusFile(repoRoot, statusPath),
    pid: status.pid,
    external: true,
    refCount: 1,
  };
  processes.set(repoRoot, handle);
  // Record / refresh the lease so a future reload still has the owner identity.
  ensureIndexdLease(repoRoot, status.pid);
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
    await stopLiveIndexing(repoRoot, { force: true });
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
  const flags = [
    "--repo-root",
    repoRoot,
    "--db-path",
    dbPath,
  ];
  if (forceFullRebuild) {
    flags.push("--full-rebuild");
  }
  const { command, args } = resolveIndexdInvocation(flags);
  const channel = getOutputChannel();
  channel.appendLine(`$ ${command} ${args.join(" ")}`);

  const proc = spawn(command, args, {
    cwd: repoRoot,
    env: {
      ...process.env,
      ...modelEnv(),
      COGNIS_DB_PATH: dbPath,
      COGNIS_INDEXD_STATUS_PATH: statusPath,
    },
  });
  const handle: LiveIndexingHandle = {
    proc,
    statusPath,
    pid: proc.pid,
    external: false,
    refCount: 1,
  };
  processes.set(repoRoot, handle);
  handle.statusWatcher = watchStatusFile(repoRoot, statusPath);
  // The Rust daemon writes the authoritative lease on start; also reconcile
  // from the extension so a lease is present even if the daemon's write is
  // delayed or fails (the exploration test asserts the lease exists once the
  // extension has observed a live owner).
  ensureIndexdLease(repoRoot, proc.pid);
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
    // Clean our lease only when the pid still matches (do not clobber a
    // newer owner's record — safe non-destruction, 3.9).
    removeLeaseForPid(repoRoot, "indexd", handle.pid);
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
    removeLeaseForPid(repoRoot, "indexd", handle.pid);
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

export interface StopLiveIndexingOptions {
  /**
   * When true, ignore the reference count and stop the daemon immediately
   * (used by force-rebuild / remove-from-workspace / deactivate). Default
   * false implements reference-aware graceful shutdown: only the last client
   * release actually kills the process.
   */
  force?: boolean;
}

/**
 * Release one local client hold on the live-indexing daemon for `repoRoot`.
 *
 * Reference-aware: the process is only stopped when the last client releases
 * (or when `force: true`). Cleanup is lease-verified — a pid whose process-
 * start identity no longer matches the recorded lease is never terminated
 * (preservation 3.9). Idempotent on a missing handle / already-dead pid.
 */
export async function stopLiveIndexing(
  repoRoot: string,
  options?: StopLiveIndexingOptions
): Promise<void> {
  const force = options?.force === true;
  const handle = processes.get(repoRoot);
  if (!handle) {
    // No in-memory handle (post-reload). Only kill a status-file pid when the
    // lease confirms identity (or there is no lease at all, in which case we
    // still refuse on a live mismatch path — but with no lease the verification
    // returns "unknown" and we proceed with the legacy best-effort kill of a
    // *live* status pid only when force is set, never on a mere release).
    if (!force) {
      return;
    }
    const status = readIndexStatus(defaultStatusPath(repoRoot));
    if (status?.pid && isPidAlive(status.pid)) {
      killByPid(status.pid, repoRoot);
      await waitForPidExit(status.pid);
      removeLeaseForPid(repoRoot, "indexd", status.pid);
    }
    return;
  }

  if (!force) {
    handle.refCount = Math.max(0, handle.refCount - 1);
    if (handle.refCount > 0) {
      // Other local clients still hold the daemon open — leave it running.
      return;
    }
  }

  handle.statusWatcher?.close();
  const pid = handle.pid ?? handle.proc?.pid;
  if (handle.proc) {
    handle.proc.kill();
    await waitForProcessExit(handle.proc);
  } else if (pid && isPidAlive(pid)) {
    killByPid(pid, repoRoot);
    await waitForPidExit(pid);
  }
  processes.delete(repoRoot);
  removeLeaseForPid(repoRoot, "indexd", pid);
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

/**
 * Force-stop every live-indexing daemon this extension owns (used on
 * deactivate / prepare-uninstall). Ignores reference counts so a host teardown
 * always reclaims Cognis-owned state; still lease-verified so a PID-reused
 * unrelated process is never terminated (preservation 3.9).
 */
export function stopAllIndexing(): void {
  for (const [root, handle] of processes) {
    handle.statusWatcher?.close();
    processes.delete(root);
    const pid = handle.pid ?? handle.proc?.pid;
    if (handle.proc) {
      // Graceful stop first (R10.4); waitForProcessExit escalation is handled
      // by the per-repo stopLiveIndexing path.
      handle.proc.kill();
    } else {
      // pid-only handle (attached via attachToExistingIndexd): don't skip it —
      // tree-kill by pid so no orphaned daemon survives deactivate (R10.1,
      // R10.3). Lease-verified: refuse on process-start mismatch (3.9).
      killByPid(handle.pid, root);
    }
    removeLeaseForPid(root, "indexd", pid);
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
