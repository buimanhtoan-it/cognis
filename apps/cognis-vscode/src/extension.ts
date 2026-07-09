import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { trace, type TraceLevel } from "./diagnostics";
import {
  initManagedBinary,
  installManagedBinary,
  uninstallManagedBinary,
  checkManagedBinaryDrift,
  BinaryInstallError,
  formatElapsed,
} from "./binary";
import {
  initManagedModel,
  installManagedModel,
  isModelInstalled,
  uninstallManagedModel,
} from "./model";
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
  disableMcp,
  pauseSync,
  removeFromWorkspace,
  repairSetup,
  resumeSync,
  setupWorkspace,
  showHealthReport,
  startLive,
  stopLive,
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
/**
 * The most recent {@link PanelContext} handed to {@link updateStatusBar}. Kept
 * so the ``cognis.advancedMode`` configuration listener can flip
 * ``advancedMode`` and re-render the panel in place (≤2s, no window reload)
 * without re-deriving the whole workspace state (R3.3).
 */
let lastContext: PanelContext | undefined;

/**
 * Read the ``cognis.advancedMode`` setting (default ``false``). Advanced/Debug
 * mode only controls which panel surface renders; if reading the config throws
 * for any reason, keep the safe default (``false``) so the panel falls back to
 * the Minimal_Surface (R3.4).
 */
function readAdvancedMode(): boolean {
  try {
    return vscode.workspace
      .getConfiguration("cognis")
      .get<boolean>("advancedMode", false);
  } catch {
    return false;
  }
}

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
    advancedMode: readAdvancedMode(),
  };
}

async function fetchPanelContext(repoRoot: string): Promise<PanelContext> {
  const context = await refreshPanelContext(repoRoot);
  return {
    ...context,
    prerequisites: lastPrerequisites,
    configured: isWorkspaceConfigured(repoRoot),
    backendAvailable,
    advancedMode: readAdvancedMode(),
  };
}

/**
 * Re-fetch the prerequisite checklist (via `cognis-cli doctor`) and refresh the
 * panel. Cached so every panel render can show the checklist without re-running
 * the CLI on each poll.
 *
 * A `doctor` report is also our cheapest proof that the engine binary is
 * actually runnable: if it returns a report the backend is reachable; if it
 * returns undefined the binary isn't installed yet (fresh machine), which the
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

/**
 * Mirror the panel's runtime state into VS Code context keys so the
 * ``view/title`` toggle buttons (Start/Stop MCP, Pause/Resume sync) show the
 * action that matches the current state. These keys are ephemeral and are
 * re-evaluated on every render via {@link updateStatusBar}.
 */
function setPanelContextKeys(ctx: PanelContext): void {
  const running =
    ctx.mcpServerPhase === "running" || ctx.mcpServerPhase === "starting";
  void vscode.commands.executeCommand(
    "setContext",
    "cognis.mcpServerRunning",
    running
  );
  void vscode.commands.executeCommand(
    "setContext",
    "cognis.syncPaused",
    Boolean(ctx.syncPaused)
  );
}

function updateStatusBar(context: PanelContext): void {
  lastContext = context;
  statusBarItem.text = outcomeLabelForContext(context);
  statusBarItem.tooltip = "Cognis: click for indexing and MCP setup status";
  statusBarItem.show();
  setPanelContextKeys(context);
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
 * Disconnect MCP: the non-destructive counterpart to Connect MCP. Removes only
 * this repo's Cognis entry from the editor's ``mcp.json`` (global + workspace
 * scope) via {@link disableMcp}, keeping the local ``.cognis`` index, source
 * code, and other workspaces' MCP entries untouched. The panel reflects the
 * disconnected state on the next health poll (≤2s).
 */
async function runDisconnectMcp(): Promise<void> {
  try {
    await disableMcp();
    await pollHealth();
  } catch (err) {
    await showErrorGuidance(err, "Disconnect MCP");
  }
}

/**
 * Cancel a running index build. Reuses {@link stopLive}, which stops the live
 * indexing daemon for this workspace (kill by pid/proc, waiting up to 5s for the
 * process to exit). This is non-destructive: the existing ``.cognis`` index
 * (including anything written before the cancel) and source code are kept, so
 * the partial index stays queryable. The panel returns to idle on the next
 * health poll.
 */
async function runCancelIndexing(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }
  try {
    await stopLive();
    await pollHealth();
  } catch (err) {
    await showErrorGuidance(err, "Cancel indexing");
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
    // "Remove everything" also uninstalls the engine binary + model Cognis
    // installed so the user gets a clean machine without touching a terminal.
    if (purgeAllMcp) {
      try {
        const binary = await uninstallManagedBinary();
        if (binary.removed) {
          parts.push("uninstalled the Cognis engine binary");
        }
      } catch (err) {
        getOutputChannel().appendLine(
          `[remove] binary uninstall warning: ${err instanceof Error ? err.message : String(err)}`
        );
      }
      if (uninstallManagedModel()) {
        parts.push("removed the semantic model");
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
 * One-click backend install: fetch the prebuilt single ``cognis`` binary for
 * this platform, checksum-verified — no Python, pip, or compiler (Requirement
 * 1.1). We re-probe afterwards so the panel advances on its own.
 */
async function runInstallBackend(): Promise<void> {
  await runInstallBinaryBackend();
}

/**
 * One-click "Start Cognis" flow behind the Unified_Control's ``off`` state.
 *
 * Reuses the existing routines sequentially to turn Cognis on for this
 * workspace (R1.5):
 *   1. ensure the engine binary is installed (``runInstallBackend`` when
 *      ``backendAvailable === false``),
 *   2. set the workspace up when it isn't ``configured`` yet
 *      (``runSetupWorkspace``),
 *   3. start live indexing, and
 *   4. connect MCP (``runConnectMcp``).
 *
 * The sequence stops early on the first step that fails or is cancelled in a
 * user-visible way. The reused routines already surface their own detailed
 * error guidance; between steps we re-check state (``backendAvailable`` /
 * ``isWorkspaceConfigured``) and, on a stall, show a short "not finished"
 * message and halt — without deleting or modifying the user's source or the
 * local ``.cognis`` index (R6.5, R9.2).
 */
async function runStartCognis(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before starting Cognis."),
      "Start Cognis"
    );
    return;
  }
  const repoRoot = folder.uri.fsPath;

  // Stop early: surface an actionable "not finished" notice and leave the
  // source tree + local .cognis index untouched (R6.5, R9.2).
  const stopEarly = (detail: string): void => {
    trace.warn("flow", `Start Cognis stopped early: ${detail}`);
    void vscode.window.showWarningMessage(
      `Cognis isn't fully started yet — ${detail} Your code and local index are ` +
        "unchanged; run “Start Cognis” again to continue."
    );
  };

  // 1. Ensure the engine binary is installed (fresh machine). Read the module
  // flag through helpers so its value is re-observed after the awaited install
  // re-probes prerequisites (a plain `if` would let the type narrow to stale).
  const engineKnownMissing = (): boolean => backendAvailable === false;
  const engineReady = (): boolean => backendAvailable === true;
  if (engineKnownMissing()) {
    await runInstallBackend();
    // runInstallBackend re-probes prerequisites and updates backendAvailable;
    // if the engine still isn't runnable, stop before touching the workspace.
    if (!engineReady()) {
      stopEarly("the Cognis engine isn't installed.");
      return;
    }
  }

  // 2. Set the workspace up if it hasn't been configured yet.
  if (!isWorkspaceConfigured(repoRoot)) {
    await runSetupWorkspace();
    if (!isWorkspaceConfigured(repoRoot)) {
      stopEarly("workspace setup did not complete.");
      return;
    }
  }

  // 3. Start live indexing (idempotent — skip when the daemon already runs).
  if (!isLiveIndexing(repoRoot)) {
    try {
      await withProgress("Cognis: Start live indexing", async () => startLive());
      startHealthPolling();
    } catch (err) {
      await showErrorGuidance(err, "Start Cognis");
      stopEarly("live indexing could not start.");
      return;
    }
  }

  // 4. Connect MCP so the editor can reach the index (handles its own errors).
  await runConnectMcp();

  await pollHealth();
}

/**
 * Fetch + verify the single ``cognis`` binary and advance setup. Surfaces a
 * clear message when no binary is published for the platform or the
 * download/verification fails.
 */
async function runInstallBinaryBackend(): Promise<void> {
  try {
    const outcome = await withProgress("Cognis: Install backend", (p, t) =>
      installManagedBinary(p, t)
    );
    if (!outcome) {
      return;
    }
    await refreshPrerequisites();
    const totalMs = outcome.timings.reduce((sum, t) => sum + t.ms, 0);

    // Fetch the semantic model (best-effort). Semantic search needs the
    // bge-small weights; download them now so `diffuse_context` returns real
    // hits. A failure here is non-fatal — the engine degrades to lexical +
    // structural until the model is present.
    let semanticNote = " Semantic search is active.";
    if (!isModelInstalled()) {
      try {
        await withProgress("Cognis: Download semantic model", (p, t) =>
          installManagedModel(p, t)
        );
      } catch (err) {
        getOutputChannel().appendLine(
          `[model] install skipped: ${err instanceof Error ? err.message : String(err)}`
        );
        semanticNote =
          " Semantic search will activate once the model finishes downloading " +
          "(run “Cognis: Install backend” again to retry); lexical + structural search work now.";
      }
    }

    const next = await vscode.window.showInformationMessage(
      `Cognis engine installed (${outcome.triple}) in ${formatElapsed(totalMs)} — no Python needed.${semanticNote} ` +
        "Set up this workspace now?",
      "Set Up Workspace",
      "Later"
    );
    if (next === "Set Up Workspace") {
      await runSetupWorkspace();
    }
  } catch (err) {
    if (err instanceof BinaryInstallError) {
      const choice = await vscode.window.showErrorMessage(
        err.userMessage,
        "Show Output"
      );
      if (choice === "Show Output") {
        void vscode.commands.executeCommand("cognis.showOutput");
      }
      return;
    }
    await showErrorGuidance(err, "Install backend");
  }
}

/**
 * Reinstall just the engine: delete the managed binary + semantic model, then
 * re-download + checksum-verify fresh copies. Fixes a corrupt, stale, or
 * version-mismatched engine without touching the index or MCP wiring. The
 * install uses the Windows-safe swap, so a still-running engine is replaced
 * rather than failing with EPERM.
 */
async function runReinstallEngine(): Promise<void> {
  const confirm = await vscode.window.showWarningMessage(
    "Reinstall the Cognis engine? This deletes the downloaded binary + semantic " +
      "model and fetches fresh, checksum-verified copies from the release. Your " +
      "index and MCP config are kept.",
    { modal: true },
    "Reinstall Engine"
  );
  if (confirm !== "Reinstall Engine") {
    return;
  }
  try {
    await withProgress("Cognis: Reinstall engine — removing", async () => {
      try {
        await uninstallManagedBinary();
      } catch (err) {
        getOutputChannel().appendLine(
          `[reinstall] binary remove warning: ${err instanceof Error ? err.message : String(err)}`
        );
      }
      uninstallManagedModel();
    });
  } catch (err) {
    await showErrorGuidance(err, "Reinstall engine");
    return;
  }
  lastPrerequisites = undefined;
  // Re-download binary + model (also offers Set Up Workspace on success).
  await runInstallBinaryBackend();
}

/**
 * Uninstall the engine: the non-destructive inverse of Install Engine. Deletes
 * only the downloaded engine binary + semantic model. The workspace index
 * (`.cognis`), MCP config, and source code are all kept. If the user cancels
 * the confirmation, nothing changes. On failure, existing state is preserved
 * and an error indicator is shown.
 */
async function runUninstallEngine(): Promise<void> {
  const confirm = await vscode.window.showWarningMessage(
    "Uninstall the Cognis engine (downloaded binary + semantic model)? Your " +
      "workspace index (.cognis) and MCP config are kept.",
    { modal: true },
    "Uninstall Engine"
  );
  if (confirm !== "Uninstall Engine") {
    return;
  }
  try {
    await uninstallManagedBinary();
    uninstallManagedModel();
    lastPrerequisites = undefined;
    await refreshPrerequisites();
  } catch (err) {
    await showErrorGuidance(err, "Uninstall engine");
  }
}

/**
 * Cold restart: the one-click recovery from a corrupted/stale state. Wipes
 * everything Cognis manages — this workspace's `.cognis` index, ALL cognis MCP
 * entries, and the downloaded engine binary + model — then rebuilds from
 * scratch: re-download the engine + model and re-index the workspace fresh.
 * Source code is never touched.
 */
async function runColdRestart(): Promise<void> {
  const folder = getWorkspaceFolder();
  if (!folder) {
    await showErrorGuidance(
      new Error("Open a workspace folder before restarting Cognis."),
      "Cold restart"
    );
    return;
  }
  const confirm = await vscode.window.showWarningMessage(
    "Cold restart Cognis? This deletes this workspace's .cognis index, removes ALL " +
      "Cognis MCP entries from your editor, uninstalls the downloaded engine + model, " +
      "then re-downloads everything and sets the workspace up fresh. Your source code " +
      "is not touched.",
    { modal: true },
    "Cold Restart"
  );
  if (confirm !== "Cold Restart") {
    return;
  }
  try {
    // 1. Full wipe: stops live indexing, deletes .cognis, purges every cognis
    //    MCP entry (removeFromWorkspace stops the daemon first to free the DB).
    await withProgress("Cognis: Cold restart — cleaning up", async () =>
      removeFromWorkspace({ purgeAllMcp: true })
    );
    try {
      await uninstallManagedBinary();
    } catch (err) {
      getOutputChannel().appendLine(
        `[cold-restart] binary remove warning: ${err instanceof Error ? err.message : String(err)}`
      );
    }
    uninstallManagedModel();
    lastPrerequisites = undefined;

    // 2. Rebuild: fresh binary + model, then set the workspace up from scratch.
    const outcome = await withProgress("Cognis: Cold restart — installing engine", (p, t) =>
      installManagedBinary(p, t)
    );
    if (!outcome) {
      return;
    }
    if (!isModelInstalled()) {
      try {
        await withProgress("Cognis: Cold restart — downloading model", (p, t) =>
          installManagedModel(p, t)
        );
      } catch (err) {
        getOutputChannel().appendLine(
          `[cold-restart] model install skipped: ${err instanceof Error ? err.message : String(err)}`
        );
      }
    }
    await refreshPrerequisites();
    await runSetupWorkspace();
    await vscode.window.showInformationMessage(
      "Cognis cold restart complete — engine + model reinstalled and the workspace re-indexed from scratch. " +
        "Reload the window so your editor picks up the fresh MCP server."
    );
  } catch (err) {
    await showErrorGuidance(err, "Cold restart");
  }
}

/**
 * After an extension update, detect a managed engine binary that's older than
 * the extension and offer a one-click upgrade. Remembers a "skip this version"
 * choice so it doesn't nag. Silent when nothing is installed or versions match.
 */
async function maybeUpgradeBackend(): Promise<void> {
  const binaryDrift = checkManagedBinaryDrift();
  if (!binaryDrift.outdated || !binaryDrift.installed || !binaryDrift.expected) {
    return;
  }
  const installed = binaryDrift.installed;
  const expected = binaryDrift.expected;

  const skipKey = `cognis.skipBackendUpgrade.${expected}`;
  if (extensionContext?.globalState.get<boolean>(skipKey)) {
    return;
  }
  const choice = await vscode.window.showInformationMessage(
    `Cognis was updated to ${expected}, but its backend is still ${installed}. ` +
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
 * engine binary updates via GitHub Releases, so a mismatch is a normal
 * production state that the matched-version e2e suite cannot catch. Silent when the backend can't be
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
  initManagedBinary(context, extVersion);
  initManagedModel(context, extVersion);
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

  // Register a command with partial-failure tolerance (R7.5): if one
  // registration throws (e.g. a duplicate id or a host quirk), log a
  // diagnostics warning and keep activating so the remaining commands still
  // register. Activation never aborts because a single command failed.
  const safeRegister = (
    id: string,
    handler: (...args: any[]) => any
  ): void => {
    try {
      context.subscriptions.push(vscode.commands.registerCommand(id, handler));
    } catch (err) {
      trace.warn("activate", `command ${id} failed to register: ${String(err)}`);
      // Continue activation with partial functionality.
    }
  };

  safeRegister("cognis.showOutput", () => {
    output.show(true);
  });

  safeRegister("cognis.showDiagnostics", async () => {
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
  });

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
          advancedMode: readAdvancedMode(),
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

  // React to `cognis.advancedMode` toggles by re-rendering the panel in place
  // (≤2s, no window reload). We flip `advancedMode` on the most recent context
  // and re-run `updateStatusBar`; if nothing has been rendered yet, fall back
  // to a fresh health poll. This is a plain event listener (not a command), so
  // it's pushed directly rather than via `safeRegister` (R3.3, R3.4).
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration("cognis.advancedMode")) {
        return;
      }
      const advancedMode = readAdvancedMode();
      if (lastContext) {
        updateStatusBar({ ...lastContext, advancedMode });
      } else {
        void pollHealth();
      }
    })
  );

  safeRegister("cognis.setupWorkspace", () => runSetupWorkspace());

  // Unified_Control "Start Cognis" one-click flow, registered in the same
  // partial-failure-tolerant group as every other command (R1.6, R7.3).
  safeRegister("cognis.startCognis", () => runStartCognis());

  safeRegister("cognis.repairSetup", () => runRepairSetup());

  safeRegister("cognis.clearAndReindex", () => runClearAndReindex());

  safeRegister("cognis.connectMcp", () => runConnectMcp());

  safeRegister("cognis.disconnectMcp", () => runDisconnectMcp());

  safeRegister("cognis.cancelIndexing", () => runCancelIndexing());

  safeRegister("cognis.pauseSync", () => runPauseSync());

  safeRegister("cognis.resumeSync", () => runResumeSync());

  safeRegister("cognis.enterLicense", () => enterLicenseKey(context));

  safeRegister("cognis.installBackend", () => runInstallBackend());

  safeRegister("cognis.removeFromWorkspace", () =>
    runRemoveFromWorkspace("workspace")
  );

  safeRegister("cognis.prepareUninstall", () => runRemoveFromWorkspace("all"));

  safeRegister("cognis.reinstallEngine", () => runReinstallEngine());

  safeRegister("cognis.uninstallEngine", () => runUninstallEngine());

  safeRegister("cognis.coldRestart", () => runColdRestart());

  safeRegister("cognis.showHealth", async () => {
    try {
      await showHealthReport();
    } catch (err) {
      await showErrorGuidance(err, "Health report");
    }
  });

  safeRegister("cognis.startMcpServer", () => runStartMcpServer());

  safeRegister("cognis.stopMcpServer", () => runStopMcpServer());

  context.subscriptions.push(
    onDidChangeMcpServerState(({ repoRoot }) => {
      const folder = getWorkspaceFolder();
      if (folder && folder.uri.fsPath === repoRoot) {
        void pollHealth();
      }
    })
  );

  safeRegister("cognis.openPanel", () => {
    panelProvider.reveal();
  });

  safeRegister("cognis.refreshPrerequisites", () => refreshPrerequisites());

  safeRegister("cognis.installPrerequisite", async (itemId?: string) => {
    const folder = getWorkspaceFolder();
    if (!folder || !itemId) {
      return;
    }
    const item = lastPrerequisites?.items.find((i) => i.id === itemId);
    if (!item) {
      return;
    }
    installPrerequisite(folder.uri.fsPath, item.install_target);
  });

  safeRegister("cognis.installAllPrerequisites", () => {
    const folder = getWorkspaceFolder();
    if (!folder || !lastPrerequisites) {
      return;
    }
    installAllMissing(folder.uri.fsPath, lastPrerequisites.combined_install_target);
  });

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
          advancedMode: readAdvancedMode(),
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

export async function deactivate(): Promise<void> {
  stopAllIndexing();
  // Await (not `void`) so cleanup confirms every MCP server has stopped within
  // its budget before the host finishes tearing the extension down (R10.2, R13.2).
  await stopAllMcpServers();
  if (healthPollTimer) {
    clearInterval(healthPollTimer);
  }
}
