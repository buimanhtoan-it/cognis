import * as vscode from "vscode";
import { runCli } from "./cli";
import {
  cognisNotInstalledGuidance,
  type UserGuidance,
  pythonMissingGuidance,
  pythonMisconfiguredGuidance,
} from "./guidance";

/**
 * Path to the extension-managed backend Python, set by backend.ts once the
 * managed venv is known. Used as a fallback before the workspace Python so a
 * one-click install "just works" without the user choosing anything.
 */
let managedPythonPath: string | undefined;

export function setManagedPythonPath(pythonPath: string | undefined): void {
  managedPythonPath = pythonPath;
}

/**
 * Resolve the Python used to run the backend, in priority order:
 *   1. cognis.pythonPath (user explicitly chose their own environment)
 *   2. the extension-managed backend environment (one-click install)
 *   3. the editor's selected workspace Python
 *   4. the system `python` / `python3`
 */
export function resolvePythonExecutable(): string {
  const configured = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("pythonPath", "")
    .trim();
  if (configured) {
    return configured;
  }
  if (managedPythonPath) {
    return managedPythonPath;
  }
  const interp = vscode.workspace
    .getConfiguration("python")
    .get<string>("defaultInterpreterPath", "")
    .trim();
  if (interp) {
    return interp;
  }
  return process.platform === "win32" ? "python" : "python3";
}

export type PythonCheckResult =
  | { ok: true }
  | { ok: false; guidance: UserGuidance };

/** Verify Python can run the cognis CLI before setup or repair. */
export async function verifyPythonEnvironment(
  repoRoot: string
): Promise<PythonCheckResult> {
  const python = resolvePythonExecutable();
  const result = await runCli(repoRoot, ["paths"], { label: "python-check" });
  if (result.exitCode === 0) {
    return { ok: true };
  }

  const combined = `${result.stderr}\n${result.stdout}`.toLowerCase();
  const detail = result.stderr.trim() || result.stdout.trim();

  if (
    combined.includes("enoent") ||
    combined.includes("not found") ||
    combined.includes("cannot find") ||
    combined.includes("is not recognized")
  ) {
    return { ok: false, guidance: pythonMissingGuidance(python) };
  }
  if (
    combined.includes("modulenotfounderror") ||
    combined.includes("no module named 'cognis") ||
    combined.includes("no module named \"cognis")
  ) {
    return { ok: false, guidance: cognisNotInstalledGuidance(python) };
  }

  return {
    ok: false,
    guidance: pythonMisconfiguredGuidance(python, detail),
  };
}
