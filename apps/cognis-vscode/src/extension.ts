import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
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
  CognisPanelProvider,
  outcomeLabelForContext,
  type PanelContext,
} from "./panel";
import { reconcileWorkspaceOnActivate } from "./reconcile";
import {
  deriveStatus,
  getState,
  initStateStorage,
  isIndexStatusBusy,
  setIndexStatus,
  setLiveIndexing,
} from "./state";
import type { SetupResult } from "./types";
import {
  getWorkspaceFolder,
  isWorkspaceConfigured,
  refreshPanelContext,
  rehydrateWorkspaceState,
  clearIndexAndReindex,
  repairSetup,
  setupForAi,
  showHealthReport,
  startLive,
} from "./workspace";

let statusBarItem: vscode.StatusBarItem;
let panelProvider: CognisPanelProvider;
let healthPollTimer: ReturnType<typeof setInterval> | undefined;
let indexingActive = false;
let blockingIndexMessage: string | undefined;
let autoIndexStartPromise: Promise<void> | undefined;

function buildIndexingContext(repoRoot: string): PanelContext {
  const state = getState(repoRoot);
  return {
    status: "indexing",
    liveIndexing: state.liveIndexing,
    mcpEnabled: state.mcpEnabled,
    indexStatus: state.indexStatus,
    indexingMessage: blockingIndexMessage,
  };
}

async function fetchPanelContext(repoRoot: string): Promise<PanelContext> {
  return refreshPanelContext(repoRoot);
}

function updateStatusBar(context: PanelContext): void {
  statusBarItem.text = outcomeLabelForContext(context);
  statusBarItem.tooltip = "Cognis: click for indexing and AI setup status";
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
        return task(reportingProgress, token);
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

async function runSetupForAi(): Promise<void> {
  try {
    const result = await withProgress("Cognis: Set Up for AI", (p, t) =>
      setupForAi(p, t)
    );
    if (result) {
      startHealthPolling();
      await reportSetupResult(result);
    }
  } catch (err) {
    await showErrorGuidance(err, "Set Up for AI");
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

export function activate(context: vscode.ExtensionContext): void {
  initStateStorage(context);
  panelProvider = new CognisPanelProvider(context.extensionUri);
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
    vscode.commands.registerCommand("cognis.setupForAi", () => runSetupForAi())
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
    vscode.commands.registerCommand("cognis.showHealth", async () => {
      try {
        await showHealthReport();
      } catch (err) {
        await showErrorGuidance(err, "Health report");
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cognis.openPanel", () => {
      panelProvider.reveal();
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
          indexStatus: state.indexStatus,
        });
      }
    }
    await pollHealth();
    startHealthPolling();

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
    },
  });
}

export function deactivate(): void {
  stopAllIndexing();
  if (healthPollTimer) {
    clearInterval(healthPollTimer);
  }
}
