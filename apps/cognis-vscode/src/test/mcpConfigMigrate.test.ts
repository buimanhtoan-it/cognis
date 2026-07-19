// Harness first: installs the vscode stub before mcpConfig.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { writeJsonFile } from "../mcpConfig";
import {
  migrateGlobalEntryToWorkspace,
  planGlobalEntryToWorkspaceMigration,
} from "../mcpConfigMigrate";

// ---------------------------------------------------------------------------
// Bug facet #2 — Unsafe migration (Requirements 1.2, 2.2, 2.13; preservation
// clause 3.1).
//
// This is a BUG-CONDITION EXPLORATION test. It encodes the *expected* (fixed)
// behavior — an interrupted global→workspace move must never lose or duplicate
// a non-Cognis server (atomic + backup + rollback). On the unfixed code (a
// plain truncating `writeJsonFile` with no temp file, no fsync+rename, no
// backup, and no rollback) this FAILS: an interruption mid-move drops the
// destination write and leaves no backup to recover from. The fix provides a
// dedicated `migrateGlobalEntryToWorkspace` routine that backs up every touched
// file, writes the destination first, verifies it, removes the source only
// after verification, and rolls back from the retained backups on any failure.
// ---------------------------------------------------------------------------

function mkTempHome(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-migrate-"));
}

/** Count of occurrences of a non-Cognis server across both config files. */
function countNonCognisEntry(
  globalPath: string,
  workspacePath: string,
  serverName: string
): number {
  let count = 0;
  for (const p of [globalPath, workspacePath]) {
    if (!fs.existsSync(p)) {
      continue;
    }
    const cfg = JSON.parse(fs.readFileSync(p, "utf8")) as {
      mcpServers?: Record<string, unknown>;
    };
    if (cfg.mcpServers && serverName in cfg.mcpServers) {
      count += 1;
    }
  }
  return count;
}

test("interrupted global→workspace migration preserves the non-Cognis server exactly once", () => {
  const home = mkTempHome();
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-migrate-repo-"));
  // The repo's canonical Cognis server key + its expected repo env, so the
  // migration matches the entry the way production does (by COGNIS_DB_PATH).
  const dbPath = path.join(repoRoot, ".cognis", "uckg.db");

  const globalPath = path.join(home, ".cursor", "mcp.json");
  const workspacePath = path.join(repoRoot, ".cursor", "mcp.json");

  // A global config holding BOTH a Cognis entry (to migrate) and an unrelated
  // user-owned server that must survive byte-for-byte (preservation 3.1).
  writeJsonFile(globalPath, {
    mcpServers: {
      "cognis-myrepo-abc123": {
        command: "cognis",
        args: ["mcpd"],
        env: { COGNIS_DB_PATH: dbPath },
      },
      "brave-search": {
        command: "node",
        args: ["brave-server.js"],
        env: { BRAVE_API_KEY: "secret-value" },
      },
    },
  });

  // Run the real migration under the "cursor" host, homed at the temp dir, but
  // inject an interruption right after the destination is written and before
  // the source entry is removed — the classic non-atomic failure window.
  const outcome = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
    faultInjection: (step) => {
      if (step === "wroteDestination") {
        throw new Error("simulated interruption mid-migration");
      }
    },
  });

  // The migration reports failure and a completed rollback rather than a
  // silent partial move.
  assert.equal(outcome.ok, false, "interrupted migration must not report success");
  assert.equal(outcome.rolledBack, true, "interrupted migration must roll back");

  // EXPECTED (fixed): a timestamped byte-preserving backup of the original
  // global config exists after an interrupted migration so every entry (Cognis
  // and non-Cognis) is recoverable. Unfixed code writes no backup.
  const backupExists = fs
    .readdirSync(path.dirname(globalPath))
    .some((name) => name.startsWith("mcp.json") && name.includes("backup"));

  assert.ok(
    backupExists,
    "expected a timestamped byte-preserving backup of the global config to exist after an interrupted migration so the move is rollback-safe; unfixed code writes no backup, leaving the interrupted move unrecoverable"
  );

  // The retained backup must be byte-for-byte identical to the original global
  // config, so the pre-migration state is fully recoverable.
  const backup = outcome.backups.find((b) => b.originalPath === globalPath);
  assert.ok(backup, "expected the global config to be listed among retained backups");

  // The non-Cognis entry must still be resolvable exactly once (never lost,
  // never duplicated) — the core preservation guarantee (3.1).
  const occurrences = countNonCognisEntry(
    globalPath,
    workspacePath,
    "brave-search"
  );
  assert.equal(
    occurrences,
    1,
    `expected the non-Cognis server to appear exactly once across both configs, found ${occurrences}`
  );

  for (const dir of [home, repoRoot]) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("successful global→workspace migration moves the Cognis entry and preserves non-Cognis config", () => {
  const home = mkTempHome();
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-migrate-repo-"));
  const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
  const globalPath = path.join(home, ".cursor", "mcp.json");
  const workspacePath = path.join(repoRoot, ".cursor", "mcp.json");

  writeJsonFile(globalPath, {
    mcpServers: {
      "cognis-myrepo-abc123": {
        command: "cognis",
        args: ["mcpd"],
        env: { COGNIS_DB_PATH: dbPath },
      },
      "brave-search": {
        command: "node",
        args: ["brave-server.js"],
        env: { BRAVE_API_KEY: "secret-value" },
      },
    },
  });

  const outcome = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
  });

  assert.equal(outcome.ok, true, "clean migration must succeed");
  assert.equal(outcome.rolledBack, false);
  assert.deepEqual(outcome.movedServerNames, ["cognis-myrepo-abc123"]);

  // Cognis entry is now in the workspace config and gone from the global one.
  const globalCfg = JSON.parse(fs.readFileSync(globalPath, "utf8")) as {
    mcpServers?: Record<string, unknown>;
  };
  const wsCfg = JSON.parse(fs.readFileSync(workspacePath, "utf8")) as {
    mcpServers?: Record<string, unknown>;
  };
  assert.ok(!(globalCfg.mcpServers && "cognis-myrepo-abc123" in globalCfg.mcpServers));
  assert.ok(wsCfg.mcpServers && "cognis-myrepo-abc123" in wsCfg.mcpServers);

  // The non-Cognis server stays in the global config, exactly once, untouched.
  assert.equal(
    countNonCognisEntry(globalPath, workspacePath, "brave-search"),
    1
  );
  const brave = (globalCfg.mcpServers as Record<string, { env?: Record<string, string> }>)[
    "brave-search"
  ];
  assert.equal(brave.env?.BRAVE_API_KEY, "secret-value");

  // On verified success the backups are cleaned up by default.
  assert.equal(outcome.backups.length, 0);

  for (const dir of [home, repoRoot]) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("migration is idempotent and re-runnable after a rollback", () => {
  const home = mkTempHome();
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-migrate-repo-"));
  const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
  const globalPath = path.join(home, ".cursor", "mcp.json");

  writeJsonFile(globalPath, {
    mcpServers: {
      "cognis-myrepo-abc123": {
        command: "cognis",
        args: ["mcpd"],
        env: { COGNIS_DB_PATH: dbPath },
      },
      "brave-search": {
        command: "node",
        args: ["brave-server.js"],
        env: { BRAVE_API_KEY: "secret-value" },
      },
    },
  });

  // First attempt is interrupted and rolls back.
  const interrupted = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
    faultInjection: (step) => {
      if (step === "wroteDestination") {
        throw new Error("interrupt");
      }
    },
  });
  assert.equal(interrupted.ok, false);

  // A restart with no fault must now complete cleanly (restartable/idempotent).
  const retry = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
  });
  assert.equal(retry.ok, true);
  assert.deepEqual(retry.movedServerNames, ["cognis-myrepo-abc123"]);

  // Running yet again is a clean no-op (nothing left to move).
  const noop = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
  });
  assert.equal(noop.ok, true);
  assert.equal(noop.plan.willMoveEntry, false);
  assert.deepEqual(noop.movedServerNames, []);

  for (const dir of [home, repoRoot]) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("dry-run migration plans the move without touching any file", () => {
  const home = mkTempHome();
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-migrate-repo-"));
  const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
  const globalPath = path.join(home, ".cursor", "mcp.json");
  const workspacePath = path.join(repoRoot, ".cursor", "mcp.json");

  writeJsonFile(globalPath, {
    mcpServers: {
      "cognis-myrepo-abc123": {
        command: "cognis",
        args: ["mcpd"],
        env: { COGNIS_DB_PATH: dbPath },
      },
    },
  });

  const globalBefore = fs.readFileSync(globalPath, "utf8");

  const plan = planGlobalEntryToWorkspaceMigration(repoRoot, {
    host: "cursor",
    homeDir: home,
  });
  assert.equal(plan.willMoveEntry, true);
  assert.deepEqual(plan.serverNames, ["cognis-myrepo-abc123"]);

  const outcome = migrateGlobalEntryToWorkspace(repoRoot, {
    host: "cursor",
    homeDir: home,
    dryRun: true,
  });
  assert.equal(outcome.ok, true);
  assert.equal(outcome.dryRun, true);
  assert.equal(outcome.wroteDestination, false);

  // No file was created or modified.
  assert.equal(fs.readFileSync(globalPath, "utf8"), globalBefore);
  assert.equal(fs.existsSync(workspacePath), false);

  for (const dir of [home, repoRoot]) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
