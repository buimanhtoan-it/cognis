// Harness first: installs the vscode stub before backend.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as path from "node:path";
import test from "node:test";

import {
  buildPackageSpec,
  classifyPipFailure,
  compareVersions,
  formatElapsed,
  isVersionAtLeast,
  parsePythonVersion,
  pipProgressLine,
  venvPythonPath,
} from "../backend";

test("parsePythonVersion reads major.minor from common outputs", () => {
  assert.deepEqual(parsePythonVersion("3.11"), [3, 11]);
  assert.deepEqual(parsePythonVersion("3.11.4"), [3, 11]);
  assert.deepEqual(parsePythonVersion("Python 3.12.1"), [3, 12]);
  // version_info-style tuple text "3, 13"
  assert.deepEqual(parsePythonVersion("3, 13"), [3, 13]);
});

test("parsePythonVersion returns undefined when no version is present", () => {
  assert.equal(parsePythonVersion("no version here"), undefined);
});

test("isVersionAtLeast enforces the 3.11 minimum", () => {
  assert.equal(isVersionAtLeast([3, 11], [3, 11]), true);
  assert.equal(isVersionAtLeast([3, 12], [3, 11]), true);
  assert.equal(isVersionAtLeast([4, 0], [3, 11]), true);
  assert.equal(isVersionAtLeast([3, 10], [3, 11]), false);
  assert.equal(isVersionAtLeast([2, 7], [3, 11]), false);
});

test("compareVersions orders dotted versions and treats missing parts as 0", () => {
  assert.equal(compareVersions("0.3.0", "0.3.1"), -1);
  assert.equal(compareVersions("0.3.1", "0.3.0"), 1);
  assert.equal(compareVersions("0.3.1", "0.3.1"), 0);
  // Missing components default to 0 → "0.3" equals "0.3.0".
  assert.equal(compareVersions("0.3", "0.3.0"), 0);
  assert.equal(compareVersions("1.0.0", "0.9.9"), 1);
  assert.equal(compareVersions("0.10.0", "0.9.0"), 1);
});

test("venvPythonPath points at the platform-correct interpreter location", () => {
  const venv = path.join("root", "backend");
  const exe = venvPythonPath(venv);
  if (process.platform === "win32") {
    assert.equal(exe, path.join(venv, "Scripts", "python.exe"));
  } else {
    assert.equal(exe, path.join(venv, "bin", "python"));
  }
});

test("formatElapsed renders seconds and minutes", () => {
  assert.equal(formatElapsed(0), "0s");
  assert.equal(formatElapsed(8000), "8s");
  assert.equal(formatElapsed(59_000), "59s");
  assert.equal(formatElapsed(80_000), "1m20s");
  assert.equal(formatElapsed(723_000), "12m03s");
});

test("pipProgressLine surfaces meaningful pip phases only", () => {
  assert.equal(
    pipProgressLine("Downloading torch-2.12.0-cp314-cp314-win_amd64.whl (123.0 MB)"),
    "Downloading torch-2.12.0-cp314-cp314-win_amd64.whl (123.0 MB)"
  );
  assert.equal(
    pipProgressLine("Installing collected packages: numpy, torch"),
    "Installing collected packages: numpy, torch"
  );
  assert.equal(pipProgressLine("Using cached threadpoolctl-3.6.0-py3-none-any.whl (18 kB)"),
    "Using cached threadpoolctl-3.6.0-py3-none-any.whl (18 kB)");
  // Noise lines are ignored.
  assert.equal(pipProgressLine("WARNING: something"), undefined);
  assert.equal(pipProgressLine(""), undefined);
  assert.equal(pipProgressLine("   "), undefined);
});

test("pipProgressLine truncates very long lines", () => {
  const long = "Downloading " + "x".repeat(200) + ".whl";
  const out = pipProgressLine(long);
  assert.ok(out && out.length <= 90, "should be truncated to <= 90 chars");
  assert.ok(out!.endsWith("…"), "should end with an ellipsis");
});

test("classifyPipFailure maps offline errors to a network hint", () => {
  const err = classifyPipFailure(
    "WARNING: Retrying ... Max retries exceeded with url: /simple/torch/"
  );
  assert.match(err.userMessage, /internet connection/i);
  assert.equal(err.canInstallPython, false);
});

test("classifyPipFailure maps no-matching-wheel to a Python-version fix", () => {
  const err = classifyPipFailure(
    "ERROR: Could not find a version that satisfies the requirement torch"
  );
  assert.match(err.userMessage, /Python version or operating system/i);
  assert.equal(err.canInstallPython, true);
});

test("classifyPipFailure flags an unpublished cognis-engine version (not a Python problem)", () => {
  // Upgrading to a version whose engine isn't on PyPI yet (e.g. the tag's
  // publish is still running) must NOT tell the user to downgrade Python —
  // cognis-engine is a pure-Python wheel that works on any supported Python.
  const err = classifyPipFailure(
    "ERROR: Could not find a version that satisfies the requirement " +
      "cognis-engine[indexer,embed-local,vector,tokenizers,mcp]==0.5.3 " +
      "(from versions: 0.5.1, 0.5.2)\n" +
      "ERROR: No matching distribution found for cognis-engine[indexer]==0.5.3"
  );
  assert.match(err.userMessage, /not on PyPI yet/i);
  assert.equal(err.canInstallPython, false);
});

test("classifyPipFailure maps a missing compiler to build-tools guidance", () => {
  const err = classifyPipFailure(
    "error: Microsoft Visual C++ 14.0 or greater is required"
  );
  assert.match(err.userMessage, /build tools/i);
  assert.equal(err.actionUrl, "https://visualstudio.microsoft.com/visual-cpp-build-tools/");
});

test("classifyPipFailure maps disk-full to a free-space hint", () => {
  const err = classifyPipFailure("OSError: [Errno 28] No space left on device");
  assert.match(err.userMessage, /disk space/i);
});

test("classifyPipFailure falls back to the output-log message", () => {
  const err = classifyPipFailure("some unexpected pip explosion");
  assert.match(err.userMessage, /output log/i);
});



// --- buildPackageSpec: deterministic version pin (the upgrade-flow contract) ---

test("buildPackageSpec pins the engine to the extension version", () => {
  assert.equal(
    buildPackageSpec("cognis-engine[indexer,mcp]", "0.5.3"),
    "cognis-engine[indexer,mcp]==0.5.3"
  );
});

test("buildPackageSpec falls back to the unpinned base when version is unknown", () => {
  assert.equal(
    buildPackageSpec("cognis-engine[indexer,mcp]", undefined),
    "cognis-engine[indexer,mcp]"
  );
});

test("buildPackageSpec does not pin a base without an extras group", () => {
  // No trailing "]" → inserting ==v would be malformed, so leave it unpinned.
  assert.equal(buildPackageSpec("cognis-engine", "0.5.3"), "cognis-engine");
});

test("buildPackageSpec honors a configured override verbatim", () => {
  assert.equal(
    buildPackageSpec("cognis-engine[indexer,mcp]", "0.5.3", "  cognis-engine==0.4.0  "),
    "cognis-engine==0.4.0"
  );
});

test("buildPackageSpec ignores a blank override", () => {
  assert.equal(
    buildPackageSpec("cognis-engine[indexer,mcp]", "0.5.3", "   "),
    "cognis-engine[indexer,mcp]==0.5.3"
  );
});
