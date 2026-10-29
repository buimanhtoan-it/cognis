/**
 * `Deactivation_Manager` — ordered turn-off when Toggle_State becomes Off.
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 4.1–4.5, 4.7–4.9,
 * 21.8).
 *
 * Fixed order:
 *
 *   stop watcher → stop every Owned_Daemon → remove Managed_MCP_Entry
 *
 * The Workspace_Database is never deleted or rewritten. Stop is a signal, not
 * a kill: an in-flight Publish_Batch may commit or abort atomically. A daemon
 * that is still alive after {@link DAEMON_EXIT_BUDGET_MS} is terminated and the
 * termination is written to the diagnostics log. Only Owned_Daemons — processes
 * this extension instance started for this workspace and holds the lease for —
 * are ever signaled or terminated.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { spawnSync } from "node:child_process";

import { trace } from "./diagnostics";
import {
  getStartedIndexdPid,
  releaseLiveIndexingAfterDeactivation,
} from "./indexd";
import { verifyLeaseOwner, type LeaseRole } from "./lease";
import { disableMcpForWorkspace } from "./mcpConfig";
import { getStartedMcpdPid, stopMcpServer } from "./mcpServer";
import { setLiveIndexing, setMcpEnabled } from "./state";
import type { TransitionPhase } from "./status";

/** The Deactivation_Manager's canonical turn-off order (Requirement 4.1–4.3). */
export const TURN_OFF_ORDER: readonly TransitionPhase[] = [
  "stop_watcher",
  "stop_daemons",
  "remove_mcp_entry",
];

/** Wait this long after a stop signal before terminating (Requirement 4.8). */
export const DAEMON_EXIT_BUDGET_MS = 10_000;

export type OwnedDaemonRole = Extract<LeaseRole, "indexd" | "mcpd" | "owner">;

export interface OwnedDaemon {
  pid: number;
  role: OwnedDaemonRole;
}

export interface DeactivationStepResult {
  phase: TransitionPhase;
  ok: boolean;
  skipped?: boolean;
  detail?: string;
}

/**
 * Injected turn-off steps so order, signal-then-terminate, and "don't touch
 * foreign processes / the database" are unit-tested without live children.
 */
export interface DeactivationHost {
  signal: AbortSignal;
  onPhase(phase: TransitionPhase | undefined): void;
  stopWatcher(): Promise<void>;
  listOwnedDaemons(): OwnedDaemon[];
  signalStop(daemon: OwnedDaemon): void;
  isAlive(daemon: OwnedDaemon): boolean;
  terminate(daemon: OwnedDaemon): void;
  logTermination(daemon: OwnedDaemon): void;
  removeManagedMcpEntries(): Promise<void>;
  sleep(ms: number): Promise<void>;
}

function abortError(): Error {
  const error = new Error("Deactivation aborted");
  error.name = "AbortError";
  return error;
}

function throwIfAborted(signal: AbortSignal): void {
  if (signal.aborted) {
    throw abortError();
  }
}

async function waitForExit(
  host: DeactivationHost,
  daemon: OwnedDaemon
): Promise<boolean> {
  const steps = 10;
  const slice = Math.max(1, Math.floor(DAEMON_EXIT_BUDGET_MS / steps));
  for (let i = 0; i < steps; i += 1) {
    throwIfAborted(host.signal);
    if (!host.isAlive(daemon)) {
      return true;
    }
    await host.sleep(slice);
  }
  return !host.isAlive(daemon);
}

async function stopOwnedDaemons(host: DeactivationHost): Promise<void> {
  for (const daemon of host.listOwnedDaemons()) {
    throwIfAborted(host.signal);
    host.signalStop(daemon);
    const exited = await waitForExit(host, daemon);
    if (!exited && host.isAlive(daemon)) {
      host.terminate(daemon);
      host.logTermination(daemon);
    }
  }
}

/**
 * Run the turn-off sequence. Never deletes or rewrites the workspace database.
 */
export async function runTurnOff(
  host: DeactivationHost
): Promise<DeactivationStepResult[]> {
  const results: DeactivationStepResult[] = [];

  const step = async (
    phase: Extract<
      TransitionPhase,
      "stop_watcher" | "stop_daemons" | "remove_mcp_entry"
    >,
    work: () => Promise<void>
  ): Promise<void> => {
    throwIfAborted(host.signal);
    host.onPhase(phase);
    try {
      await work();
      throwIfAborted(host.signal);
      results.push({ phase, ok: true });
    } catch (error) {
      if (host.signal.aborted || (error instanceof Error && error.name === "AbortError")) {
        throw error instanceof Error ? error : abortError();
      }
      const detail = error instanceof Error ? error.message : String(error);
      results.push({ phase, ok: false, detail });
    }
  };

  await step("stop_watcher", () => host.stopWatcher());
  await step("stop_daemons", () => stopOwnedDaemons(host));
  await step("remove_mcp_entry", () => host.removeManagedMcpEntries());
  host.onPhase(undefined);
  return results;
}

function defaultDbPath(repoRoot: string): string {
  return path.join(repoRoot, ".cognis", "uckg.db");
}

function pidAlive(pid: number): boolean {
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

function signalPid(pid: number): void {
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    // Already gone.
  }
}

function terminatePid(pid: number, repoRoot: string, role: OwnedDaemonRole): void {
  if (!pidAlive(pid)) {
    return;
  }
  if (verifyLeaseOwner(repoRoot, role, pid) === "mismatch") {
    return;
  }
  try {
    if (process.platform === "win32") {
      spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"]);
    } else {
      process.kill(pid, "SIGKILL");
    }
  } catch {
    // Best-effort terminate.
  }
}

export function createProductionDeactivationHost(
  repoRoot: string,
  signal: AbortSignal,
  onPhase: (phase: TransitionPhase | undefined) => void
): DeactivationHost {
  return {
    signal,
    onPhase,
    async stopWatcher() {
      const pid = getStartedIndexdPid(repoRoot);
      if (pid !== undefined) {
        signalPid(pid);
      }
    },
    listOwnedDaemons() {
      const daemons: OwnedDaemon[] = [];
      const indexd = getStartedIndexdPid(repoRoot);
      if (indexd !== undefined) {
        daemons.push({ pid: indexd, role: "indexd" });
      }
      const mcpd = getStartedMcpdPid(repoRoot);
      if (mcpd !== undefined) {
        daemons.push({ pid: mcpd, role: "mcpd" });
      }
      return daemons;
    },
    signalStop(daemon) {
      signalPid(daemon.pid);
    },
    isAlive(daemon) {
      return pidAlive(daemon.pid);
    },
    terminate(daemon) {
      terminatePid(daemon.pid, repoRoot, daemon.role);
    },
    logTermination(daemon) {
      trace.warn("deactivation", "terminated owned daemon after stop budget", {
        pid: daemon.pid,
        role: daemon.role,
        budgetMs: DAEMON_EXIT_BUDGET_MS,
      });
    },
    async removeManagedMcpEntries() {
      await disableMcpForWorkspace(repoRoot);
      setMcpEnabled(repoRoot, false);
    },
    sleep(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    },
  };
}

export interface DeactivationManagerHooks {
  onPhase?(repoRoot: string, phase: TransitionPhase | undefined): void;
}

/** Production Deactivation_Manager used by Toggle_Control. */
export class DeactivationManager {
  currentPhase: TransitionPhase | undefined;
  private readonly hooks: DeactivationManagerHooks;

  constructor(hooks: DeactivationManagerHooks = {}) {
    this.hooks = hooks;
  }

  async turnOff(repoRoot: string, signal: AbortSignal): Promise<DeactivationStepResult[]> {
    const host = createProductionDeactivationHost(repoRoot, signal, (phase) => {
      this.currentPhase = phase;
      this.hooks.onPhase?.(repoRoot, phase);
    });
    try {
      const results = await runTurnOff(host);
      await releaseLiveIndexingAfterDeactivation(repoRoot);
      await stopMcpServer(repoRoot, { force: true });
      setLiveIndexing(repoRoot, false);
      const dbPath = defaultDbPath(repoRoot);
      trace.info("deactivation", "turn-off finished", {
        repoRoot,
        results: results.map((step) => step.phase),
        databasePresent: fs.existsSync(dbPath),
      });
      return results;
    } finally {
      this.currentPhase = undefined;
      this.hooks.onPhase?.(repoRoot, undefined);
    }
  }
}
