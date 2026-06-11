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
const DEFAULT_PACKAGE_BASE = "cognis-engine[indexer,embed-local,vector,tokenizers,mcp]";
const DEFAULT_PACKAGE_SPEC = DEFAULT_PACKAGE_BASE;

let managedRootDir: string | undefined;
/** The extension's own version, used as the target backend version. */
let expectedBackendVersion: string | undefined;

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
 * automatically (unless the user set their own ``cognis.pythonPath``). Also
 * records the extension version so we can detect a backend that lags behind
 * after an extension update.
 */
export function initManagedBackend(
  context: vscode.ExtensionContext,
  extensionVersion?: string
): void {
  managedRootDir = context.globalStorageUri.fsPath;
  expectedBackendVersion =
    extensionVersion ??
    (context.extension?.packageJSON?.version as string | undefined);
  setManagedPythonPath(managedPythonPath());
}

/** The extension version we want the backend to match. */
export function targetBackendVersion(): string | undefined {
  return expectedBackendVersion;
}

export interface BackendDriftCheck {
  /** The currently installed backend version, if importable. */
  installed?: string;
  /** The version the extension expects (its own version). */
  expected?: string;
  /** True when the installed backend is older than the extension. */
  outdated: boolean;
}

/**
 * Detect whether the managed backend lags behind the extension after an update.
 *
 * Only applies to the *managed* environment — if the user brought their own
 * Python (``cognis.pythonPath``), we never touch it, so drift there is the
 * user's call. Returns ``outdated: false`` when there's nothing to do.
 */
export async function checkManagedBackendDrift(options?: {
  userPythonPath?: string;
}): Promise<BackendDriftCheck> {
  const expected = expectedBackendVersion;
  // BYO python or no managed install or unknown target → nothing to manage.
  if (options?.userPythonPath?.trim() || !expected || !isManagedBackendInstalled()) {
    return { expected, outdated: false };
  }
  const installed = await probeBackendVersion(managedPythonPath()!);
  if (!installed) {
    // Managed venv exists but cognis isn't importable — that's a broken install,
    // handled by the normal "reinstall backend" path, not a version drift.
    return { installed, expected, outdated: false };
  }
  return {
    installed,
    expected,
    outdated: compareVersions(installed, expected) < 0,
  };
}

/** Parse "3.11" / "3.11.4" / version_info tuple text into [major, minor]. */
export function parsePythonVersion(output: string): [number, number] | undefined {
  const match = output.match(/(\d+)\s*[.,]\s*(\d+)/);
  if (!match) {
    return undefined;
  }
  return [Number(match[1]), Number(match[2])];
}

/**
 * Compare two dotted version strings (``a.b.c``). Returns -1, 0, or 1.
 * Missing components are treated as 0, so "0.3" == "0.3.0".
 */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.split(".").map((n) => parseInt(n, 10) || 0);
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i += 1) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) {
      return diff < 0 ? -1 : 1;
    }
  }
  return 0;
}

/**
 * Ask a Python to print the installed cognis version (``cognis.__version__``).
 * Returns the version string, or undefined if the backend isn't importable.
 */
export async function probeBackendVersion(
  pythonPath: string
): Promise<string | undefined> {
  if (!pythonPath) {
    return undefined;
  }
  const result = await runProcess(pythonPath, [
    "-c",
    "import cognis,sys;sys.stdout.write(cognis.__version__)",
  ]);
  if (result.code !== 0) {
    return undefined;
  }
  const version = result.stdout.trim();
  return /^\d+\.\d+/.test(version) ? version : undefined;
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

/** Human-friendly elapsed time: "8s", "1m20s", "12m03s". */
export function formatElapsed(ms: number): string {
  const totalSec = Math.max(0, Math.round(ms / 1000));
  if (totalSec < 60) {
    return `${totalSec}s`;
  }
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}m${s.toString().padStart(2, "0")}s`;
}

/**
 * Pick the lines worth showing live during a pip install. pip (when its output
 * is piped, not a TTY) prints discrete progress lines like
 * ``Downloading torch-2.x-...whl (123.0 MB)`` and ``Installing collected
 * packages: ...`` — exactly the "what's happening now" signal we want to mirror
 * into the progress notification. Returns a trimmed line or undefined to ignore.
 */
export function pipProgressLine(raw: string): string | undefined {
  const line = raw.replace(/\r/g, "").trim();
  if (!line) {
    return undefined;
  }
  // Surface the meaningful pip phases; ignore everything else (warnings, hints).
  if (
    /^(Collecting|Downloading|Using cached|Installing collected packages|Building wheel|Preparing metadata|Getting requirements|Created wheel)\b/i.test(
      line
    )
  ) {
    // Drop the trailing hash/path noise pip sometimes appends.
    return line.length > 90 ? `${line.slice(0, 89)}…` : line;
  }
  return undefined;
}

function runProcess(
  command: string,
  args: string[],
  token?: vscode.CancellationToken,
  onLine?: (line: string) => void
): Promise<ProcResult> {
  const channel = getOutputChannel();
  channel.appendLine(`$ ${command} ${args.join(" ")}`);
  return new Promise((resolve) => {
    const proc = spawn(command, args, { env: process.env });
    let stdout = "";
    let stderr = "";
    // Buffer partial lines across chunks so onLine always gets whole lines.
    let lineBuf = "";
    const pump = (text: string) => {
      if (!onLine) {
        return;
      }
      lineBuf += text;
      const parts = lineBuf.split(/\r?\n/);
      lineBuf = parts.pop() ?? "";
      for (const part of parts) {
        onLine(part);
      }
    };
    const cancelSub = token?.onCancellationRequested(() => {
      proc.kill();
    });
    proc.stdout.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stdout += text;
      channel.append(text);
      pump(text);
    });
    proc.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stderr += text;
      channel.append(text);
      pump(text);
    });
    proc.on("close", (code) => {
      cancelSub?.dispose();
      if (onLine && lineBuf.trim()) {
        onLine(lineBuf);
      }
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

/**
 * The pip requirement Cognis installs for its backend.
 *
 * Default is **version-pinned to this extension's version** (e.g.
 * ``cognis-engine[...]==0.4.0``) so a given prebuilt build always installs the
 * matching engine — a buyer's install is deterministic and never silently picks
 * up a newer (or broken) engine from PyPI. The pin is applied to the
 * extras-bearing base spec by inserting ``==<version>`` after the extras group.
 *
 * If the user overrides ``cognis.backendPackageSpec`` in settings, we honor it
 * verbatim (power users / offline mirrors / pre-release testing).
 */
function packageSpec(): string {
  const configured = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("backendPackageSpec", "")
    .trim();
  if (configured) {
    return configured;
  }
  const v = expectedBackendVersion;
  // ``cognis-engine[extras]`` -> ``cognis-engine[extras]==<v>``. Only pin when
  // we know the version and the base ends with the extras group as expected.
  if (v && DEFAULT_PACKAGE_BASE.endsWith("]")) {
    return `${DEFAULT_PACKAGE_BASE}==${v}`;
  }
  return DEFAULT_PACKAGE_BASE;
}

export class BackendInstallError extends Error {
  readonly userMessage: string;
  readonly canInstallPython: boolean;
  /** Optional extra action label/URL the handler can surface as a button. */
  readonly actionLabel?: string;
  readonly actionUrl?: string;
  constructor(
    userMessage: string,
    options?: {
      canInstallPython?: boolean;
      actionLabel?: string;
      actionUrl?: string;
    }
  ) {
    super(userMessage);
    this.name = "BackendInstallError";
    this.userMessage = userMessage;
    this.canInstallPython = options?.canInstallPython ?? false;
    this.actionLabel = options?.actionLabel;
    this.actionUrl = options?.actionUrl;
  }
}

/**
 * Turn a failed ``pip install`` into a *specific, actionable* message instead of
 * a generic "see the log". Each branch tells the user the one concrete thing to
 * do. Covers the failures we expect on a fresh machine.
 */
export function classifyPipFailure(combinedOutput: string): BackendInstallError {
  const text = combinedOutput.toLowerCase();

  // Offline / DNS / proxy.
  if (
    text.includes("temporary failure in name resolution") ||
    text.includes("failed to establish a new connection") ||
    text.includes("network is unreachable") ||
    text.includes("max retries exceeded") ||
    text.includes("connection timed out") ||
    text.includes("could not fetch url")
  ) {
    return new BackendInstallError(
      "Couldn't reach PyPI to download the backend. Check your internet connection (or proxy/VPN) and try Install backend again. " +
        "Behind a corporate proxy? Set HTTPS_PROXY and reload the window.",
    );
  }

  // No matching wheel for this Python/OS (e.g. too-new Python, niche platform).
  if (
    text.includes("could not find a version that satisfies") ||
    text.includes("no matching distribution found")
  ) {
    return new BackendInstallError(
      "No prebuilt package matched your Python version or operating system. This usually means your Python is newer than the dependencies support. " +
        "Install Python 3.11 or 3.12 and set cognis.pythonPath to it, then try again.",
      { canInstallPython: true }
    );
  }

  // Build-from-source toolchain missing (C/C++ compiler).
  if (
    text.includes("microsoft visual c++ 14") ||
    text.includes("error: command 'gcc'") ||
    text.includes("error: command 'cc'") ||
    text.includes("failed building wheel") ||
    (text.includes("error: ") && text.includes("compiler"))
  ) {
    return new BackendInstallError(
      "A dependency needs to be compiled and no C/C++ build tools were found. On Windows install the “Build Tools for Visual Studio” (C++); on macOS run `xcode-select --install`. Then try Install backend again.",
      {
        actionLabel: "Get Build Tools",
        actionUrl: "https://visualstudio.microsoft.com/visual-cpp-build-tools/",
      }
    );
  }

  // Out of disk space (torch + models are large).
  if (text.includes("no space left on device") || text.includes("errno 28")) {
    return new BackendInstallError(
      "Ran out of disk space while installing the backend (the model dependencies are large — budget ~2 GB). Free up space and try Install backend again."
    );
  }

  // Permission problems writing into the environment.
  if (
    text.includes("permission denied") ||
    text.includes("errno 13") ||
    text.includes("access is denied") ||
    text.includes("winerror 5")
  ) {
    return new BackendInstallError(
      "Permission denied while writing the backend environment. Close any running Cognis processes, make sure the folder isn't read-only or synced by antivirus, then try again."
    );
  }

  // pip itself too old to resolve modern metadata.
  if (text.includes("upgrade pip") && text.includes("resolve")) {
    return new BackendInstallError(
      "pip couldn't resolve the dependencies. Try Install backend again — Cognis upgrades pip first, which usually fixes this."
    );
  }

  return new BackendInstallError(
    "Backend install failed. Open the Cognis output log to see the exact pip error, then try Install backend again."
  );
}

export interface InstallOutcome {
  /** "managed" = installed into our venv; "byo" = into user's own python. */
  mode: "managed" | "byo";
  pythonPath: string;
  /** Per-phase timing so the UI can report how long each step took. */
  timings: Array<{ phase: string; ms: number }>;
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
  const timings: Array<{ phase: string; ms: number }> = [];
  const channel = getOutputChannel();

  // Time a phase, record it, and echo the duration into the output log.
  async function phase<T>(name: string, fn: () => Promise<T>): Promise<T> {
    const started = Date.now();
    try {
      return await fn();
    } finally {
      const ms = Date.now() - started;
      timings.push({ phase: name, ms });
      channel.appendLine(`[backend] ${name} took ${formatElapsed(ms)}`);
    }
  }

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
      const base = await phase("find Python", () => findBasePython());
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
      const venv = await phase("create venv", () =>
        runProcess(base.cmd, [...base.args, "-m", "venv", venvDir], token)
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
  await phase("upgrade pip", () =>
    runProcess(targetPython, ["-m", "pip", "install", "--upgrade", "pip"], token)
  );

  if (token.isCancellationRequested) {
    throw new BackendInstallError("Install cancelled.");
  }

  // Live progress: mirror pip's discrete "Downloading/Installing …" lines into
  // the notification so the user can see exactly what's happening, plus a running
  // elapsed clock. ``--progress-bar off`` keeps pip emitting clean, parseable
  // status lines instead of carriage-return progress bars.
  const installStarted = Date.now();
  const baseMsg = "Installing the Cognis backend";
  progress.report({ message: `${baseMsg} (this can take a few minutes)…` });
  let lastLine = "";
  const ticker = setInterval(() => {
    const elapsed = formatElapsed(Date.now() - installStarted);
    const suffix = lastLine ? ` — ${lastLine}` : "…";
    progress.report({ message: `${baseMsg} [${elapsed}]${suffix}` });
  }, 1000);

  let install: ProcResult;
  try {
    install = await phase("pip install", () =>
      runProcess(
        targetPython,
        ["-m", "pip", "install", "--upgrade", "--progress-bar", "off", spec],
        token,
        (line) => {
          const friendly = pipProgressLine(line);
          if (friendly) {
            lastLine = friendly;
            const elapsed = formatElapsed(Date.now() - installStarted);
            progress.report({ message: `${baseMsg} [${elapsed}] — ${friendly}` });
          }
        }
      )
    );
  } finally {
    clearInterval(ticker);
  }

  if (token.isCancellationRequested) {
    throw new BackendInstallError("Install cancelled.");
  }
  if (install.code !== 0) {
    throw classifyPipFailure(`${install.stderr}\n${install.stdout}`);
  }

  if (mode === "managed") {
    setManagedPythonPath(targetPython);
  }
  return { mode, pythonPath: targetPython, timings };
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
