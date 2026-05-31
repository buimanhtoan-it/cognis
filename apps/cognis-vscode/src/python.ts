import * as vscode from "vscode";
import { runCli } from "./cli";
import {
  cognisNotInstalledGuidance,
  type UserGuidance,
  pythonMissingGuidance,
  pythonMisconfiguredGuidance,
} from "./guidance";

/** Resolve Python executable: setting > VS Code interpreter > `python`. */
export function resolvePythonExecutable(): string {
  const configured = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("pythonPath", "")
    .trim();
  if (configured) {
    return configured;
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
