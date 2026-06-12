/**
 * Runner for the full-stack VS Code host e2e (normal Node process).
 *
 * Downloads a VS Code build, then launches it with this extension loaded and a
 * disposable temp workspace, and runs the Mocha suite in `index.ts` inside the
 * extension host. Resources (workspace, user-data-dir, diagnostics dir) are all
 * throwaway temp dirs so a developer's real environment is never touched.
 *
 * Requires a Python with the cognis backend installed; pass it via
 * COGNIS_TEST_PYTHON (the extension is pointed at it as `cognis.pythonPath`).
 * Without it the host test skips the backend assertions.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { runTests } from "@vscode/test-electron";

async function main(): Promise<void> {
  // out/test-host -> out -> apps/cognis-vscode (the extension root w/ package.json)
  const extensionDevelopmentPath = path.resolve(__dirname, "..", "..");
  const extensionTestsPath = path.resolve(__dirname, "index.js");

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
        COGNIS_TEST_PYTHON: process.env.COGNIS_TEST_PYTHON ?? "",
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
