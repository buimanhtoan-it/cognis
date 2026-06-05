import * as vscode from "vscode";
import { runCliJson } from "./cli";
import { resolvePythonExecutable } from "./python";
import type { PrerequisiteReport } from "./types";

/**
 * Prerequisite checklist for the setup panel.
 *
 * The Python CLI (`cognis-cli doctor --json`) is the single source of truth for
 * which optional dependency groups (parsers, local embeddings, vector search,
 * MCP server, tokenizers) are installed. The panel renders this as a checklist
 * with per-item install buttons so a fresh user can satisfy every requirement
 * before running setup or indexing.
 */

const PIP_INSTALL_TERMINAL = "Cognis: Install";

/**
 * Fetch the prerequisite report. Returns ``undefined`` when the CLI itself
 * cannot run (e.g. Python/cognis not installed) — the caller treats that as a
 * higher-priority "fix Python first" state rather than a checklist failure.
 */
export async function fetchPrerequisites(
  repoRoot: string
): Promise<PrerequisiteReport | undefined> {
  try {
    const pythonPath = vscode.workspace
      .getConfiguration("cognis")
      .get<string>("pythonPath", "")
      .trim();
    const args = ["doctor"];
    if (pythonPath) {
      args.push("--python", pythonPath);
    }
    return await runCliJson<PrerequisiteReport>(repoRoot, args);
  } catch {
    return undefined;
  }
}

/**
 * Install a pip target (e.g. ``.[embed-local]``) in a visible terminal.
 *
 * We deliberately run this in an integrated terminal rather than capturing it
 * silently: installs can be slow (torch), may prompt, and the user benefits
 * from seeing real pip output. After it finishes the panel re-polls `doctor`.
 */
export function installPrerequisite(repoRoot: string, installTarget: string): void {
  const python = resolvePythonExecutable();
  const terminal = findOrCreateInstallTerminal(repoRoot);
  terminal.show(true);
  // Quote the target: ``.[extra]`` contains glob/bracket chars that some
  // shells expand. Single quotes on POSIX, double on Windows cmd/pwsh.
  const quoted = quoteForShell(installTarget);
  terminal.sendText(`${quoteForShell(python)} -m pip install -e ${quoted}`);
}

/** Install every missing item in one pip invocation. */
export function installAllMissing(repoRoot: string, combinedTarget: string): void {
  if (!combinedTarget) {
    return;
  }
  installPrerequisite(repoRoot, combinedTarget);
}

function findOrCreateInstallTerminal(repoRoot: string): vscode.Terminal {
  const existing = vscode.window.terminals.find(
    (t) => t.name === PIP_INSTALL_TERMINAL
  );
  if (existing) {
    return existing;
  }
  return vscode.window.createTerminal({
    name: PIP_INSTALL_TERMINAL,
    cwd: repoRoot,
  });
}

function quoteForShell(value: string): string {
  if (process.platform === "win32") {
    // PowerShell / cmd: wrap in double quotes; brackets are literal inside.
    return `"${value.replace(/"/g, '""')}"`;
  }
  // POSIX: single-quote to prevent glob expansion of ``[`` ``]``.
  return `'${value.replace(/'/g, "'\\''")}'`;
}
