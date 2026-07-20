import { execFileSync, spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { runTests } from "@vscode/test-electron";

const REQUIRED_FILES = [
  "crates/cognis-store/src/lib.rs",
  "crates/cognis-indexer/src/pipeline.rs",
  "crates/cognis-core/src/warm_policy.rs",
  "apps/cognis-vscode/src/extension.ts",
  "apps/cognis-vscode/src/panel.ts",
] as const;

function buildCognisBinary(repoRoot: string): string {
  const exe = process.platform === "win32" ? "cognis.exe" : "cognis";
  const built = path.join(repoRoot, "target", "debug", exe);
  console.log("[large-host] building real engine: cargo build -p cognis");
  const result = spawnSync("cargo", ["build", "-p", "cognis"], {
    cwd: repoRoot,
    stdio: "inherit",
    env: {
      ...scrubbedEnv(process.env),
      CARGO_NET_OFFLINE: "true",
    },
  });
  if (result.error || result.status !== 0 || !fs.existsSync(built)) {
    throw new Error(
      `cargo build -p cognis failed (status=${result.status}, error=${String(
        result.error ?? "none"
      )}); the practical large-host gate never skips its backend`
    );
  }
  return fs.realpathSync(built);
}

function scrubbedEnv(
  source: NodeJS.ProcessEnv
): Record<string, string | undefined> {
  const env = { ...source };
  delete env.COGNIS_DB_PATH;
  delete env.COGNIS_MCP_FIXTURE;
  delete env.COGNIS_INDEXD_STATUS_PATH;
  delete env.COGNIS_REPO_ROOT;
  delete env.COGNIS_ONNX_MODEL_DIR;
  return env;
}

function gitLines(repoRoot: string, args: string[]): string[] {
  return execFileSync("git", args, {
    cwd: repoRoot,
    encoding: "utf8",
    env: scrubbedEnv(process.env),
  })
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter(Boolean);
}

function trackedProductionFiles(repoRoot: string): string[] {
  const candidates = gitLines(repoRoot, [
    "ls-files",
    "--",
    "crates/*/src/*.rs",
    "crates/*/src/**/*.rs",
    "bins/*/src/*.rs",
    "bins/*/src/**/*.rs",
    "apps/cognis-vscode/src/*.ts",
  ]);
  const files = candidates.filter((rel) => {
    const normalized = rel.replace(/\\/gu, "/");
    return (
      !normalized.includes("/test/") &&
      !normalized.includes("/test-host/") &&
      !normalized.includes("/test-host-large/") &&
      !normalized.includes("/sim/") &&
      !normalized.includes("node_modules") &&
      !normalized.includes("/.cognis/") &&
      !normalized.includes("/.benchmarks/") &&
      !normalized.includes("/target/") &&
      !normalized.includes("/out/")
    );
  });

  for (const required of REQUIRED_FILES) {
    if (!files.includes(required)) {
      throw new Error(`tracked large-corpus allowlist is missing ${required}`);
    }
  }
  if (files.length < 100) {
    throw new Error(
      `large corpus unexpectedly shrank to ${files.length} files (minimum 100)`
    );
  }
  return files.sort();
}

function copyTrackedCorpus(
  repoRoot: string,
  workspace: string
): { files: number; bytes: number } {
  let bytes = 0;
  const files = trackedProductionFiles(repoRoot);
  for (const rel of files) {
    const source = path.join(repoRoot, ...rel.split("/"));
    const destination = path.join(workspace, ...rel.split("/"));
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.copyFileSync(source, destination);
    bytes += fs.statSync(destination).size;
  }
  return { files: files.length, bytes };
}
function vscodeExecutable(): string {
  const override = process.env.COGNIS_VSCODE_EXECUTABLE?.trim();
  const candidates = [
    override,
    process.platform === "win32"
      ? path.join(
          process.env.LOCALAPPDATA ?? "",
          "Programs",
          "Microsoft VS Code",
          "Code.exe"
        )
      : undefined,
    process.platform === "darwin"
      ? "/Applications/Visual Studio Code.app/Contents/MacOS/Electron"
      : undefined,
    process.platform === "linux" ? "/usr/bin/code" : undefined,
    process.platform === "linux" ? "/usr/share/code/code" : undefined,
  ].filter((value): value is string => Boolean(value));

  const executable = candidates.find((candidate) => fs.existsSync(candidate));
  if (!executable) {
    throw new Error(
      "No local VS Code executable found. This no-network suite will not download one; " +
        "set COGNIS_VSCODE_EXECUTABLE to an existing VS Code executable."
    );
  }
  return fs.realpathSync(executable);
}

function readStatusPid(workspace: string): number | undefined {
  const statusFile = path.join(workspace, ".cognis", "indexd-status.json");
  try {
    const value = JSON.parse(fs.readFileSync(statusFile, "utf8")) as {
      pid?: unknown;
    };
    return typeof value.pid === "number" && value.pid > 0
      ? value.pid
      : undefined;
  } catch {
    return undefined;
  }
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function processCommandLine(pid: number): string | undefined {
  if (process.platform === "linux") {
    try {
      return fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").replace(/\0/gu, " ");
    } catch {
      return undefined;
    }
  }
  const result =
    process.platform === "win32"
      ? spawnSync(
          "powershell",
          [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            `(Get-CimInstance Win32_Process -Filter \"ProcessId = ${pid}\").CommandLine`,
          ],
          { encoding: "utf8" }
        )
      : spawnSync("ps", ["-p", String(pid), "-o", "command="], {
          encoding: "utf8",
        });
  return result.status === 0 ? result.stdout.trim() : undefined;
}

function ownsIndexdProcess(
  pid: number,
  binaryPath: string,
  workspace: string
): boolean {
  const command = processCommandLine(pid)?.replace(/\\/gu, "/").toLowerCase();
  const binary = binaryPath.replace(/\\/gu, "/").toLowerCase();
  const root = workspace.replace(/\\/gu, "/").toLowerCase();
  return Boolean(command?.includes(binary) && command.includes("indexd") && command.includes(root));
}

function sleepSync(ms: number): void {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function killProcessTree(pid: number, binaryPath: string, workspace: string): void {
  if (!isAlive(pid)) {
    return;
  }
  if (!ownsIndexdProcess(pid, binaryPath, workspace)) {
    throw new Error(
      `refusing to kill live pid ${pid}: command line does not identify this sandbox's indexd`
    );
  }
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"], {
      stdio: "ignore",
    });
  } else {
    process.kill(pid, "SIGTERM");
  }
  for (let attempt = 0; attempt < 50 && isAlive(pid); attempt += 1) {
    sleepSync(100);
  }
  if (isAlive(pid) && process.platform !== "win32") {
    process.kill(pid, "SIGKILL");
  }
}

function printDiagnostics(diagnosticsDir: string): void {
  const file = path.join(diagnosticsDir, "diagnostics.jsonl");
  console.log(`\n===== large-host diagnostics (${file}) =====`);
  if (fs.existsSync(file)) {
    console.log(fs.readFileSync(file, "utf8"));
  } else {
    console.log("[large-host] diagnostics.jsonl was not created");
  }
  console.log("===== end large-host diagnostics =====\n");
}

async function main(): Promise<void> {
  const extensionDevelopmentPath = path.resolve(__dirname, "..", "..");
  const extensionTestsPath = path.resolve(__dirname, "index.js");
  const repoRoot = path.resolve(__dirname, "..", "..", "..", "..");
  const started = Date.now();
  const binaryPath = buildCognisBinary(repoRoot);
  const codePath = vscodeExecutable();
  const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-host-large-"));
  const workspace = path.join(sandbox, "workspace");
  const userDataDir = path.join(sandbox, "vscode-user-data");
  const extensionsDir = path.join(sandbox, "vscode-extensions");
  const diagnosticsDir = path.join(sandbox, "diagnostics");
  const homeDir = path.join(sandbox, "home");
  const xdgConfigDir = path.join(sandbox, "xdg-config");
  const xdgDataDir = path.join(sandbox, "xdg-data");
  let failure: unknown;
  let corpus = { files: 0, bytes: 0 };
  try {
    for (const dir of [
      workspace,
      userDataDir,
      extensionsDir,
      diagnosticsDir,
      homeDir,
      xdgConfigDir,
      xdgDataDir,
    ]) {
      fs.mkdirSync(dir, { recursive: true });
    }

    corpus = copyTrackedCorpus(repoRoot, workspace);
    fs.mkdirSync(path.join(workspace, ".vscode"), { recursive: true });
    fs.writeFileSync(
      path.join(workspace, ".vscode", "settings.json"),
      JSON.stringify(
        {
          "cognis.autoManageOnActivate": false,
          "cognis.autoStartLiveIndexing": false,
          "cognis.autoIndexOnFileChange": false,
          "cognis.mcpHost": "cursor",
          "cognis.mcpConfigScope": "workspace",
          "cognis.mcpWarmSemanticOnStartup": false,
        },
        null,
        2
      ) + "\n",
      "utf8"
    );

    console.log(`[large-host] sandbox: ${sandbox}`);
    console.log(
      `[large-host] copied ${corpus.files} tracked production files (${corpus.bytes} bytes)`
    );
    console.log(`[large-host] VS Code: ${codePath}`);
    console.log(
      "[large-host] mode: lexical/structural only; semantic is out of scope"
    );
    const env = scrubbedEnv(process.env);
    await runTests({
      vscodeExecutablePath: codePath,
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        workspace,
        "--disable-extensions",
        "--user-data-dir",
        userDataDir,
        "--extensions-dir",
        extensionsDir,
        "--disable-workspace-trust",
        "--disable-telemetry",
      ],
      extensionTestsEnv: {
        ...env,
        HOME: homeDir,
        USERPROFILE: homeDir,
        XDG_CONFIG_HOME: xdgConfigDir,
        XDG_DATA_HOME: xdgDataDir,
        COGNIS_BINARY_PATH: binaryPath,
        COGNIS_DIAGNOSTICS_DIR: diagnosticsDir,
        COGNIS_HOST_LARGE_WORKSPACE: workspace,
        COGNIS_HOST_LARGE_EXPECTED_FILES: String(corpus.files),
        COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
      },
    });
  } catch (err) {
    failure = err;
    process.exitCode = 1;
  } finally {
    try {
      printDiagnostics(diagnosticsDir);
    } catch (err) {
      failure = failure ?? err;
      process.exitCode = 1;
      console.error("[large-host] could not print diagnostics:", err);
    }
    const pid = readStatusPid(workspace);
    try {
      if (pid !== undefined) {
        killProcessTree(pid, binaryPath, workspace);
      }
    } catch (err) {
      failure = failure ?? err;
      process.exitCode = 1;
    }
    const orphaned = pid !== undefined && isAlive(pid);
    if (orphaned) {
      failure = failure ?? new Error(`indexd pid ${pid} survived runner cleanup`);
      process.exitCode = 1;
    }
    console.log(
      `[large-host] elapsed: ${((Date.now() - started) / 1000).toFixed(1)}s`
    );
    console.log(`[large-host] indexd cleanup: ${orphaned ? "FAILED" : "ok"}`);
    try {
      fs.rmSync(sandbox, { recursive: true, force: true, maxRetries: 5 });
    } catch (err) {
      failure = failure ?? err;
      process.exitCode = 1;
    }
    console.log(
      `[large-host] sandbox cleanup: ${fs.existsSync(sandbox) ? "FAILED" : "ok"}`
    );
  }

  if (failure) {
    console.error("Large-codebase host e2e failed:", failure);
  }
}

void main().catch((err) => {
  console.error("Large-codebase host runner failed:", err);
  process.exitCode = 1;
});
