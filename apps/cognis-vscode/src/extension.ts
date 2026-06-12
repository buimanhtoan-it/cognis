import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { trace, type TraceLevel } from "./diagnostics";
import {
  initManagedBackend,
  installManagedBackend,
  isManagedBackendInstalled,
  uninstallManagedBackend,
  checkManagedBackendDrift,
  BackendInstallError,
  formatElapsed,
} from "./backend";
import {
  presentGuidance,
  setupResultGuidance,
  showErrorGuidance,
} from "./guidance";
import {
  isLiveIndexing,
  onDidChangeIndexStatus,
  stopAllIndexing,
} from "./indexd";
import {
  onDidChangeMcpServerState,
  startMcpServer,
  stopAllMcpServers,
  stopMcpServer,
} from "./mcpServer";
import {
  CognisPanelProvider,
  outcomeLabelForContext,
  type PanelContext,
} from "./panel";
import { reconcileWorkspaceOnActivate } from "./reconcile";
import { performHandshake } from "./handshake";
import { handshakeWarning } from "./contract";
import { enterLicenseKey, requireLicense } from "./license";
import {
  addCognisToGitignore,
  shouldRemindGitignore,
} from "./gitignore";
import {
  fetchPrerequisites,
  installAllMissing,
  installPrerequisite,
} from "./prerequisites";
import {
  deriveStatus,
  getState,
  initStateStorage,
  isIndexStatusBusy,
  setIndexStatus,
  setLiveIndexing,
  setMcpEnabled,
} from "./state";
import type { PrerequisiteReport, SetupResult } from "./types";
import { enableMcpForWorkspace, writeHttpMcpConfig } from "./mcpConfig";
import {
  getWorkspaceFolder,
  isWorkspaceConfigured,
  isWorkspaceSyncPaused,
  refreshPanelContext,
  rehydrateWorkspaceState,
  clearIndexAndReindex,
  connectMcp,
  pauseSync,
  removeFromWorkspace,
  repairSetup,
  resumeSync,
  setupWorkspace,
  showHealthReport,
  startLive,
} from "./workspace";

let statusBarItem: vscode.StatusBarItem;
let panelProvider: CognisPanelProvider;
let healthPollTimer: ReturnType<typeof setInterval> | undefined;
let extensionContext: vscode.ExtensionContext | undefined;
let lastPrerequisites: PrerequisiteReport | undefined;
let backendAvailable: boolean | undefined;
let indexingActive = false;
let blockingIndexMessage: string | undefined;
let autoIndexStartPromise: Promise<void> | undefined;

function buildIndexingContext(repoRoot: string): PanelContext {
  const state = getState(repoRoot);
  return {
    status: "indexing",
    liveIndexing: state.liveIndexing,
    mcpEnabled: state.mcpEnabled,
    syncPaused: state.syncPaused,
    indexStatus: state.indexStatus,
    indexingMessage: blockingIndexMessage,
    prerequisites: lastPrerequisites,
    configured: isWorkspaceConfigured(repoRoot),
    backendAvailable,
  };
}

async function fetchPanelContext(repoRoot: string): Promise<PanelContext> {
  const context = await refreshPanelContext(repoRoot);
  return {
    ...context,
    prerequisites: lastPrerequisites,
    configured: isWorkspaceConfigured(repoRoot),
    backendAvailable,
  };
}

/**
 * Re-fetch the prerequisite checklist (via `cognis-cli doctor`) and refresh the
 * panel. Cached so every panel render can show the checklist without re-running
 * the CLI on each poll.
 *
 * A `doctor` report is also our cheapest proof that the Python backend is
 * actually runnable: if it returns a report the backend is reachable; if it
 * returns undefined the backend isn't installed yet (fresh machine), which the
 * panel surfaces as "Install the Cognis backend".
 */
async function refreshPrerequisites(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  lastPrerequisites = await fetchPrerequisites(folder.uri.fsPath);
  backendAvailable = lastPrerequisites !== undefined;
  await pollHealth();
}

function updateStatusBar(context: PanelContext): void {
  statusBarItem.text = outcomeLabelForContext(context);
  statusBarItem.tooltip = "Cognis: click for indexing and MCP setup status";
  statusBarItem.show();
  panelProvider?.updateContext(context);
}

async function pollHealth(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  // Don't let background health polls overwrite "Indexing…" while bootstrap/sync runs.
  if (indexingActive) {
    updateStatusBar(buildIndexingContext(folder.uri.fsPath));
    return;
  }
  const context = await fetchPanelContext(folder.uri.fsPath);
  updateStatusBar(context);
}

async function ensureLiveIndexingForWorkspaceChange(
  uri?: vscode.Uri
): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  if (uri) {
    const owner = vscode.workspace.getWorkspaceFolder(uri);
    if (!owner || owner.uri.fsPath !== folder.uri.fsPath) {
      return;
    }
  }

  const config = vscode.workspace.getConfiguration("cognis");
  if (!config.get<boolean>("autoIndexOnFileChange", true)) {
    return;
  }

  const repoRoot = folder.uri.fsPath;
  if (!isWorkspaceConfigured(repoRoot) || isLiveIndexing(repoRoot)) {
    return;
  }
  // Respect an explicit user pause: don't resurrect the daemon on file save.
  if (isWorkspaceSyncPaused(repoRoot)) {
    return;
  }
  if (autoIndexStartPromise) {
    await autoIndexStartPromise;
    return;
  }

  autoIndexStartPromise = (async () => {
    const output = getOutputChannel();
    output.appendLine(
      "[auto-index] Workspace files changed; starting live indexing daemon."
    );
    try {
      await startLive();
      startHealthPolling();
      await pollHealth();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      output.appendLine(`[auto-index] ${message}`);
    } finally {
      autoIndexStartPromise = undefined;
    }
  })();

  await autoIndexStartPromise;
}

function startHealthPolling(): void {
  const seconds = vscode.workspace
    .getConfiguration("cognis")
    .get<number>("pollHealthSeconds", 30);
  if (healthPollTimer) {
    clearInterval(healthPollTimer);
  }
  healthPollTimer = setInterval(() => {
    void pollHealth();
  }, seconds * 1000);
}

async function withProgress<T>(
  title: string,
  task: (
    progress: vscode.Progress<{ message?: string }>,
    token: vscode.CancellationToken
  ) => Promise<T>
): Promise<T | undefined> {
  indexingActive = true;
  const folder = getWorkspaceFolder();
  if (folder) {
    updateStatusBar(buildIndexingContext(folder.uri.fsPath));
  }
  try {
    return await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title, cancellable: true },
      async (progress, token) => {
        const reportingProgress: vscode.Progress<{ message?: string }> = {
          report: (value) => {
            blockingIndexMessage = value.message;
            if (folder) {
              updateStatusBar(buildIndexingContext(folder.uri.fsPath));
            }
            progress.report(value);
          },
        };
        // Trace every progress-wrapped flow uniformly: start, ok+duration, or
        // failure — so a bug in any flow is reconstructable from the log.
        return trace.span("flow", title, () => task(reportingProgress, token));
      }
    );
  } finally {
    indexingActive = false;
    blockingIndexMessage = undefined;
    await pollHealth();
  }
}

async function reportSetupResult(result: SetupResult): Promise<void> {
  const guidance = setupResultGuidance(result);
  if (guidance) {
    await presentGuidance(guidance);
  }
}

async function runSetupWorkspace(): Promise<void> {
  // Paid-feature gate. No-op (returns true) in the open-source/source build,
  // which ships without an embedded license public key; only the prebuilt
  // commercial build enforces this.
  if (extensionContext && !(await requireLicense(extensionContext, "Set Up Workspace"))) {
    return;
  }
  try {
    const result = await withProgress("Cognis: Set Up Workspace", (p, t) =>
      setupWorkspace(p, t)
    );
    if (result) {
      startHealthPolling();
      await maybeRemindGitignore();
      await reportSetupResult(result);
    }
  } catch (err) {
    await showErrorGuidance(err, "Set Up Workspace");
  }
}

/**
 * After setup, keep ``.cognis/`` out of version control automatically.
 *
 * The directory holds the local index DB, caches, and audit log — machine
 * specific files that should never be committed. Rather than asking, we just
 * add the entry when we're inside a git repo and it isn't ignored yet, then
 * show a non-blocking notice (with a quick way to view the change). Idempotent.
 */
async function maybeRemindGitignore(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  const repoRoot = folder.uri.fsPath;
  if (!shouldRemindGitignore(repoRoot)) {
    return;
  }
  const written = addCognisToGitignore(repoRoot);
  if (!written) {
    getOutputChannel().appendLine(
      "[gitignore] Could not update .gitignore automatically. Check file permissions."
    );
    return;
  }
  const choice = await vscode.window.showInformationMessage(
    "Added `.cognis/` to .gitignore so the local index, caches, and audit log aren't committed.",
    "View .gitignore"
  );
  if (choice === "View .gitignore") {
    try {
      const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(written));
      await vscode.window.showTextDocument(doc);
    } catch {
      // Non-fatal: the entry is already written regardless of whether we can
      // open the editor.
    }
  }
}

async function runRepairSetup(): Promise<void> {
  try {
    const result = await withProgress("Cognis: Repair Setup", (p, t) =>
      repairSetup(p, t)
    );
    if (result) {
      startHealthPolling();
      await reportSetupResult(result);
    }
  } catch (err) {
    await showErrorGuidance(err, "Repair Setup");
  }
}

async function runClearAndReindex(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before clearing the Cognis index."),
      "Clear & Re-index"
    );
    return;
  }
  // Destructive action: deletes the stored index and forces a full rebuild.
  const confirmation = await vscode.window.showWarningMessage(
    "Clear the Cognis index for this workspace and re-index from scratch? " +
      "This deletes the local index database and capsule cache (your config and " +
      "MCP wiring are kept), then rebuilds the index. Re-indexing a large repo can take a few minutes.",
    { modal: true },
    "Clear & Re-index"
  );
  if (confirmation !== "Clear & Re-index") {
    return;
  }
  try {
    const result = await withProgress("Cognis: Clear & Re-index", (p, t) =>
      clearIndexAndReindex(p, t)
    );
    if (result) {
      startHealthPolling();
      await reportSetupResult(result);
    }
  } catch (err) {
    await showErrorGuidance(err, "Clear & Re-index");
  }
}

/**
 * Connect MCP: write the workspace ``mcp.json`` on disk for the current editor
 * and open it. Concrete wiring (no copy-paste guide) — see {@link connectMcp}.
 */
async function runConnectMcp(): Promise<void> {
  try {
    await connectMcp();
  } catch (err) {
    await showErrorGuidance(err, "Connect MCP");
  }
}

/**
 * Start the standalone HTTP MCP server and make it usable in one click:
 * pre-flight the workspace, start the server, write the url-form mcp.json so the
 * editor connects to it, then tell the user exactly which file changed and that
 * a window reload is needed (offering the Reload button).
 */
async function runStartMcpServer(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  const repoRoot = folder.uri.fsPath;
  trace.info("flow", "Start MCP server requested", { repoRoot });

  // Pre-flight: warn with the exact next action instead of failing opaquely.
  if (!isWorkspaceConfigured(repoRoot)) {
    const pick = await vscode.window.showWarningMessage(
      "Set up Cognis for this workspace before starting the MCP server (it needs an index to serve).",
      "Set Up Workspace"
    );
    if (pick === "Set Up Workspace") {
      await runSetupWorkspace();
    }
    return;
  }

  try {
    const state = await startMcpServer(repoRoot);
    if (state.phase === "error" || !state.url) {
      await showErrorGuidance(
        new Error(state.lastError ?? "the server did not report a URL"),
        "Start MCP server"
      );
      return;
    }
    // Make it usable: point the editor's mcp.json at the running server.
    const { configPath } = writeHttpMcpConfig(repoRoot, state.url);
    setMcpEnabled(repoRoot, true);
    trace.info("flow", "MCP server running", { url: state.url, configPath });
    await pollHealth();
    const choice = await vscode.window.showInformationMessage(
      `Cognis MCP server running at ${state.url}. Updated ${configPath} to use it. ` +
        "Reload the window so your editor connects.",
      "Reload Window"
    );
    if (choice === "Reload Window") {
      void vscode.commands.executeCommand("workbench.action.reloadWindow");
    }
  } catch (err) {
    await showErrorGuidance(err, "Start MCP server");
  }
}

/**
 * Stop the HTTP server and revert mcp.json to the editor-managed (stdio) form,
 * so the editor keeps working (it auto-spawns stdio) instead of pointing at
 * a dead URL. Tells the user a reload applies the change.
 */
async function runStopMcpServer(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  const repoRoot = folder.uri.fsPath;
  trace.info("flow", "Stop MCP server requested", { repoRoot });
  await stopMcpServer(repoRoot);
  let reverted = false;
  try {
    await enableMcpForWorkspace(repoRoot);
    reverted = true;
  } catch (err) {
    getOutputChannel().appendLine(
      `[mcp-http] could not revert mcp.json to stdio: ${
        err instanceof Error ? err.message : String(err)
      }`
    );
  }
  await pollHealth();
  const detail = reverted
    ? "Reverted mcp.json to the editor-managed (stdio) config so your editor keeps working. Reload the window to apply."
    : "Could not rewrite mcp.json automatically — open the Cognis output log for details.";
  const choice = await vscode.window.showInformationMessage(
    `Cognis MCP server stopped. ${detail}`,
    "Reload Window"
  );
  if (choice === "Reload Window") {
    void vscode.commands.executeCommand("workbench.action.reloadWindow");
  }
}

async function runPauseSync(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before pausing index sync."),
      "Pause sync"
    );
    return;
  }
  try {
    await withProgress("Cognis: Pause index sync", async () => pauseSync());
    await pollHealth();
    await vscode.window.showInformationMessage(
      "Index sync paused. Cognis keeps answering against the last-synced index but " +
        "stops re-indexing file changes until you resume."
    );
  } catch (err) {
    await showErrorGuidance(err, "Pause sync");
  }
}

async function runResumeSync(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before resuming index sync."),
      "Resume sync"
    );
    return;
  }
  try {
    await withProgress("Cognis: Resume index sync", async () => resumeSync());
    startHealthPolling();
    await pollHealth();
    await vscode.window.showInformationMessage(
      "Index sync resumed. Cognis is watching this workspace for changes again."
    );
  } catch (err) {
    await showErrorGuidance(err, "Resume sync");
  }
}

/**
 * Lifecycle "remove" action: stop indexing, disconnect MCP, and delete the
 * local ``.cognis/`` directory after an explicit confirmation. Mirrors the
 * "remove the container" mental model so users can cleanly back out.
 *
 * @param scope "workspace" removes only this repo's wiring; "all" also purges
 *   every cognis-* entry from the shared global MCP config (uninstall prep).
 */
async function runRemoveFromWorkspace(scope: "workspace" | "all" = "workspace"): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before removing Cognis."),
      "Remove from workspace"
    );
    return;
  }
  const purgeAllMcp = scope === "all";
  const confirmMessage = purgeAllMcp
    ? "Remove Cognis everywhere and prepare to uninstall? This stops live indexing, " +
      "deletes this workspace's local .cognis index, removes EVERY cognis MCP server " +
      "entry from your editor config (all repos), and uninstalls the Cognis backend that " +
      "Cognis installed for you. Your source code is not touched."
    : "Remove Cognis from this workspace? This stops live indexing, disconnects the " +
      "MCP server from your editor, and deletes the local .cognis index directory " +
      "(database, caches, audit log). Your source code is not touched. You can run " +
      "Set Up Workspace again later to recreate it.";
  const confirmLabel = purgeAllMcp ? "Remove Everything" : "Remove";
  const confirmation = await vscode.window.showWarningMessage(
    confirmMessage,
    { modal: true },
    confirmLabel
  );
  if (confirmation !== confirmLabel) {
    return;
  }
  try {
    const result = await withProgress(
      purgeAllMcp ? "Cognis: Prepare for uninstall" : "Cognis: Remove from workspace",
      async () => removeFromWorkspace({ purgeAllMcp })
    );
    if (!result) {
      return;
    }
    // Re-probe the backend/prereqs next poll instead of trusting stale state.
    lastPrerequisites = undefined;
    const parts: string[] = [];
    if (result.cognisDirRemoved) {
      parts.push("deleted the local .cognis index");
    }
    if (purgeAllMcp && result.purgedConfigPaths.length > 0) {
      parts.push(
        `removed all Cognis MCP entries from ${result.purgedConfigPaths.join(", ")}`
      );
    } else if (result.mcpRemoved) {
      parts.push(`disconnected MCP from ${result.configPath}`);
    }
    // "Remove everything" also uninstalls the backend Cognis installed so the
    // user gets a clean machine without touching a terminal.
    if (purgeAllMcp) {
      const userPythonPath = vscode.workspace
        .getConfiguration("cognis")
        .get<string>("pythonPath", "")
        .trim();
      try {
        const backend = await uninstallManagedBackend({
          userPythonPath: userPythonPath || undefined,
        });
        if (backend.mode !== "none") {
          parts.push(
            backend.mode === "managed-deleted"
              ? "uninstalled the Cognis backend"
              : "removed the cognis package from your Python"
          );
        }
      } catch (err) {
        getOutputChannel().appendLine(
          `[remove] backend uninstall warning: ${err instanceof Error ? err.message : String(err)}`
        );
      }
    }
    await refreshPrerequisites();
    let summary =
      parts.length > 0
        ? `Removed Cognis: ${parts.join("; ")}. Reload your editor or MCP host to apply.`
        : "Cognis was not configured for this workspace.";
    if (purgeAllMcp) {
      summary += " You can now uninstall the extension.";
    }
    await vscode.window.showInformationMessage(summary);
    await pollHealth();
  } catch (err) {
    await showErrorGuidance(err, "Remove from workspace");
  }
}

/**
 * One-click backend install. Creates/refreshes the managed environment (or uses
 * the user's own Python if cognis.pythonPath is set), installs the package, then
 * re-probes so the panel advances on its own — no terminal, no manual steps.
 */
async function runInstallBackend(): Promise<void> {
  const userPythonPath = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("pythonPath", "")
    .trim();
  try {
    const outcome = await withProgress("Cognis: Install backend", (p, t) =>
      installManagedBackend(p, t, { userPythonPath: userPythonPath || undefined })
    );
    if (!outcome) {
      return;
    }
    await refreshPrerequisites();
    const where =
      outcome.mode === "managed"
        ? "in a private environment Cognis manages for you"
        : "in your configured Python environment";
    // Report how long the whole install took (sum of phases) so the user gets a
    // sense of the cost and a confirmation it actually finished.
    const totalMs = outcome.timings.reduce((sum, t) => sum + t.ms, 0);
    const next = await vscode.window.showInformationMessage(
      `Cognis backend installed ${where} in ${formatElapsed(totalMs)}. Set up this workspace now?`,
      "Set Up Workspace",
      "Later"
    );
    if (next === "Set Up Workspace") {
      await runSetupWorkspace();
    }
  } catch (err) {
    if (err instanceof BackendInstallError) {
      const actions: string[] = [];
      if (err.canInstallPython) {
        actions.push("Get Python");
      }
      if (err.actionLabel) {
        actions.push(err.actionLabel);
      }
      actions.push("Show Output");
      const choice = await vscode.window.showErrorMessage(err.userMessage, ...actions);
      if (choice === "Get Python") {
        void vscode.env.openExternal(
          vscode.Uri.parse("https://www.python.org/downloads/")
        );
      } else if (choice === err.actionLabel && err.actionUrl) {
        void vscode.env.openExternal(vscode.Uri.parse(err.actionUrl));
      } else if (choice === "Show Output") {
        void vscode.commands.executeCommand("cognis.showOutput");
      }
      return;
    }
    await showErrorGuidance(err, "Install backend");
  }
}

/**
 * After an extension update, detect a managed backend that's older than the
 * extension and offer a one-click upgrade. Only prompts for the managed env
 * (never a bring-your-own Python), and remembers a "skip this version" choice so
 * it doesn't nag. Silent when nothing is installed or versions already match.
 */
async function maybeUpgradeBackend(): Promise<void> {
  const userPythonPath = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("pythonPath", "")
    .trim();
  let drift;
  try {
    drift = await checkManagedBackendDrift({
      userPythonPath: userPythonPath || undefined,
    });
  } catch {
    return;
  }
  if (!drift.outdated || !drift.installed || !drift.expected) {
    return;
  }
  const skipKey = `cognis.skipBackendUpgrade.${drift.expected}`;
  if (extensionContext?.globalState.get<boolean>(skipKey)) {
    return;
  }
  const choice = await vscode.window.showInformationMessage(
    `Cognis was updated to ${drift.expected}, but its backend is still ${drift.installed}. ` +
      "Upgrade the backend so features and fixes match?",
    "Upgrade backend",
    "Later",
    "Skip this version"
  );
  if (choice === "Skip this version") {
    await extensionContext?.globalState.update(skipKey, true);
    return;
  }
  if (choice === "Upgrade backend") {
    await runInstallBackend();
  }
}

/**
 * Verify the backend implements the contract version this extension was built
 * against, and surface a clear, actionable warning when they disagree. This is
 * the version-skew guard: the extension updates via the marketplace while the
 * backend updates via PyPI, so a mismatch is a normal production state that the
 * matched-version e2e suite cannot catch. Silent when the backend can't be
 * reached (fresh machine) or the contract matches. Remembers a per-skew "skip"
 * so it never nags.
 */
async function maybeCheckHandshake(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  const result = await performHandshake(folder.uri.fsPath);
  if (!result || result.compatibility === "ok") {
    return;
  }
  const warning = handshakeWarning(result);
  if (!warning) {
    return;
  }
  const skipKey =
    `cognis.skipHandshakeWarning.${result.compatibility}.` +
    `${result.backendContractVersion ?? "x"}->${result.expectedContractVersion}`;
  if (extensionContext?.globalState.get<boolean>(skipKey)) {
    return;
  }
  const choice = await vscode.window.showWarningMessage(
    warning,
    "Install Backend",
    "Show Diagnostics",
    "Dismiss"
  );
  if (choice === "Install Backend") {
    await runInstallBackend();
  } else if (choice === "Show Diagnostics") {
    void vscode.commands.executeCommand("cognis.showDiagnostics");
  } else if (choice === "Dismiss") {
    await extensionContext?.globalState.update(skipKey, true);
  }
}

export function activate(context: vscode.ExtensionContext): void {
  extensionContext = context;
  initStateStorage(context);
  const extVersion = context.extension?.packageJSON?.version as string | undefined;
  trace.init(context, extVersion);
  const configuredLevel = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("logLevel", "info");
  trace.setMinLevel(
    (["debug", "info", "warn", "error"].includes(configuredLevel)
      ? configuredLevel
      : "info") as TraceLevel
  );
  trace.info("activate", "Cognis extension activating", { extVersion });
  initManagedBackend(context, extVersion);
  panelProvider = new CognisPanelProvider(
    context.extensionUri,
    context.extension?.packageJSON?.version
  );
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      CognisPanelProvider.viewType,
      panelProvider
    )
  );

  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100
  );
  statusBarItem.command = "cognis.openPanel";
  context.subscriptions.push(statusBarItem);

  const output = getOutputChannel();
  context.subscriptions.push(output);

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.showOutput", () => {
      output.show(true);
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.showDiagnostics", async () => {
      const file = trace.logFilePath();
      if (!file) {
        await vscode.window.showWarningMessage(
          "Diagnostics log is not ready yet. Try again in a moment."
        );
        return;
      }
      try {
        const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
        await vscode.window.showTextDocument(doc);
      } catch {
        await vscode.window.showWarningMessage(
          `Could not open the diagnostics log at ${file}.`
        );
      }
    })
  );

  context.subscriptions.push(
    onDidChangeIndexStatus(({ repoRoot, status }) => {
      const folder = getWorkspaceFolder();
      if (!folder || folder.uri.fsPath !== repoRoot) {
        return;
      }
      setIndexStatus(repoRoot, status);
      if (status && getState(repoRoot).liveIndexing !== status.active) {
        setLiveIndexing(repoRoot, status.active);
      }
      if (indexingActive || isIndexStatusBusy(status)) {
        updateStatusBar(buildIndexingContext(repoRoot));
        return;
      }
      const state = getState(repoRoot);
      if (state.lastHealth) {
        updateStatusBar({
          status: deriveStatus(repoRoot, state.lastHealth, false),
          liveIndexing: state.liveIndexing,
          mcpEnabled: state.mcpEnabled,
          syncPaused: state.syncPaused,
          indexStatus: state.indexStatus,
        });
        return;
      }
      void pollHealth();
    })
  );

  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (document.uri.scheme !== "file") {
        return;
      }
      void ensureLiveIndexingForWorkspaceChange(document.uri);
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidCreateFiles((event) => {
      void ensureLiveIndexingForWorkspaceChange(event.files[0]);
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidDeleteFiles((event) => {
      void ensureLiveIndexingForWorkspaceChange(event.files[0]);
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidRenameFiles((event) => {
      void ensureLiveIndexingForWorkspaceChange(event.files[0]?.newUri);
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.setupWorkspace", () => runSetupWorkspace())
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.repairSetup", () => runRepairSetup())
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.clearAndReindex", () =>
      runClearAndReindex()
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.connectMcp", () =>
      runConnectMcp()
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.pauseSync", () => runPauseSync())
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.resumeSync", () => runResumeSync())
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.enterLicense", () =>
      enterLicenseKey(context)
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.installBackend", () =>
      runInstallBackend()
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.removeFromWorkspace", () =>
      runRemoveFromWorkspace("workspace")
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.prepareUninstall", () =>
      runRemoveFromWorkspace("all")
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.showHealth", async () => {
      try {
        await showHealthReport();
      } catch (err) {
        await showErrorGuidance(err, "Health report");
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.startMcpServer", () => runStartMcpServer())
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.stopMcpServer", () => runStopMcpServer())
  );

  context.subscriptions.push(
    onDidChangeMcpServerState(({ repoRoot }) => {
      const folder = getWorkspaceFolder();
      if (folder && folder.uri.fsPath === repoRoot) {
        void pollHealth();
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.openPanel", () => {
      panelProvider.reveal();
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.refreshPrerequisites", () =>
      refreshPrerequisites()
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand(
      "cognis.installPrerequisite",
      async (itemId?: string) => {
        const folder = getWorkspaceFolder();
        if (!folder || !itemId) {
          return;
        }
        const item = lastPrerequisites?.items.find((i) => i.id === itemId);
        if (!item) {
          return;
        }
        installPrerequisite(folder.uri.fsPath, item.install_target);
      }
    )
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.installAllPrerequisites", () => {
      const folder = getWorkspaceFolder();
      if (!folder || !lastPrerequisites) {
        return;
      }
      installAllMissing(folder.uri.fsPath, lastPrerequisites.combined_install_target);
    })
  );

  void (async () => {
    await rehydrateWorkspaceState();
    const folder = getWorkspaceFolder();
    if (folder) {
      const state = getState(folder.uri.fsPath);
      if (state.lastHealth) {
        updateStatusBar({
          status: deriveStatus(folder.uri.fsPath, state.lastHealth, false),
          liveIndexing: state.liveIndexing,
          mcpEnabled: state.mcpEnabled,
          syncPaused: state.syncPaused,
          indexStatus: state.indexStatus,
        });
      }
    }
    // Populate the prerequisite checklist early so the panel can show it (and
    // gate setup) before the user takes any action.
    await refreshPrerequisites();
    await pollHealth();
    startHealthPolling();

    // After an extension update the managed backend can lag behind. Offer a
    // one-click upgrade so the running backend matches the extension version.
    void maybeUpgradeBackend();

    // Independently of the version *number*, verify the backend implements the
    // contract this extension was built against, and warn on a skew before it
    // manifests as a silent failure.
    void maybeCheckHandshake();

    indexingActive = true;
    blockingIndexMessage = "Inspecting workspace and checking live indexing…";
    if (folder) {
      updateStatusBar(buildIndexingContext(folder.uri.fsPath));
    }
    const silentProgress: vscode.Progress<{ message?: string }> = {
      report: (value) => {
        blockingIndexMessage = value.message;
        if (folder) {
          updateStatusBar(buildIndexingContext(folder.uri.fsPath));
        }
      },
    };
    const noCancelToken: vscode.CancellationToken = {
      isCancellationRequested: false,
      onCancellationRequested: () => ({ dispose: () => {} }),
    };
    void (async () => {
      try {
        await reconcileWorkspaceOnActivate(
          context,
          silentProgress,
          noCancelToken
        );
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (message !== "Cancelled") {
          getOutputChannel().appendLine(`[reconcile] ${message}`);
          await showErrorGuidance(err, "Auto-manage on activate");
        }
      } finally {
        indexingActive = false;
        blockingIndexMessage = undefined;
        await pollHealth();
      }
    })();
  })();
  context.subscriptions.push({
    dispose: () => {
      if (healthPollTimer) {
        clearInterval(healthPollTimer);
      }
      stopAllIndexing();
      void stopAllMcpServers();
    },
  });
}

export function deactivate(): void {
  stopAllIndexing();
  void stopAllMcpServers();
  if (healthPollTimer) {
    clearInterval(healthPollTimer);
  }
}
