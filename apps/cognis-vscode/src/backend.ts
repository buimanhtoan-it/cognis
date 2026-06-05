import { spawn } from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import { setManagedPythonPath } from "./python";

/**
 * Managed Python backend lifecycle.
 *
 * To make "install backend" and "uninstall backend" one-click and *safe*, the
 * extension installs the Cognis Python package into a virtual environment it
 * owns, under the extension's global storage. Because we created that folder,
 * we can delete it on uninstall without any risk of touching the user's system
 * or shared Python. Users who prefer their own environment can still set
 * ``cognis.pythonPath`` — in that case we install/remove only the cognis
 * package there and never delete the environment.
 */

const PYTHON_MIN = [3, 11] as const;
const DEFAULT_PACKAGE_SPEC = "cognis-engine[indexer,embed-local,vector,tokenizers,mcp]";

let managedRootDir: string | undefined;

/** The folder that holds the managed venv (``<globalStorage>/backend``). */
export function managedBackendDir(): string | undefined {
  return managedRootDir ? path.join(managedRootDir, "backend") : undefined;
}

/** Path to the python executable inside a venv directory, per platform. */
export function venvPythonPath(venvDir: string): string {
  return process.platform === "win32"
    ? path.join(venvDir, "Scripts", "python.exe")
    : path.join(venvDir, "bin", "python");
}

/** Path to the managed venv's python, or undefined before init. */
export function managedPythonPath(): string | undefined {
  const dir = managedBackendDir();
  return dir ? venvPythonPath(dir) : undefined;
}

/** True when the managed venv python actually exists on disk. */
export function isManagedBackendInstalled(): boolean {
  const exe = managedPythonPath();
  return Boolean(exe && fs.existsSync(exe));
}

/**
 * Wire up the managed backend at activation: remember the storage root and
 * register the managed python path with the resolver so every CLI call uses it
 * automatically (unless the user set their own ``cognis.pythonPath``).
 */
export function initManagedBackend(context: vscode.ExtensionContext): void {
  managedRootDir = context.globalStorageUri.fsPath;
  setManagedPythonPath(managedPythonPath());
}

/** Parse "3.11" / "3.11.4" / version_info tuple text into [major, minor]. */
export function parsePythonVersion(output: string): [number, number] | undefined {
  const match = output.match(/(\d+)\s*[.,]\s*(\d+)/);
  if (!match) {
    return undefined;
  }
  return [Number(match[1]), Number(match[2])];
}

export function isVersionAtLeast(
  version: [number, number],
  min: readonly [number, number]
): boolean {
  if (version[0] !== min[0]) {
    return version[0] > min[0];
  }
  return version[1] >= min[1];
}

interface ProcResult {
  code: number;
  stdout: string;
  stderr: string;
}

function runProcess(
  command: string,
  args: string[],
  token?: vscode.CancellationToken
): Promise<ProcResult> {
  const channel = getOutputChannel();
  channel.appendLine(`$ ${command} ${args.join(" ")}`);
  return new Promise((resolve) => {
    const proc = spawn(command, args, { env: process.env });
    let stdout = "";
    let stderr = "";
    const cancelSub = token?.onCancellationRequested(() => {
      proc.kill();
    });
    proc.stdout.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stdout += text;
      channel.append(text);
    });
    proc.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stderr += text;
      channel.append(text);
    });
    proc.on("close", (code) => {
      cancelSub?.dispose();
      resolve({ code: code ?? 1, stdout, stderr });
    });
    proc.on("error", (err) => {
      cancelSub?.dispose();
      resolve({ code: 1, stdout, stderr: `${stderr}\n${err.message}` });
    });
  });
}

/** Candidate commands to bootstrap a venv from, in priority order. */
function basePythonCandidates(): string[] {
  if (process.platform === "win32") {
    return ["python", "py -3", "python3"];
  }
  return ["python3", "python"];
}

/**
 * Find a base Python (>= 3.11) usable to create the managed venv. Returns the
 * command plus optional leading args (e.g. ``py -3``), or undefined if none of
 * the candidates are present and new enough.
 */
async function findBasePython(): Promise<{ cmd: string; args: string[] } | undefined> {
  for (const candidate of basePythonCandidates()) {
    const [cmd, ...prefix] = candidate.split(" ");
    const result = await runProcess(cmd, [
      ...prefix,
      "-c",
      "import sys;print('%d.%d'%sys.version_info[:2])",
    ]);
    if (result.code !== 0) {
      continue;
    }
    const version = parsePythonVersion(result.stdout);
    if (version && isVersionAtLeast(version, PYTHON_MIN)) {
      return { cmd, args: prefix };
    }
  }
  return undefined;
}

function packageSpec(): string {
  return vscode.workspace
    .getConfiguration("cognis")
    .get<string>("backendPackageSpec", DEFAULT_PACKAGE_SPEC)
    .trim() || DEFAULT_PACKAGE_SPEC;
}

export class BackendInstallError extends Error {
  readonly userMessage: string;
  readonly canInstallPython: boolean;
  constructor(userMessage: string, options?: { canInstallPython?: boolean }) {
    super(userMessage);
    this.name = "BackendInstallError";
    this.userMessage = userMessage;
    this.canInstallPython = options?.canInstallPython ?? false;
  }
}

export interface InstallOutcome {
  /** "managed" = installed into our venv; "byo" = into user's own python. */
  mode: "managed" | "byo";
  pythonPath: string;
}

/**
 * Install the Cognis backend with no manual steps.
 *
 * - If the user set ``cognis.pythonPath`` (bring-your-own), install the package
 *   into that environment.
 * - Otherwise create/refresh the managed venv and install there.
 */
export async function installManagedBackend(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken,
  options?: { userPythonPath?: string }
): Promise<InstallOutcome> {
  const spec = packageSpec();
  const byoPython = options?.userPythonPath?.trim();

  let targetPython: string;
  let mode: "managed" | "byo";

  if (byoPython) {
    mode = "byo";
    targetPython = byoPython;
  } else {
    mode = "managed";
    const venvDir = managedBackendDir();
    if (!venvDir) {
      throw new BackendInstallError(
        "Cognis storage is not ready yet. Reload the window and try again."
      );
    }
    targetPython = venvPythonPath(venvDir);
    if (!fs.existsSync(targetPython)) {
      progress.report({ message: "Finding a Python to build the backend…" });
      const base = await findBasePython();
      if (!base) {
        throw new BackendInstallError(
          "Cognis needs Python 3.11 or newer to install its backend, and none was found. " +
            "Install Python from python.org (keep “Add Python to PATH” checked on Windows), then try again.",
          { canInstallPython: true }
        );
      }
      if (token.isCancellationRequested) {
        throw new BackendInstallError("Install cancelled.");
      }
      progress.report({ message: "Creating the Cognis backend environment…" });
      fs.mkdirSync(venvDir, { recursive: true });
      const venv = await runProcess(
        base.cmd,
        [...base.args, "-m", "venv", venvDir],
        token
      );
      if (venv.code !== 0) {
        throw new BackendInstallError(
          "Could not create the backend environment. See the Cognis output log for details."
        );
      }
    }
  }

  if (token.isCancellationRequested) {
    throw new BackendInstallError("Install cancelled.");
  }

  progress.report({ message: "Upgrading pip…" });
  await runProcess(targetPython, ["-m", "pip", "install", "--upgrade", "pip"], token);

  if (token.isCancellationRequested) {
    throw new BackendInstallError("Install cancelled.");
  }

  progress.report({
    message: "Installing the Cognis backend (this can take a few minutes)…",
  });
  const install = await runProcess(
    targetPython,
    ["-m", "pip", "install", "--upgrade", spec],
    token
  );
  if (token.isCancellationRequested) {
    throw new BackendInstallError("Install cancelled.");
  }
  if (install.code !== 0) {
    throw new BackendInstallError(
      "Backend install failed. Open the Cognis output log to see the pip error."
    );
  }

  if (mode === "managed") {
    setManagedPythonPath(targetPython);
  }
  return { mode, pythonPath: targetPython };
}

export interface UninstallOutcome {
  /** "managed-deleted" removed our venv folder; "byo-package" pip-removed the
   *  cognis package from the user's env; "none" found nothing to do. */
  mode: "managed-deleted" | "byo-package" | "none";
  detail: string;
}

/**
 * Remove the backend automatically and safely.
 *
 * - Managed venv → delete the folder we created (full, clean removal).
 * - Bring-your-own python → ``pip uninstall -y cognis`` (removes only the
 *   package, never the user's environment).
 */
export async function uninstallManagedBackend(options?: {
  userPythonPath?: string;
}): Promise<UninstallOutcome> {
  const channel = getOutputChannel();
  if (isManagedBackendInstalled()) {
    const venvDir = managedBackendDir()!;
    try {
      fs.rmSync(venvDir, { recursive: true, force: true });
    } catch (err) {
      channel.appendLine(
        `[backend] delete venv failed: ${err instanceof Error ? err.message : String(err)}`
      );
      throw err;
    }
    setManagedPythonPath(managedPythonPath());
    return {
      mode: "managed-deleted",
      detail: `Deleted the managed backend at ${venvDir}.`,
    };
  }

  const byoPython = options?.userPythonPath?.trim();
  if (byoPython && fs.existsSync(byoPython)) {
    const result = await runProcess(byoPython, [
      "-m",
      "pip",
      "uninstall",
      "-y",
      "cognis",
    ]);
    if (result.code === 0) {
      return {
        mode: "byo-package",
        detail:
          "Removed the cognis package from your Python environment (the environment itself was kept).",
      };
    }
    channel.appendLine(`[backend] pip uninstall exited ${result.code}`);
  }
  return { mode: "none", detail: "No Cognis backend was installed by the extension." };
}
