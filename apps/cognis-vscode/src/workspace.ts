import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { getOutputChannel, runCli, runCliJson } from "./cli";
import {
  CognisGuidanceError,
  healthDegradedGuidance,
  mcpReloadRequiredGuidance,
  noWorkspaceGuidance,
} from "./guidance";
import { fetchHealth, formatHealthSummary } from "./health";
import {
  getLiveIndexStatus,
  isLiveIndexing,
  startLiveIndexing,
  stopLiveIndexing,
} from "./indexd";
import {
  hasExpectedMcpConfigForRepo,
  disableMcpForWorkspace,
  enableMcpForWorkspace,
  isCognisMcpConfiguredForRepo,
  removeAllCognisMcpEntries,
  showMcpConfigPreview,
} from "./mcpConfig";
import { verifyPythonEnvironment } from "./python";
import { fetchPrerequisites } from "./prerequisites";
import type { PanelContext } from "./panel";
import {
  deriveStatus,
  getState,
  loadPersistedState,
  setAutoManaged,
  setIndexStatus,
  setLastHealth,
  setLiveIndexing,
  setMcpEnabled,
} from "./state";
import { buildRepairPlan } from "./repairPlan";
import type {
  BootstrapPayload,
  HealthReport,
  RepairPlan,
  SetupResult,
  WorkspacePaths,
  WorkspaceStatus,
} from "./types";

export function getWorkspaceFolder(): vscode.WorkspaceFolder | undefined {
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    return undefined;
  }
  return folders[0];
}

export function requireWorkspaceFolder(): vscode.WorkspaceFolder {
  const folder = getWorkspaceFolder();
  if (!folder) {
    throw new CognisGuidanceError(noWorkspaceGuidance());
  }
  return folder;
}

function cognisConfigPath(repoRoot: string): string {
  return `${repoRoot}/.cognis/config.yaml`;
}

export async function diagnoseRepairPlan(repoRoot: string): Promise<RepairPlan> {
  const configExists = fs.existsSync(cognisConfigPath(repoRoot));
  const mcpConfigured = await hasExpectedMcpConfigForRepo(repoRoot);
  let health: HealthReport | undefined;
  try {
    health = await fetchHealth(repoRoot);
  } catch {
    health = undefined;
  }
  const state = getState(repoRoot);
  return buildRepairPlan({
    configExists,
    mcpConfigured,
    health,
    stateLiveIndexing: state.liveIndexing,
    liveIndexingRunning: isLiveIndexing(repoRoot),
  });
}

export function isWorkspaceConfigured(repoRoot: string): boolean {
  return fs.existsSync(path.join(repoRoot, ".cognis", "config.yaml"));
}

export function isWorkspaceAutoManaged(repoRoot: string): boolean {
  return getState(repoRoot).autoManaged;
}

function syncMcpStateFromDisk(repoRoot: string): void {
  const onDisk = isCognisMcpConfiguredForRepo(repoRoot);
  if (getState(repoRoot).mcpEnabled !== onDisk) {
    setMcpEnabled(repoRoot, onDisk);
  }
}

function syncLiveIndexingFromProcess(repoRoot: string): boolean {
  const running = isLiveIndexing(repoRoot);
  if (getState(repoRoot).liveIndexing !== running) {
    setLiveIndexing(repoRoot, running);
  }
  return running;
}

function syncIndexStatusFromDaemon(repoRoot: string) {
  const status = getLiveIndexStatus(repoRoot);
  setIndexStatus(repoRoot, status);
  return status;
}

export async function rehydrateWorkspaceState(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  const repoRoot = folder.uri.fsPath;
  loadPersistedState(repoRoot);

  const configured = isWorkspaceConfigured(repoRoot);
  syncMcpStateFromDisk(repoRoot);

  const state = getState(repoRoot);
  if (state.liveIndexing && configured) {
    try {
      const paths = await fetchPaths(repoRoot);
      await startLiveIndexing(repoRoot, paths.db_path, paths.indexd_status_path);
      setLiveIndexing(repoRoot, true);
    } catch {
      setLiveIndexing(repoRoot, false);
    }
  } else if (state.liveIndexing && !configured) {
    setLiveIndexing(repoRoot, false);
  }
  syncIndexStatusFromDaemon(repoRoot);
}

export async function fetchPaths(repoRoot: string): Promise<WorkspacePaths> {
  return runCliJson<WorkspacePaths>(repoRoot, ["paths"]);
}

export async function ensureWorkspaceConfigFresh(repoRoot: string): Promise<void> {
  const result = await runCli(repoRoot, ["init", "--quiet"], {
    label: "init --quiet",
  });
  if (result.exitCode !== 0) {
    throw new Error(
      `cognis CLI failed (${result.exitCode}): ${result.stderr || result.stdout}`
    );
  }
}

export async function bootstrapWorkspace(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken
): Promise<BootstrapPayload> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const skipEmbeddings = vscode.workspace
    .getConfiguration("cognis")
    .get<boolean>("skipEmbeddingsOnBootstrap", false);
  const args = ["bootstrap", "."];
  if (skipEmbeddings) {
    args.push("--skip-embeddings");
  }
  args.push("--json");

  progress.report({ message: "Running bootstrap (init → index → health)…" });
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  const paths = await fetchPaths(repoRoot);
  const result = await runCliJson<BootstrapPayload>(repoRoot, args, {
    COGNIS_DB_PATH: paths.db_path,
  });
  setLastHealth(repoRoot, result.overall);
  setAutoManaged(repoRoot, true);
  return result;
}

async function ensurePythonReady(
  repoRoot: string,
  progress: vscode.Progress<{ message?: string }>
): Promise<void> {
  progress.report({ message: "Checking Python environment…" });
  const pythonCheck = await verifyPythonEnvironment(repoRoot);
  if (!pythonCheck.ok) {
    throw new CognisGuidanceError(pythonCheck.guidance);
  }
}

/**
 * Block setup when a required prerequisite (parsers, local embeddings, MCP
 * server) is missing. This runs *before* any ``.cognis/`` files are created so
 * a fresh user is never left with a half-provisioned workspace that can't index
 * or serve. Optional items (vector, tokenizers) never block.
 */
async function ensurePrerequisitesReady(
  repoRoot: string,
  progress: vscode.Progress<{ message?: string }>
): Promise<void> {
  progress.report({ message: "Checking prerequisites…" });
  const report = await fetchPrerequisites(repoRoot);
  if (!report) {
    // doctor couldn't run — the Python check above already passed, so this is
    // an unexpected CLI failure. Let setup proceed; downstream steps surface a
    // concrete error if something is truly broken.
    return;
  }
  if (report.ready) {
    return;
  }
  const missing = report.items.filter(
    (item) => item.required && item.status === "missing"
  );
  const names = missing.map((item) => item.label).join(", ");
  throw new CognisGuidanceError({
    title: "Install prerequisites first",
    message:
      `Cognis can't set up this workspace until required components are installed: ${names}. ` +
      "Use the checklist in the Cognis panel to install them, then run Set Up for AI again.",
    severity: "warning",
    actions: [
      { label: "Open Cognis Panel", command: "cognis.openPanel" },
      { label: "Show Output", command: "cognis.showOutput" },
    ],
    technicalDetail: missing
      .map((item) => `${item.label}: ${item.detail} (install: ${item.install_target})`)
      .join("\n"),
  });
}

function repairCancelledError(): CognisGuidanceError {
  return new CognisGuidanceError({
    title: "Cancelled",
    message:
      "Repair was cancelled. Run Repair Setup again when you are ready to continue.",
    severity: "info",
    actions: [{ label: "Repair Setup", command: "cognis.repairSetup" }],
  });
}

async function tryFetchHealth(repoRoot: string): Promise<HealthReport | undefined> {
  try {
    return await fetchHealth(repoRoot);
  } catch {
    return undefined;
  }
}

function pendingHealth(message: string): HealthReport {
  return {
    runtime_version: "pending",
    overall: "warn",
    checks: {
      index: {
        status: "warn",
        message,
      },
    },
  };
}

async function bestEffortHealth(
  repoRoot: string,
  fallbackMessage: string
): Promise<HealthReport> {
  return (await tryFetchHealth(repoRoot)) ?? pendingHealth(fallbackMessage);
}

function shouldForceManagedRebuild(health: HealthReport | undefined): boolean {
  if (!health) {
    return true;
  }
  return ["config", "db", "index", "version"].some(
    (name) => health.checks[name]?.status === "fail"
  );
}

async function buildManagedSetupPayload(
  repoRoot: string,
  health: HealthReport,
  command: string
): Promise<BootstrapPayload> {
  const paths = await fetchPaths(repoRoot);
  return {
    command,
    runtime_version: health.runtime_version,
    repo_root: repoRoot,
    index_path: "",
    db_path: paths.db_path,
    skip_embeddings: false,
    paths,
    phases: [
      { name: "init", status: "ok" },
      { name: "indexd", status: "started" },
    ],
    health,
    overall: health.overall,
    exit_code: 0,
  };
}

export async function setupForAi(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken
): Promise<SetupResult> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;

  await ensurePythonReady(repoRoot, progress);
  await ensurePrerequisitesReady(repoRoot, progress);
  const wasConfigured = isWorkspaceConfigured(repoRoot);

  progress.report({ message: "Step 1/4: Preparing workspace config…" });
  await ensureWorkspaceConfigFresh(repoRoot);
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }
  const preflightHealth = await tryFetchHealth(repoRoot);
  const needsManagedRebuild =
    !wasConfigured || shouldForceManagedRebuild(preflightHealth);

  progress.report({ message: "Step 2/4: Enabling MCP…" });
  let mcpConfigPath: string | undefined;
  let mcpError: string | undefined;
  try {
    const { configPath } = await enableMcpForWorkspace(repoRoot);
    setMcpEnabled(repoRoot, true);
    mcpConfigPath = configPath;
  } catch (err) {
    mcpError = err instanceof Error ? err.message : String(err);
  }
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  progress.report({
    message: needsManagedRebuild
      ? "Step 3/4: Starting managed index build…"
      : "Step 3/4: Starting live indexing…",
  });
  let liveIndexingStarted = false;
  let liveIndexingError: string | undefined;
  try {
    await startLive({ forceFullRebuild: needsManagedRebuild });
    liveIndexingStarted = true;
  } catch (err) {
    liveIndexingError = err instanceof Error ? err.message : String(err);
  }
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  const indexingInBackground =
    needsManagedRebuild && liveIndexingStarted && !liveIndexingError;
  progress.report({
    message: indexingInBackground
      ? "Step 4/4: Verifying background indexing…"
      : "Step 4/4: Verifying setup…",
  });
  const health = await bestEffortHealth(
    repoRoot,
    indexingInBackground
      ? "Managed indexing is rebuilding the semantic index in the background."
      : "Cognis health information is not available yet."
  );
  setLastHealth(repoRoot, health.overall);
  setAutoManaged(repoRoot, true);
  const bootstrap = await buildManagedSetupPayload(repoRoot, health, "setup");

  return {
    bootstrap,
    mcpConfigPath,
    mcpError,
    liveIndexingStarted,
    liveIndexingError,
    health,
    indexingInBackground,
  };
}

export async function repairSetup(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken
): Promise<SetupResult> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;

  await ensurePythonReady(repoRoot, progress);
  progress.report({ message: "Repair: refreshing workspace config…" });
  await ensureWorkspaceConfigFresh(repoRoot);

  const plan = await diagnoseRepairPlan(repoRoot);
  const needsManagedRebuild =
    plan.needsBootstrap ||
    plan.needsReindex ||
    shouldForceManagedRebuild(plan.health);

  if (token.isCancellationRequested) {
    throw repairCancelledError();
  }

  progress.report({ message: "Repair: checking MCP configuration…" });
  let mcpConfigPath: string | undefined;
  let mcpError: string | undefined;
  try {
    const { configPath } = await enableMcpForWorkspace(repoRoot);
    if (plan.needsMcp || !getState(repoRoot).mcpEnabled) {
      setMcpEnabled(repoRoot, true);
    }
    mcpConfigPath = configPath;
  } catch (err) {
    mcpError = err instanceof Error ? err.message : String(err);
  }
  if (token.isCancellationRequested) {
    throw repairCancelledError();
  }

  progress.report({
    message: needsManagedRebuild
      ? "Repair: restarting managed indexing…"
      : "Repair: checking live indexing…",
  });
  let liveIndexingStarted = isLiveIndexing(repoRoot);
  let liveIndexingError: string | undefined;
  if (needsManagedRebuild || !liveIndexingStarted) {
    try {
      await startLive({ forceFullRebuild: needsManagedRebuild });
      liveIndexingStarted = true;
    } catch (err) {
      liveIndexingError = err instanceof Error ? err.message : String(err);
    }
  }
  if (token.isCancellationRequested) {
    throw repairCancelledError();
  }

  const indexingInBackground =
    needsManagedRebuild && liveIndexingStarted && !liveIndexingError;
  progress.report({
    message: indexingInBackground
      ? "Repair: verifying background indexing…"
      : "Repair: verifying health…",
  });
  const health = await bestEffortHealth(
    repoRoot,
    indexingInBackground
      ? "Managed indexing is rebuilding the semantic index in the background."
      : "Cognis health information is not available yet."
  );
  setLastHealth(repoRoot, health.overall);
  setAutoManaged(repoRoot, true);
  const bootstrap = await buildManagedSetupPayload(repoRoot, health, "repair");

  return {
    bootstrap,
    mcpConfigPath,
    mcpError,
    liveIndexingStarted,
    liveIndexingError,
    health,
    indexingInBackground,
  };
}

export async function syncIndex(full: boolean): Promise<void> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const paths = await fetchPaths(repoRoot);
  const args = full
    ? ["index", "--full", "."]
    : ["index", "."];
  await runCli(repoRoot, args, {
    env: { COGNIS_DB_PATH: paths.db_path },
    label: full ? "index --full" : "index",
  });
}

/**
 * Delete the stored index artifacts under ``.cognis`` and rebuild from scratch.
 *
 * The rebuild runs **synchronously** through the CLI ``index --clear`` path so
 * the DB is fully populated before we report health (no daemon race). The CLI
 * removes the UCKG database + WAL/SHM sidecars and the capsule cache, then
 * cold-indexes the repo. The workspace ``config.yaml`` is preserved. After the
 * rebuild we (re)start the watcher daemon on the populated DB and re-assert MCP
 * wiring.
 */
export async function clearIndexAndReindex(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken
): Promise<SetupResult> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const output = getOutputChannel();

  await ensurePythonReady(repoRoot, progress);

  progress.report({ message: "Clear & Re-index: stopping live indexing…" });
  const paths = await fetchPaths(repoRoot);
  // Stop any running daemon first so the DB file handle is released (important
  // on Windows, where SQLite holds an exclusive lock while open) and so the
  // synchronous rebuild below owns the database.
  try {
    await stopLiveIndexing(repoRoot);
    setLiveIndexing(repoRoot, false);
  } catch (err) {
    output.appendLine(
      `[clear-index] stop indexing warning: ${err instanceof Error ? err.message : String(err)}`
    );
  }
  // Reset cached daemon status so the panel reflects the wiped index immediately.
  setIndexStatus(repoRoot, undefined);
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  // Recreate config defaults if missing; preserves existing config.yaml.
  progress.report({ message: "Clear & Re-index: refreshing workspace config…" });
  await ensureWorkspaceConfigFresh(repoRoot);
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  // Deterministic, synchronous rebuild: `cognis-cli index --clear .` deletes the
  // stored index (DB + sidecars + capsule cache, keeping config.yaml) and runs a
  // full cold index. It only returns once indexing is complete, so the health
  // check below sees the populated DB instead of a transient empty one.
  progress.report({
    message: "Clear & Re-index: rebuilding index from scratch (this can take a few minutes)…",
  });
  const rebuild = await runCli(repoRoot, ["index", "--clear", "."], {
    env: { COGNIS_DB_PATH: paths.db_path },
    label: "index --clear",
  });
  if (rebuild.exitCode !== 0) {
    throw new Error(
      `Index rebuild failed (exit ${rebuild.exitCode}): ${rebuild.stderr || rebuild.stdout}`
    );
  }
  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  // Start the watcher on the already-populated DB. forceFullRebuild is false:
  // the index is fresh, so the daemon only needs to watch for new changes.
  progress.report({ message: "Clear & Re-index: starting live indexing…" });
  let liveIndexingStarted = false;
  let liveIndexingError: string | undefined;
  try {
    await startLive();
    liveIndexingStarted = true;
  } catch (err) {
    liveIndexingError = err instanceof Error ? err.message : String(err);
  }

  // Make sure MCP wiring is still present after the reset.
  let mcpConfigPath: string | undefined;
  let mcpError: string | undefined;
  try {
    const { configPath } = await enableMcpForWorkspace(repoRoot);
    setMcpEnabled(repoRoot, true);
    mcpConfigPath = configPath;
  } catch (err) {
    mcpError = err instanceof Error ? err.message : String(err);
  }

  progress.report({ message: "Clear & Re-index: verifying health…" });
  const health = await bestEffortHealth(
    repoRoot,
    "Cognis health information is not available yet.",
  );
  setLastHealth(repoRoot, health.overall);
  setAutoManaged(repoRoot, true);
  const bootstrap = await buildManagedSetupPayload(repoRoot, health, "clear-reindex");

  return {
    bootstrap,
    mcpConfigPath,
    mcpError,
    liveIndexingStarted,
    liveIndexingError,
    health,
    // Rebuild already completed synchronously above; not a background job.
    indexingInBackground: false,
  };
}

export async function showHealthReport(): Promise<void> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const report = await fetchHealth(repoRoot);
  setLastHealth(repoRoot, report.overall);
  const doc = await vscode.workspace.openTextDocument({
    content: formatHealthSummary(report),
    language: "plaintext",
  });
  await vscode.window.showTextDocument(doc, { preview: true });
}

export async function openAuditLog(): Promise<void> {
  const folder = requireWorkspaceFolder();
  const paths = await fetchPaths(folder.uri.fsPath);
  if (!fs.existsSync(paths.audit_log_path)) {
    await vscode.window.showWarningMessage(
      `Audit log not found at ${paths.audit_log_path}. Run bootstrap first.`
    );
    return;
  }
  const doc = await vscode.workspace.openTextDocument(
    vscode.Uri.file(paths.audit_log_path)
  );
  await vscode.window.showTextDocument(doc);
}

export async function startLive(options?: {
  forceFullRebuild?: boolean;
}): Promise<void> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const paths = await fetchPaths(repoRoot);
  await startLiveIndexing(
    repoRoot,
    paths.db_path,
    paths.indexd_status_path,
    options
  );
  setLiveIndexing(repoRoot, true);
  syncIndexStatusFromDaemon(repoRoot);
}

export async function stopLive(): Promise<void> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  await stopLiveIndexing(repoRoot);
  setLiveIndexing(repoRoot, false);
  syncIndexStatusFromDaemon(repoRoot);
}

export async function enableMcp(options?: { silent?: boolean }): Promise<string> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const { configPath } = await enableMcpForWorkspace(repoRoot);
  setMcpEnabled(repoRoot, true);
  if (!options?.silent) {
    const guidance = mcpReloadRequiredGuidance(configPath);
    const action = await vscode.window.showInformationMessage(
      guidance.message,
      "Open Config",
      "Preview JSON",
      "Show Output"
    );
    if (action === "Open Config") {
      const doc = await vscode.workspace.openTextDocument(
        vscode.Uri.file(configPath)
      );
      await vscode.window.showTextDocument(doc);
    } else if (action === "Preview JSON") {
      await showMcpConfigPreview(repoRoot);
    } else if (action === "Show Output") {
      void vscode.commands.executeCommand("cognis.showOutput");
    }
  }
  return configPath;
}

export async function disableMcp(): Promise<void> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const { configPath, removed } = await disableMcpForWorkspace(repoRoot);
  setMcpEnabled(repoRoot, false);
  if (removed) {
    vscode.window.showInformationMessage(
      `Removed cognis from ${configPath}. Reload your editor or MCP host to apply.`
    );
  } else {
    vscode.window.showWarningMessage(`No cognis entry in ${configPath}.`);
  }
}

/**
 * Tear Cognis out of the current workspace: stop the watcher, disconnect MCP,
 * and delete the local ``.cognis/`` index directory. This is the lifecycle
 * counterpart to Set Up for AI — the "remove the container" action — so a user
 * can cleanly back out without hunting through config files. ``config.yaml`` is
 * intentionally removed too (the whole ``.cognis/`` goes), since setup recreates
 * it. Returns whether the directory was actually deleted.
 *
 * @param options.purgeAllMcp When true, also strip *every* ``cognis-*`` server
 *   from the shared/global MCP host config — not just this repo's entry. This is
 *   the "preparing to uninstall" path: MCP config is written globally by default,
 *   so a per-workspace removal would leave orphaned entries the host keeps trying
 *   to spawn after the extension is gone.
 */
export async function removeFromWorkspace(options?: {
  purgeAllMcp?: boolean;
}): Promise<{
  configPath: string;
  mcpRemoved: boolean;
  cognisDirRemoved: boolean;
  purgedConfigPaths: string[];
}> {
  const folder = requireWorkspaceFolder();
  const repoRoot = folder.uri.fsPath;
  const output = getOutputChannel();

  // 1. Stop the live-indexing daemon so the DB handle is released (Windows
  //    holds an exclusive SQLite lock while open) before we delete the dir.
  try {
    await stopLiveIndexing(repoRoot);
  } catch (err) {
    output.appendLine(
      `[remove] stop indexing warning: ${err instanceof Error ? err.message : String(err)}`
    );
  }
  setLiveIndexing(repoRoot, false);
  setIndexStatus(repoRoot, undefined);

  // 2. Disconnect MCP wiring from the editor/host config.
  let configPath = "";
  let mcpRemoved = false;
  const purgedConfigPaths: string[] = [];
  try {
    if (options?.purgeAllMcp) {
      const touched = await removeAllCognisMcpEntries(repoRoot);
      for (const entry of touched) {
        purgedConfigPaths.push(entry.configPath);
        output.appendLine(
          `[remove] purged ${entry.serverNames.join(", ")} from ${entry.configPath}`
        );
      }
      mcpRemoved = touched.length > 0;
      configPath = touched[0]?.configPath ?? "";
    } else {
      const result = await disableMcpForWorkspace(repoRoot);
      configPath = result.configPath;
      mcpRemoved = result.removed;
    }
  } catch (err) {
    output.appendLine(
      `[remove] disable MCP warning: ${err instanceof Error ? err.message : String(err)}`
    );
  }
  setMcpEnabled(repoRoot, false);

  // 3. Delete the local index directory.
  const cognisDir = path.join(repoRoot, ".cognis");
  let cognisDirRemoved = false;
  try {
    if (fs.existsSync(cognisDir)) {
      fs.rmSync(cognisDir, { recursive: true, force: true });
      cognisDirRemoved = true;
    }
  } catch (err) {
    output.appendLine(
      `[remove] delete .cognis warning: ${err instanceof Error ? err.message : String(err)}`
    );
    throw err;
  }

  // 4. Reset cached state so the panel falls back to "Not set up".
  setLastHealth(repoRoot, undefined);
  setAutoManaged(repoRoot, false);

  return { configPath, mcpRemoved, cognisDirRemoved, purgedConfigPaths };
}

export async function refreshPanelContext(repoRoot: string): Promise<PanelContext> {
  try {
    syncMcpStateFromDisk(repoRoot);
    const report = await fetchHealth(repoRoot);
    setLastHealth(repoRoot, report.overall);
    syncLiveIndexingFromProcess(repoRoot);
    const indexStatus = syncIndexStatusFromDaemon(repoRoot);
    const current = getState(repoRoot);
    return {
      status: deriveStatus(repoRoot, report.overall, false),
      health: report,
      liveIndexing: current.liveIndexing,
      mcpEnabled: current.mcpEnabled,
      indexStatus,
    };
  } catch {
    syncMcpStateFromDisk(repoRoot);
    const configured = isWorkspaceConfigured(repoRoot);
    const indexStatus = syncIndexStatusFromDaemon(repoRoot);
    const current = getState(repoRoot);
    return {
      status: configured ? deriveStatus(repoRoot, undefined, false) : "notInstalled",
      setupHint: configured ? "python" : undefined,
      liveIndexing: current.liveIndexing,
      mcpEnabled: current.mcpEnabled,
      indexStatus,
    };
  }
}

export async function refreshStatus(
  repoRoot: string
): Promise<WorkspaceStatus> {
  if (!isWorkspaceConfigured(repoRoot)) {
    return "notInstalled";
  }
  try {
    syncMcpStateFromDisk(repoRoot);
    const report = await fetchHealth(repoRoot);
    setLastHealth(repoRoot, report.overall);
    syncLiveIndexingFromProcess(repoRoot);
    syncIndexStatusFromDaemon(repoRoot);
    return deriveStatus(repoRoot, report.overall, false);
  } catch {
    return isWorkspaceConfigured(repoRoot)
      ? deriveStatus(repoRoot, undefined, false)
      : "notInstalled";
  }
}

export async function showDegradedGuidance(repoRoot: string): Promise<void> {
  try {
    const report = await fetchHealth(repoRoot);
    throw new CognisGuidanceError(healthDegradedGuidance(report));
  } catch (err) {
    if (err instanceof CognisGuidanceError) {
      throw err;
    }
    throw new CognisGuidanceError({
      title: "Health unavailable",
      message:
        "Cognis health could not be read. Run Repair Setup after confirming Python and bootstrap are configured.",
      severity: "error",
      actions: [
        { label: "Repair Setup", command: "cognis.repairSetup" },
        { label: "Show Output", command: "cognis.showOutput" },
      ],
      technicalDetail: err instanceof Error ? err.message : String(err),
    });
  }
}
