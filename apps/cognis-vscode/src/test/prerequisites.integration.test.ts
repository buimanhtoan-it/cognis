// Harness import first: installs the vscode + child_process stubs before any
// production module under test is required.
import {
  getSpawnRecords,
  killLiveDaemons,
  noCancelToken,
  resetHarness,
  silentProgress,
} from "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { CognisGuidanceError } from "../guidance";
import { fetchPrerequisites } from "../prerequisites";
import {
  addCognisToGitignore,
  isCognisIgnored,
  isGitRepository,
  shouldRemindGitignore,
} from "../gitignore";
import { isWorkspaceConfigured, setupForAi } from "../workspace";

function makeFreshRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-prereq-"));
}

function cleanup(repoRoot: string): void {
  killLiveDaemons();
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    // best effort
  }
}

// ---------------------------------------------------------------------------
// Prerequisite checklist
// ---------------------------------------------------------------------------

test("fetchPrerequisites returns the doctor report shape", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { prerequisitesReady: true });
  try {
    const report = await fetchPrerequisites(repoRoot);
    assert.ok(report, "expected a prerequisite report");
    assert.equal(report!.ready, true);
    assert.ok(Array.isArray(report!.items) && report!.items.length > 0);
    for (const item of report!.items) {
      assert.ok(item.id && item.label && item.install_target);
      assert.ok(item.status === "ok" || item.status === "missing");
    }
  } finally {
    cleanup(repoRoot);
  }
});

test("fetchPrerequisites surfaces a missing required item", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { prerequisitesReady: false });
  try {
    const report = await fetchPrerequisites(repoRoot);
    assert.ok(report);
    assert.equal(report!.ready, false);
    const missing = report!.items.filter(
      (i) => i.required && i.status === "missing"
    );
    assert.ok(missing.length > 0, "expected at least one missing required item");
    assert.equal(report!.combined_install_target, ".[indexer]");
  } finally {
    cleanup(repoRoot);
  }
});

// ---------------------------------------------------------------------------
// Setup is gated on prerequisites (no .cognis created when blocked)
// ---------------------------------------------------------------------------

test("setupForAi refuses and creates nothing when a required prerequisite is missing", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { prerequisitesReady: false });
  try {
    await assert.rejects(
      () => setupForAi(silentProgress(), noCancelToken()),
      (err: unknown) => {
        assert.ok(err instanceof CognisGuidanceError, "should throw guidance");
        return true;
      }
    );

    // Critically: setup must NOT have created the .cognis directory, and must
    // NOT have run `init` — a blocked setup leaves the workspace untouched.
    assert.equal(isWorkspaceConfigured(repoRoot), false);
    assert.equal(
      fs.existsSync(path.join(repoRoot, ".cognis")),
      false,
      ".cognis must not be created when prerequisites are missing"
    );
    const ranInit = getSpawnRecords().some((r) => r.args.includes("init"));
    assert.equal(ranInit, false, "init must not run when prerequisites are missing");
  } finally {
    cleanup(repoRoot);
  }
});

test("setupForAi proceeds when prerequisites are satisfied", async () => {
  const repoRoot = makeFreshRepo();
  resetHarness(repoRoot, { prerequisitesReady: true });
  try {
    const result = await setupForAi(silentProgress(), noCancelToken());
    assert.ok(result.bootstrap);
    assert.equal(isWorkspaceConfigured(repoRoot), true);
  } finally {
    cleanup(repoRoot);
  }
});

// ---------------------------------------------------------------------------
// .gitignore reminder
// ---------------------------------------------------------------------------

test("shouldRemindGitignore is true in a git repo missing the .cognis entry", () => {
  const repoRoot = makeFreshRepo();
  try {
    fs.mkdirSync(path.join(repoRoot, ".git"));
    assert.equal(isGitRepository(repoRoot), true);
    assert.equal(isCognisIgnored(repoRoot), false);
    assert.equal(shouldRemindGitignore(repoRoot), true);
  } finally {
    cleanup(repoRoot);
  }
});

test("shouldRemindGitignore is false outside a git repo", () => {
  const repoRoot = makeFreshRepo();
  try {
    assert.equal(isGitRepository(repoRoot), false);
    assert.equal(shouldRemindGitignore(repoRoot), false);
  } finally {
    cleanup(repoRoot);
  }
});

test("addCognisToGitignore adds the entry and is idempotent", () => {
  const repoRoot = makeFreshRepo();
  try {
    fs.mkdirSync(path.join(repoRoot, ".git"));
    const written = addCognisToGitignore(repoRoot);
    assert.ok(written, "should return the .gitignore path");
    assert.equal(isCognisIgnored(repoRoot), true);

    const contentAfterFirst = fs.readFileSync(written!, "utf8");
    // Re-running must not duplicate the entry.
    addCognisToGitignore(repoRoot);
    const contentAfterSecond = fs.readFileSync(written!, "utf8");
    assert.equal(contentAfterFirst, contentAfterSecond, "must be idempotent");

    const occurrences = contentAfterSecond
      .split(/\r?\n/)
      .filter((l) => l.trim().replace(/^\/+/, "").replace(/\/+$/, "") === ".cognis").length;
    assert.equal(occurrences, 1, "exactly one .cognis entry");
  } finally {
    cleanup(repoRoot);
  }
});

test("addCognisToGitignore preserves existing entries", () => {
  const repoRoot = makeFreshRepo();
  try {
    fs.mkdirSync(path.join(repoRoot, ".git"));
    const gi = path.join(repoRoot, ".gitignore");
    fs.writeFileSync(gi, "node_modules/\ndist/\n", "utf8");
    addCognisToGitignore(repoRoot);
    const content = fs.readFileSync(gi, "utf8");
    assert.ok(content.includes("node_modules/"), "existing entries preserved");
    assert.ok(content.includes("dist/"), "existing entries preserved");
    assert.ok(content.includes(".cognis/"), "new entry added");
  } finally {
    cleanup(repoRoot);
  }
});

test("isCognisIgnored recognizes common spellings", () => {
  const repoRoot = makeFreshRepo();
  try {
    const gi = path.join(repoRoot, ".gitignore");
    fs.writeFileSync(gi, "/.cognis\n", "utf8");
    assert.equal(isCognisIgnored(repoRoot), true);
  } finally {
    cleanup(repoRoot);
  }
});
