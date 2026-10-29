import * as vscode from "vscode";
import { runCliJson } from "./cli";
import type { PrerequisiteReport } from "./types";

/**
 * Prerequisite checklist for the setup panel.
 *
 * The engine ships as a single self-contained `cognis` binary (SQLite bundled,
 * ONNX assets local) — there are no separately-installable dependency groups.
 * `cognis cli doctor --json` (when available) is the source of truth for the
 * checklist; satisfying any missing item is done by installing the managed
 * binary backend, not by a package manager.
 */

/**
 * Fetch the prerequisite report. Returns ``undefined`` when the CLI itself
 * cannot run (e.g. the backend is not installed yet) — the caller treats that
 * as a higher-priority "install the backend first" state rather than a
 * checklist failure.
 */
export async function fetchPrerequisites(
  repoRoot: string
): Promise<PrerequisiteReport | undefined> {
  try {
    return await runCliJson<PrerequisiteReport>(repoRoot, ["doctor"]);
  } catch {
    return undefined;
  }
}

/**
 * Satisfy a missing prerequisite. The single-binary backend has no
 * package-manager prerequisites, so this routes to the managed binary install
 * (which downloads the self-contained `cognis` binary, checksum-verified).
 */
export function installPrerequisite(_repoRoot: string, _installTarget: string): void {
  void vscode.commands.executeCommand("cognis.installBackend");
}

/** Satisfy every missing item by installing the managed binary backend. */
export function installAllMissing(_repoRoot: string, _combinedTarget: string): void {
  void vscode.commands.executeCommand("cognis.installBackend");
}
