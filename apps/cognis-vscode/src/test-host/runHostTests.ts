/**
 * Runner for the full-stack VS Code host e2e (normal Node process).
 *
 * Downloads a VS Code build, then launches it with this extension loaded and a
 * disposable temp workspace, and runs the Mocha suite in `index.ts` inside the
 * extension host. Resources (workspace, user-data-dir, diagnostics dir) are all
 * throwaway temp dirs so a developer's real environment is never touched.
 *
 * The backend under test is the **pure-Rust `cognis` binary**: this runner
 * builds it (`cargo build -p cognis`) and points the extension at it via
 * `COGNIS_BINARY_PATH`, so the full-stack test drives the real engine over real
 * process boundaries — no Python. If the build is unavailable (e.g. no cargo on
 * the machine), `COGNIS_BINARY_PATH` is left empty and the host test skips its
 * backend assertions rather than failing.
 */
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { runTests } from "@vscode/test-electron";

/** Build the `cognis` binary and return its path, or "" when unavailable. */
function buildCognisBinary(repoRoot: string): string {
  const exe = process.platform === "win32" ? "cognis.exe" : "cognis";
  const built = path.join(repoRoot, "target", "debug", exe);
  console.log("[runHostTests] building cognis binary (cargo build -p cognis)…");
  const result = spawnSync("cargo", ["build", "-p", "cognis"], {
    cwd: repoRoot,
    stdio: "inherit",
  });
  if (result.status !== 0 || !fs.existsSync(built)) {
    console.warn(
      `[runHostTests] could not build the cognis binary (status=${result.status}); ` +
        "the host test will skip backend assertions."
    );
    return "";
  }
  console.log(`[runHostTests] using engine binary: ${built}`);
  return built;
}

async function main(): Promise<void> {
  // out/test-host -> out -> apps/cognis-vscode (the extension root w/ package.json)
  const extensionDevelopmentPath = path.resolve(__dirname, "..", "..");
  const extensionTestsPath = path.resolve(__dirname, "index.js");
  // apps/cognis-vscode/out/test-host -> repo root is four levels up.
  const repoRoot = path.resolve(__dirname, "..", "..", "..", "..");

  const binaryPath = buildCognisBinary(repoRoot);

  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-host-ws-"));
  fs.mkdirSync(path.join(workspace, "src"), { recursive: true });
  fs.writeFileSync(
    path.join(workspace, "src", "auth.py"),
    "def authenticate(token):\n    return verify(token)\n\n\ndef verify(token):\n    return bool(token)\n",
    "utf8"
  );
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-host-ud-"));
  const diagnosticsDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-host-diag-"));

  try {
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        workspace,
        "--disable-extensions",
        "--user-data-dir",
        userDataDir,
        "--disable-workspace-trust",
      ],
      extensionTestsEnv: {
        COGNIS_DIAGNOSTICS_DIR: diagnosticsDir,
        // Drive the real Rust engine binary (empty when the build was skipped).
        COGNIS_BINARY_PATH: binaryPath,
        COGNIS_HOST_WORKSPACE: workspace,
      },
    });
  } catch (err) {
    console.error("Full-stack host tests failed:", err);
    process.exitCode = 1;
  } finally {
    // Always surface the extension's trace so a hang/failure is diagnosable.
    const diagFile = path.join(diagnosticsDir, "diagnostics.jsonl");
    if (fs.existsSync(diagFile)) {
      console.log(`\n===== diagnostics.jsonl (${diagFile}) =====`);
      console.log(fs.readFileSync(diagFile, "utf8"));
      console.log("===== end diagnostics =====\n");
    } else {
      console.log(`\n[runHostTests] no diagnostics.jsonl at ${diagFile}`);
    }
    console.log(`[runHostTests] workspace: ${workspace}`);
  }
}

void main();
