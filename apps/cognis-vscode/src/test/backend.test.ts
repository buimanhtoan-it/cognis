// Harness first: installs the vscode stub before backend.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as path from "node:path";
import test from "node:test";

import {
  isVersionAtLeast,
  parsePythonVersion,
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

test("venvPythonPath points at the platform-correct interpreter location", () => {
  const venv = path.join("root", "backend");
  const exe = venvPythonPath(venv);
  if (process.platform === "win32") {
    assert.equal(exe, path.join(venv, "Scripts", "python.exe"));
  } else {
    assert.equal(exe, path.join(venv, "bin", "python"));
  }
});
