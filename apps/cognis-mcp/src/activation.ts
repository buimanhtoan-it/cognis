/**
 * `Activation_Manager` — ordered turn-on when Toggle_State becomes On.
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 3.1–3.9, 3.11,
 * 3.12, 21.3, 21.4).
 *
 * Fixed order (Requirement 3.9):
 *
 *   verify Engine_Binary → verify Model_Assets → open/create Workspace_Database
 *   → reconcile and index → start watcher → write Managed_MCP_Entry
 *
 * Downloads are checksum-gated (21.4): a mismatched payload is discarded and
 * never executed. A failed binary or model download degrades to the furthest
 * reachable state rather than aborting the rest of the sequence (3.7, 3.8).
 *
 * The watcher/entry reconciler keeps `watcher running ⇔ Managed_MCP_Entry`
 * outside a transition, restoring a removed entry within 5 s while the watcher
 * runs and removing the entry when the watcher stops (3.11, 3.12).
 */

import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import {
  installManagedBinary,
  isManagedBinaryInstalled,
  verifyDownloadedBinary,
} from "./binary";
import { trace } from "./diagnostics";
import { isLiveIndexing, startLiveIndexing } from "./indexd";
import {
  disableMcpForWorkspace,
  enableMcpForWorkspace,
  isCognisMcpConfiguredForRepo,
} from "./mcpConfig";
import { installManagedModel, isModelInstalled } from "./model";
import { setLiveIndexing, setMcpEnabled } from "./state";
import type { TransitionPhase } from "./status";
import { ensureWorkspaceConfigFresh } from "./workspace";
import { runCli } from "./cli";

/** The Activation_Manager's canonical turn-on order (Requirement 3.9). */
export const TURN_ON_ORDER: readonly TransitionPhase[] = [
  "verify_engine_binary",
  "verify_model_assets",
  "open_database",
  "reconcile_and_index",
  "start_watcher",
  "write_mcp_entry",
];

/** Deadline for restoring a removed Managed_MCP_Entry while the watcher runs. */
export const ENTRY_RESTORE_BUDGET_MS = 5000;

export interface ActivationStepResult {
  phase: TransitionPhase;
  ok: boolean;
  skipped?: boolean;
  detail?: string;
}

/**
 * Injected turn-on steps so the order, degrade, and skip rules are unit-tested
 * without a live download or engine binary.
 */
export interface ActivationHost {
  signal: AbortSignal;
  onPhase(phase: TransitionPhase | undefined): void;
  verifyEngineBinary(): Promise<void>;
  verifyModelAssets(): Promise<void>;
  databaseExists(): boolean;
  openOrCreateDatabase(mode: "initial" | "existing"): Promise<void>;
  reconcileAndIndex(mode: "initial" | "existing"): Promise<void>;
  startWatcher(): Promise<void>;
  isWatcherRunning(): boolean;
  writeManagedMcpEntry(): Promise<void>;
}

function abortError(): Error {
  const error = new Error("Activation aborted");
  error.name = "AbortError";
  return error;
}

function throwIfAborted(signal: AbortSignal): void {
  if (signal.aborted) {
    throw abortError();
  }
}

/**
 * True only when the downloaded bytes match the published `.sha256` sidecar.
 * A mismatch is never usable (Requirement 21.4).
 */
export function downloadedPayloadIsUsable(
  payload: Buffer | Uint8Array,
  sidecarText: string
): boolean {
  return verifyDownloadedBinary(payload, sidecarText).ok;
}

function remainingPhases(
  after: TransitionPhase | undefined
): TransitionPhase[] {
  if (after === undefined) {
    return [...TURN_ON_ORDER];
  }
  const index = TURN_ON_ORDER.indexOf(after);
  return TURN_ON_ORDER.slice(index + 1);
}

/**
 * Run the turn-on sequence. Engine-process steps are skipped when the binary
 * cannot be verified. A model-asset failure is non-fatal (lexical still runs).
 */
export async function runTurnOn(
  host: ActivationHost
): Promise<ActivationStepResult[]> {
  const results: ActivationStepResult[] = [];

  const skipRest = (after: TransitionPhase, detail: string): void => {
    for (const phase of remainingPhases(after)) {
      results.push({ phase, ok: false, skipped: true, detail });
    }
    host.onPhase(undefined);
  };

  const step = async (
    phase: Extract<
      TransitionPhase,
      | "verify_engine_binary"
      | "verify_model_assets"
      | "open_database"
      | "reconcile_and_index"
      | "start_watcher"
      | "write_mcp_entry"
    >,
    work: () => Promise<void>,
    options: { required: boolean }
  ): Promise<boolean> => {
    throwIfAborted(host.signal);
    host.onPhase(phase);
    try {
      await work();
      throwIfAborted(host.signal);
      results.push({ phase, ok: true });
      return true;
    } catch (error) {
      if (host.signal.aborted || (error instanceof Error && error.name === "AbortError")) {
        throw error instanceof Error ? error : abortError();
      }
      const detail = error instanceof Error ? error.message : String(error);
      results.push({ phase, ok: false, detail });
      if (options.required) {
        skipRest(phase, detail);
        return false;
      }
      return true;
    }
  };

  const binaryOk = await step(
    "verify_engine_binary",
    () => host.verifyEngineBinary(),
    { required: true }
  );
  if (!binaryOk) {
    return results;
  }

  await step("verify_model_assets", () => host.verifyModelAssets(), {
    required: false,
  });

  const dbExisted = host.databaseExists();
  const mode = dbExisted ? "existing" : "initial";

  if (
    !(await step("open_database", () => host.openOrCreateDatabase(mode), {
      required: true,
    }))
  ) {
    return results;
  }

  if (
    !(await step("reconcile_and_index", () => host.reconcileAndIndex(mode), {
      required: true,
    }))
  ) {
    return results;
  }

  if (
    !(await step("start_watcher", () => host.startWatcher(), { required: true }))
  ) {
    return results;
  }

  if (!host.isWatcherRunning()) {
    skipRest("start_watcher", "watcher is not running");
    return results;
  }

  await step("write_mcp_entry", () => host.writeManagedMcpEntry(), {
    required: false,
  });
  host.onPhase(undefined);
  return results;
}

export interface WatcherEntryReconcilerDeps {
  isWatcherRunning(repoRoot: string): boolean;
  isEntryPresent(repoRoot: string): boolean;
  restoreEntry(repoRoot: string): Promise<void>;
  removeEntry(repoRoot: string): Promise<void>;
  inTransition?(repoRoot: string): boolean;
  now?: () => number;
  restoreBudgetMs?: number;
}

/**
 * Keeps watcher-running ⇔ Managed_MCP_Entry outside a transition.
 * Restores a removed entry within {@link ENTRY_RESTORE_BUDGET_MS} while the
 * watcher runs; removes the entry when the watcher stops.
 */
export class WatcherEntryReconciler {
  private readonly missingSince = new Map<string, number>();
  private readonly deps: WatcherEntryReconcilerDeps;
  private readonly budgetMs: number;

  constructor(deps: WatcherEntryReconcilerDeps) {
    this.deps = deps;
    // Default 0: restore on the next observation (poll ≤ 1 s), which is inside
    // the 5 s SLA. Tests inject `restoreBudgetMs` to prove the ceiling.
    this.budgetMs = deps.restoreBudgetMs ?? 0;
  }

  async tick(repoRoot: string): Promise<void> {
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

  startPolling(
    repoRoot: string,
    intervalMs = 1000
  ): { dispose(): void } {
    const id = setInterval(() => {
      void this.tick(repoRoot).catch(() => {});
    }, intervalMs);
    void this.tick(repoRoot).catch(() => {});
    return {
      dispose(): void {
        clearInterval(id);
      },
    };
  }
}

function silentProgress(): vscode.Progress<{ message?: string }> {
  return { report(): void {} };
}

function tokenFromSignal(signal: AbortSignal): vscode.CancellationToken {
  return {
    get isCancellationRequested() {
      return signal.aborted;
    },
    onCancellationRequested: (listener: () => void) => {
      const wrapped = () => listener();
      signal.addEventListener("abort", wrapped);
      return {
        dispose: () => signal.removeEventListener("abort", wrapped),
      };
    },
  } as vscode.CancellationToken;
}

function defaultDbPath(repoRoot: string): string {
  return path.join(repoRoot, ".cognis", "uckg.db");
}

export function createProductionActivationHost(
  repoRoot: string,
  signal: AbortSignal,
  onPhase: (phase: TransitionPhase | undefined) => void
): ActivationHost {
  return {
    signal,
    onPhase,
    async verifyEngineBinary() {
      if (isManagedBinaryInstalled()) {
        return;
      }
      await installManagedBinary(silentProgress(), tokenFromSignal(signal));
      if (!isManagedBinaryInstalled()) {
        throw new Error("Engine binary was not installed");
      }
    },
    async verifyModelAssets() {
      if (isModelInstalled()) {
        return;
      }
      await installManagedModel(silentProgress(), tokenFromSignal(signal));
      if (!isModelInstalled()) {
        throw new Error("Model assets were not installed");
      }
    },
    databaseExists() {
      return fs.existsSync(defaultDbPath(repoRoot));
    },
    async openOrCreateDatabase(_mode) {
      await ensureWorkspaceConfigFresh(repoRoot);
    },
    async reconcileAndIndex(mode) {
      const dbPath = defaultDbPath(repoRoot);
      const args = mode === "initial" ? ["index", "--full", "."] : ["index", "."];
      const result = await runCli(repoRoot, args, {
        env: { COGNIS_DB_PATH: dbPath },
        label: args.join(" "),
      });
      if (result.exitCode !== 0) {
        throw new Error(
          `cognis index failed (${result.exitCode}): ${result.stderr || result.stdout}`
        );
      }
    },
    async startWatcher() {
      const dbPath = defaultDbPath(repoRoot);
      await startLiveIndexing(repoRoot, dbPath);
      setLiveIndexing(repoRoot, true);
    },
    isWatcherRunning() {
      return isLiveIndexing(repoRoot);
    },
    async writeManagedMcpEntry() {
      await enableMcpForWorkspace(repoRoot);
      setMcpEnabled(repoRoot, true);
    },
  };
}

export interface ActivationManagerHooks {
  onPhase?(repoRoot: string, phase: TransitionPhase | undefined): void;
}

/** Production Activation_Manager used by Toggle_Control. */
export class ActivationManager {
  currentPhase: TransitionPhase | undefined;
  private readonly hooks: ActivationManagerHooks;
  private readonly transitioning = new Set<string>();
  private readonly reconciler: WatcherEntryReconciler;
  private poll: { dispose(): void } | undefined;

  constructor(hooks: ActivationManagerHooks = {}) {
    this.hooks = hooks;
    this.reconciler = new WatcherEntryReconciler({
      isWatcherRunning: (root) => isLiveIndexing(root),
      isEntryPresent: (root) => isCognisMcpConfiguredForRepo(root),
      restoreEntry: async (root) => {
        await enableMcpForWorkspace(root);
        setMcpEnabled(root, true);
      },
      removeEntry: async (root) => {
        await disableMcpForWorkspace(root);
        setMcpEnabled(root, false);
      },
      inTransition: (root) => this.transitioning.has(root),
    });
  }

  async turnOn(repoRoot: string, signal: AbortSignal): Promise<ActivationStepResult[]> {
    this.transitioning.add(repoRoot);
    const host = createProductionActivationHost(repoRoot, signal, (phase) => {
      this.currentPhase = phase;
      this.hooks.onPhase?.(repoRoot, phase);
    });
    try {
      const results = await runTurnOn(host);
      trace.info("activation", "turn-on finished", {
        repoRoot,
        results: results.map((step) => step.phase),
      });
      return results;
    } finally {
      this.currentPhase = undefined;
      this.transitioning.delete(repoRoot);
      this.hooks.onPhase?.(repoRoot, undefined);
    }
  }

  /** Mark a workspace as inside a transition so the reconciler stays idle. */
  beginTransition(repoRoot: string): void {
    this.transitioning.add(repoRoot);
  }

  endTransition(repoRoot: string): void {
    this.transitioning.delete(repoRoot);
  }

  /** Start the watcher ⇔ entry reconciler for a workspace (poll ≤ 1 s). */
  watchCoupling(repoRoot: string): { dispose(): void } {
    this.poll?.dispose();
    this.poll = this.reconciler.startPolling(repoRoot, 1000);
    return this.poll;
  }

  dispose(): void {
    this.poll?.dispose();
    this.poll = undefined;
  }
}
