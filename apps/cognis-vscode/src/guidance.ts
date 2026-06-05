import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import type { HealthReport, SetupResult } from "./types";
import { resolvePythonExecutable } from "./python";

export type GuidanceSeverity = "error" | "warning" | "info";

export interface GuidanceAction {
  label: string;
  command: string;
}

export interface UserGuidance {
  title: string;
  message: string;
  severity: GuidanceSeverity;
  actions: GuidanceAction[];
  technicalDetail?: string;
}

export class CognisGuidanceError extends Error {
  readonly guidance: UserGuidance;

  constructor(guidance: UserGuidance) {
    super(guidance.message);
    this.name = "CognisGuidanceError";
    this.guidance = guidance;
  }
}

const OUTPUT_ACTION: GuidanceAction = {
  label: "Show Output",
  command: "cognis.showOutput",
};

const REPAIR_ACTION: GuidanceAction = {
  label: "Repair Setup",
  command: "cognis.repairSetup",
};

const INSTALL_BACKEND_ACTION: GuidanceAction = {
  label: "Install backend",
  command: "cognis.installBackend",
};

const SETUP_ACTION: GuidanceAction = {
  label: "Set Up for AI",
  command: "cognis.setupForAi",
};

const HEALTH_ACTION: GuidanceAction = {
  label: "Show Health",
  command: "cognis.showHealth",
};

export function pythonMissingGuidance(pythonPath: string): UserGuidance {
  return {
    title: "Cognis backend not ready",
    message:
      "Cognis couldn't start its backend. Click Install backend and Cognis will set it up for you automatically.",
    severity: "error",
    actions: [INSTALL_BACKEND_ACTION, OUTPUT_ACTION],
    technicalDetail: `Python executable: ${pythonPath}`,
  };
}

export function cognisNotInstalledGuidance(pythonPath: string): UserGuidance {
  return {
    title: "Cognis backend not installed",
    message:
      "The Cognis backend isn't installed yet. Click Install backend and Cognis will set it up for you — no terminal needed.",
    severity: "error",
    actions: [INSTALL_BACKEND_ACTION, OUTPUT_ACTION],
    technicalDetail: `Backend Python: ${pythonPath}`,
  };
}

export function pythonMisconfiguredGuidance(
  pythonPath: string,
  detail: string
): UserGuidance {
  return {
    title: "Cognis backend not ready",
    message:
      "Cognis couldn't use its backend. Reinstall it in one click, or run Troubleshoot if the problem continues.",
    severity: "error",
    actions: [INSTALL_BACKEND_ACTION, REPAIR_ACTION, OUTPUT_ACTION],
    technicalDetail: `Python: ${pythonPath}\n${detail.trim()}`,
  };
}

export function bootstrapFailedGuidance(detail: string): UserGuidance {
  return {
    title: "Bootstrap failed",
    message:
      "Workspace bootstrap did not finish. Check the Cognis output log, fix Python or disk issues, then run Repair Setup.",
    severity: "error",
    actions: [REPAIR_ACTION, HEALTH_ACTION, OUTPUT_ACTION],
    technicalDetail: detail.trim(),
  };
}

export function mcpWriteFailedGuidance(detail: string): UserGuidance {
  return {
    title: "MCP config not written",
    message:
      "Cognis could not update your MCP client config. Check file permissions for your MCP settings file, then run Repair Setup.",
    severity: "error",
    actions: [REPAIR_ACTION, OUTPUT_ACTION],
    technicalDetail: detail.trim(),
  };
}

export function mcpReloadRequiredGuidance(configPath: string): UserGuidance {
  return {
    title: "Reload MCP host",
    message:
      "MCP config was written. Reload your editor or MCP host so Cognis tools appear.",
    severity: "info",
    actions: [HEALTH_ACTION, OUTPUT_ACTION],
    technicalDetail: `Config path: ${configPath}`,
  };
}

export function liveIndexingFailedGuidance(detail: string): UserGuidance {
  return {
    title: "Live indexing failed",
    message:
      "Cognis could not start live indexing. Run Repair Setup after bootstrap succeeds so Cognis can restore the workspace through the normal managed flow.",
    severity: "warning",
    actions: [REPAIR_ACTION, OUTPUT_ACTION],
    technicalDetail: detail.trim(),
  };
}

export function healthDegradedGuidance(report: HealthReport): UserGuidance {
  const failed = Object.entries(report.checks)
    .filter(([, check]) => check.status === "fail")
    .map(([name]) => name);
  const warned = Object.entries(report.checks)
    .filter(([, check]) => check.status === "warn")
    .map(([name]) => name);

  let message =
    "Cognis health is degraded. Run Repair Setup to restore AI-ready state.";
  if (failed.includes("index") || failed.includes("version")) {
    message =
      "The code index is missing or out of date. Run Repair Setup to rebuild bootstrap and indexing.";
  } else if (failed.length > 0) {
    message = `Health checks failed (${failed.join(", ")}). Run Repair Setup after fixing the reported issue.`;
  } else if (warned.length === 1 && warned[0] === "config") {
    message =
      "Workspace config defaults are stale. Run Repair Setup to refresh Cognis config and MCP wiring.";
  } else if (warned.length > 0) {
    message = `Cognis is partially ready (${warned.join(", ")} warnings). Review health details or run Repair Setup.`;
  }

  return {
    title: "Health degraded",
    message,
    severity: report.overall === "fail" ? "error" : "warning",
    actions: [REPAIR_ACTION, HEALTH_ACTION, OUTPUT_ACTION],
    technicalDetail: formatHealthChecks(report),
  };
}

export function noWorkspaceGuidance(): UserGuidance {
  return {
    title: "No workspace",
    message: "Open a folder in the editor before running Cognis setup or repair.",
    severity: "error",
    actions: [],
  };
}

export function cancelledGuidance(): UserGuidance {
  return {
    title: "Cancelled",
    message: "Cognis setup was cancelled. Run Set Up for AI or Repair Setup when you are ready to continue.",
    severity: "info",
    actions: [SETUP_ACTION, REPAIR_ACTION],
  };
}

function formatHealthChecks(report: HealthReport): string {
  const lines = [`Overall: ${report.overall}`];
  for (const [name, check] of Object.entries(report.checks)) {
    lines.push(`${name}: ${check.status} — ${check.message}`);
  }
  return lines.join("\n");
}

function normalizeErrorText(err: unknown): string {
  if (err instanceof CognisGuidanceError) {
    return err.guidance.technicalDetail ?? err.message;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}

export function classifyError(err: unknown, context?: string): UserGuidance {
  if (err instanceof CognisGuidanceError) {
    return err.guidance;
  }

  const text = normalizeErrorText(err).toLowerCase();
  const detail = context
    ? `${context}\n${normalizeErrorText(err)}`
    : normalizeErrorText(err);
  const python = resolvePythonExecutable();

  if (text.includes("open a workspace folder")) {
    return noWorkspaceGuidance();
  }
  if (text.includes("cancelled")) {
    return cancelledGuidance();
  }
  if (
    text.includes("enoent") ||
    text.includes("not found") ||
    text.includes("cannot find") ||
    text.includes("spawn ")
  ) {
    if (text.includes("python") || text.includes("modulenotfounderror")) {
      if (text.includes("cognis") || text.includes("no module named")) {
        return cognisNotInstalledGuidance(python);
      }
    }
    return pythonMissingGuidance(python);
  }
  if (
    text.includes("modulenotfounderror") ||
    text.includes("no module named 'cognis") ||
    text.includes("no module named \"cognis")
  ) {
    return cognisNotInstalledGuidance(python);
  }
  if (text.includes("mcp") && (text.includes("eacces") || text.includes("eperm"))) {
    return mcpWriteFailedGuidance(detail);
  }
  if (text.includes("mcp-config") || text.includes("mcp config")) {
    return mcpWriteFailedGuidance(detail);
  }
  if (text.includes("indexd") || text.includes("live indexing")) {
    return liveIndexingFailedGuidance(detail);
  }
  if (text.includes("bootstrap") || text.includes("cognis cli failed")) {
    return bootstrapFailedGuidance(detail);
  }

  return {
    title: "Cognis error",
    message:
      "Something went wrong while running Cognis. Check the output log for details, then run Repair Setup.",
    severity: "error",
    actions: [REPAIR_ACTION, OUTPUT_ACTION],
    technicalDetail: detail.trim(),
  };
}

export function setupResultGuidance(result: SetupResult): UserGuidance | undefined {
  if (
    result.health.overall === "ok" &&
    !result.mcpError &&
    result.mcpConfigPath &&
    result.liveIndexingStarted &&
    !result.liveIndexingError
  ) {
    return {
      title: "Ready for AI",
      message:
        "Cognis is ready. MCP is configured. Reload your editor or MCP host if tools do not appear yet.",
      severity: "info",
      actions: [HEALTH_ACTION, OUTPUT_ACTION],
      technicalDetail: result.mcpConfigPath
        ? `MCP config: ${result.mcpConfigPath}`
        : undefined,
    };
  }

  if (result.mcpError) {
    return mcpWriteFailedGuidance(result.mcpError);
  }
  if (result.liveIndexingError) {
    return liveIndexingFailedGuidance(result.liveIndexingError);
  }
  if (result.indexingInBackground) {
    return {
      title: "Indexing in background",
      message:
        "Cognis finished setup and started rebuilding the semantic index in the background. Semantic search will become fully ready when indexing completes.",
      severity: "info",
      actions: [HEALTH_ACTION, OUTPUT_ACTION],
      technicalDetail: result.mcpConfigPath
        ? `MCP config: ${result.mcpConfigPath}`
        : undefined,
    };
  }
  if (result.health.overall !== "ok") {
    return healthDegradedGuidance(result.health);
  }
  if (!result.mcpConfigPath) {
    return mcpWriteFailedGuidance("MCP config path was not returned.");
  }
  if (!result.liveIndexingStarted) {
    return liveIndexingFailedGuidance("Live indexing did not start.");
  }

  return undefined;
}

export async function presentGuidance(guidance: UserGuidance): Promise<void> {
  const channel = getOutputChannel();
  channel.appendLine(`[${guidance.title}] ${guidance.message}`);
  if (guidance.technicalDetail) {
    channel.appendLine(guidance.technicalDetail);
  }

  const labels = [
    ...guidance.actions.map((action) => action.label),
    OUTPUT_ACTION.label,
  ];
  const uniqueLabels = [...new Set(labels)];

  const choice =
    guidance.severity === "error"
      ? await vscode.window.showErrorMessage(guidance.message, ...uniqueLabels)
      : guidance.severity === "warning"
        ? await vscode.window.showWarningMessage(
            guidance.message,
            ...uniqueLabels
          )
        : await vscode.window.showInformationMessage(
            guidance.message,
            ...uniqueLabels
          );

  if (!choice) {
    return;
  }

  if (choice === OUTPUT_ACTION.label) {
    void vscode.commands.executeCommand(OUTPUT_ACTION.command);
    return;
  }

  const action = guidance.actions.find((item) => item.label === choice);
  if (action) {
    void vscode.commands.executeCommand(action.command);
  }
}

export async function showErrorGuidance(
  err: unknown,
  context?: string
): Promise<void> {
  await presentGuidance(classifyError(err, context));
}
