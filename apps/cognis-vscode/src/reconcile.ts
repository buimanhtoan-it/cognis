import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { isLiveIndexing } from "./indexd";
import { hasExpectedMcpConfigForRepo } from "./mcpConfig";
import { verifyPythonEnvironment } from "./python";
import { isSyncPaused, setAutoManaged, setMcpEnabled } from "./state";
import {
  diagnoseRepairPlan,
  enableMcp,
  ensureWorkspaceConfigFresh,
  getWorkspaceFolder,
  isWorkspaceConfigured,
  rehydrateWorkspaceState,
  startLive,
} from "./workspace";

function mcpDismissKey(repoRoot: string): string {
  return `cognis.mcpPromptDismissed.${repoRoot}`;
}

export function isAutoManageEnabled(): boolean {
  const config = vscode.workspace.getConfiguration("cognis");
  const manageInspect = config.inspect<boolean>("autoManageOnActivate");
  if (
    manageInspect?.globalValue !== undefined ||
    manageInspect?.workspaceValue !== undefined ||
    manageInspect?.workspaceFolderValue !== undefined
  ) {
    return config.get<boolean>("autoManageOnActivate", true);
  }
  const legacyInspect = config.inspect<boolean>("autoBootstrapOnOpen");
  if (
    legacyInspect?.globalValue !== undefined ||
    legacyInspect?.workspaceValue !== undefined ||
    legacyInspect?.workspaceFolderValue !== undefined
  ) {
    return config.get<boolean>("autoBootstrapOnOpen", false);
  }
  return config.get<boolean>("autoManageOnActivate", true);
}

async function maybeEnableMcp(
  context: vscode.ExtensionContext,
  repoRoot: string
): Promise<void> {
  if (await hasExpectedMcpConfigForRepo(repoRoot)) {
    setMcpEnabled(repoRoot, true);
    return;
  }

  const config = vscode.workspace.getConfiguration("cognis");
  const promptBeforeWrite = config.get<boolean>("promptBeforeMcpWrite", true);

  if (context.globalState.get<boolean>(mcpDismissKey(repoRoot))) {
    return;
  }

  if (promptBeforeWrite) {
    const choice = await vscode.window.showInformationMessage(
      "Cognis can write MCP configuration so your AI agent can use semantic search for this workspace.",
      "Enable MCP",
      "Not Now",
      "Don't Ask Again"
    );
    if (choice === "Don't Ask Again") {
      await context.globalState.update(mcpDismissKey(repoRoot), true);
      return;
    }
    if (choice !== "Enable MCP") {
      return;
    }
  }

  const configPath = await enableMcp({ silent: true });
  getOutputChannel().appendLine(
    `[reconcile] MCP config written to ${configPath}`
  );
  vscode.window.showInformationMessage(
    `Cognis MCP config written at ${configPath}. Reload your editor or MCP host to apply.`
  );
}

export async function reconcileWorkspaceOnActivate(
  context: vscode.ExtensionContext,
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken
): Promise<void> {
  if (!isAutoManageEnabled()) {
    return;
  }

  const folder = getWorkspaceFolder();
  if (!folder) {
    return;
  }

  const repoRoot = folder.uri.fsPath;
  const channel = getOutputChannel();
  channel.appendLine("[reconcile] Inspecting workspace…");

  await rehydrateWorkspaceState();

  // Do NOT provision an unconfigured workspace on activation. Creating
  // ``.cognis/`` (config, DB, caches) is a deliberate action the user takes by
  // clicking "Set Up for AI". On activation we only *manage* a workspace that
  // is already configured — otherwise we leave the repo untouched so opening a
  // folder never writes files the user didn't ask for.
  if (!isWorkspaceConfigured(repoRoot)) {
    channel.appendLine(
      "[reconcile] Workspace not set up yet; leaving it untouched until the user runs Set Up for AI."
    );
    return;
  }

  progress.report({ message: "Checking Python environment…" });
  const pythonCheck = await verifyPythonEnvironment(repoRoot);
  if (!pythonCheck.ok) {
    channel.appendLine(`[reconcile] ${pythonCheck.guidance.message}`);
    return;
  }

  progress.report({ message: "Refreshing workspace config…" });
  await ensureWorkspaceConfigFresh(repoRoot);

  const plan = await diagnoseRepairPlan(repoRoot);
  const needsManagedRebuild =
    plan.needsBootstrap || plan.needsReindex || !plan.health;

  if (token.isCancellationRequested) {
    throw new Error("Cancelled");
  }

  const autoLive = vscode.workspace
    .getConfiguration("cognis")
    .get<boolean>("autoStartLiveIndexing", true);
  // Honor an explicit user pause: never auto-start the daemon while sync is
  // paused for this workspace, even if a rebuild would otherwise be due.
  if (isSyncPaused(repoRoot)) {
    channel.appendLine(
      "[reconcile] Index sync is paused for this workspace; skipping live indexing."
    );
  } else if (
    autoLive &&
    isWorkspaceConfigured(repoRoot) &&
    (needsManagedRebuild || !isLiveIndexing(repoRoot))
  ) {
    try {
      channel.appendLine(
        needsManagedRebuild
          ? "[reconcile] Starting managed index rebuild…"
          : "[reconcile] Starting live indexing…"
      );
      await startLive({ forceFullRebuild: needsManagedRebuild });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      channel.appendLine(`[reconcile] live indexing: ${message}`);
    }
  }

  setAutoManaged(repoRoot, true);
  await maybeEnableMcp(context, repoRoot);

  channel.appendLine("[reconcile] Done");
}
