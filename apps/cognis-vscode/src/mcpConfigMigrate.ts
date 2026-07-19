import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { readJsonFile, resolveMcpHost, writeJsonFile } from "./mcpConfig";
import {
  getGlobalMcpConfigPath,
  getWorkspaceMcpConfigPath,
} from "./mcpConfigPaths";
import { envMatchesRepo } from "./mcpEnv";
import { deriveMcpServerName, isCognisMcpServerName } from "./mcpServerName";
import type { McpServerBlock } from "./types";

// ---------------------------------------------------------------------------
// Safe global → workspace MCP config migration (Requirements 2.2, 2.13;
// preservation 3.1, 3.2).
//
// Moving a Cognis entry out of a shared *global* host config
// (~/.cursor/mcp.json) into the repository-local *workspace* config
// (<repo>/.cursor/mcp.json) must never lose or corrupt unrelated user config.
// The plain `writeJsonFile` truncate offered no cross-file transaction: an
// interruption between "remove from source" and "write to destination" left an
// entry in NEITHER file (lost) or BOTH (duplicated).
//
// This routine makes the move atomic-per-file (via the temp+fsync+rename
// `writeJsonFile` primitive), backed up (timestamped byte-preserving copies of
// every touched file), verified (destination is host-visible and its repo env
// matches), and rollback-safe (any failed step restores every touched file
// from its backup and retains the backups so the interrupted move stays
// recoverable). It writes the destination FIRST and removes the source ONLY
// after the destination is verified, so a crash can never drop the entry from
// both files.
// ---------------------------------------------------------------------------

/** Ordered, named steps the migration walks through — used for auditing and
 * (in tests) deterministic fault injection. */
export type MigrationStep =
  | "planned"
  | "backedUp"
  | "wroteDestination"
  | "verifiedDestination"
  | "removedSource"
  | "verifiedSource";

/**
 * The immutable, side-effect-free description of what a migration *would* do.
 * Produced by {@link planGlobalEntryToWorkspaceMigration} and embedded in every
 * {@link MigrationOutcome} so the operation is auditable and dry-runnable.
 */
export interface MigrationPlan {
  repoRoot: string;
  host: string;
  /** Global host config the entry is moving out of. */
  sourcePath: string;
  /** Workspace config the entry is moving into. */
  destinationPath: string;
  /** Cognis server keys in the source that belong to this repository. */
  serverNames: string[];
  /** The canonical (extension-derived) server name for this repository. */
  canonicalServerName: string;
  sourceExists: boolean;
  destinationExists: boolean;
  /** True when there is at least one matching Cognis entry to move. */
  willMoveEntry: boolean;
}

/** A retained backup: the original file and its timestamped byte copy. */
export interface MigrationBackup {
  originalPath: string;
  backupPath: string;
}

/** The auditable result of running (or planning) a migration. */
export interface MigrationOutcome {
  ok: boolean;
  dryRun: boolean;
  plan: MigrationPlan;
  /** Backups retained on disk (present on failure; cleared on verified
   * success unless `retainBackups` is set). */
  backups: MigrationBackup[];
  movedServerNames: string[];
  wroteDestination: boolean;
  removedFromSource: boolean;
  rolledBack: boolean;
  /** Ordered human-readable audit trail of every step taken. */
  steps: string[];
  error?: string;
}

export interface MigrateOptions {
  /** MCP host; defaults to the resolved host from settings. */
  host?: string;
  /** Home directory for resolving the global config; defaults to os.homedir(). */
  homeDir?: string;
  /** When true, compute and return the plan without touching any file. */
  dryRun?: boolean;
  /** Keep backups even after a verified success (default: remove on success). */
  retainBackups?: boolean;
  /** Clock source for backup timestamps (test seam). */
  now?: () => number;
  /**
   * Test-only seam: invoked after each {@link MigrationStep}. Throwing from it
   * simulates a crash/interruption at that point so rollback + backup-retention
   * can be exercised deterministically.
   */
  faultInjection?: (step: MigrationStep) => void;
}

/** In-process guard so two concurrent migrations of the same file pair
 * serialize rather than racing on the same backups/temp files. */
const migrationLocks = new Set<string>();

function toServerMap(
  cfg: Record<string, unknown>
): Record<string, McpServerBlock> {
  return (cfg.mcpServers as Record<string, McpServerBlock> | undefined) ?? {};
}

/**
 * Which Cognis server keys in `sourcePath` belong to `repoRoot` — matched by
 * repository env (`COGNIS_DB_PATH`) so a renamed/legacy key still migrates, or
 * by the canonical derived name as a fallback.
 */
function findRepoCognisEntries(
  sourceServers: Record<string, McpServerBlock>,
  repoRoot: string,
  canonicalServerName: string
): string[] {
  const names: string[] = [];
  for (const [name, block] of Object.entries(sourceServers)) {
    if (!isCognisMcpServerName(name)) {
      continue;
    }
    const env = block?.env ?? {};
    if (name === canonicalServerName || envMatchesRepo(repoRoot, env)) {
      names.push(name);
    }
  }
  return names;
}

/**
 * Compute the migration plan for `repoRoot` without side effects. Safe to call
 * for a dry-run/preview and to render an auditable "what will move" summary.
 */
export function planGlobalEntryToWorkspaceMigration(
  repoRoot: string,
  options: MigrateOptions = {}
): MigrationPlan {
  const host = options.host ?? resolveMcpHost();
  const homeDir = options.homeDir ?? os.homedir();
  const sourcePath = getGlobalMcpConfigPath(host, homeDir);
  const destinationPath = getWorkspaceMcpConfigPath(repoRoot, host);
  if (!destinationPath) {
    throw new Error(
      `MCP host "${host}" has no workspace-scoped config path; cannot migrate to workspace scope`
    );
  }
  const canonicalServerName = deriveMcpServerName(repoRoot);
  const sourceExists = fs.existsSync(sourcePath);
  const destinationExists = fs.existsSync(destinationPath);
  const serverNames = sourceExists
    ? findRepoCognisEntries(
        toServerMap(readJsonFile(sourcePath)),
        repoRoot,
        canonicalServerName
      )
    : [];
  return {
    repoRoot,
    host,
    sourcePath,
    destinationPath,
    serverNames,
    canonicalServerName,
    sourceExists,
    destinationExists,
    willMoveEntry: serverNames.length > 0,
  };
}

function timestampFor(now: () => number): string {
  // ISO instant with filesystem-safe separators (no ':' or '.').
  return new Date(now()).toISOString().replace(/[:.]/g, "-");
}

/** Byte-for-byte copy of `filePath` to a timestamped `.backup` sibling. */
function backupFile(filePath: string, timestamp: string): MigrationBackup {
  const dir = path.dirname(filePath);
  const base = path.basename(filePath);
  let backupPath = path.join(dir, `${base}.${timestamp}.backup`);
  // Guard against an (unlikely) same-timestamp collision so we never clobber an
  // earlier backup within one process.
  let suffix = 1;
  while (fs.existsSync(backupPath)) {
    backupPath = path.join(dir, `${base}.${timestamp}.${suffix}.backup`);
    suffix += 1;
  }
  fs.copyFileSync(filePath, backupPath);
  return { originalPath: filePath, backupPath };
}

/**
 * Migrate this repository's Cognis MCP entry from the shared global host config
 * into its workspace config, atomically and rollback-safely.
 *
 * Order of operations (the safety-critical part):
 *   1. plan + lock source and destination
 *   2. write timestamped byte-preserving backups of every file that exists
 *   3. parse/validate both files
 *   4. merge only the matched Cognis entry into the destination (preserving all
 *      non-Cognis keys) and write it atomically
 *   5. verify the destination is host-visible and its repo env matches
 *   6. remove the entry from the source ONLY after the destination is verified,
 *      write the source atomically, and verify the removal
 *
 * If any step throws (including a simulated interruption), every touched file
 * is restored from its backup, the backups are retained, and the outcome
 * reports `rolledBack: true`. On verified success the backups are removed
 * (unless `retainBackups` is set).
 */
export function migrateGlobalEntryToWorkspace(
  repoRoot: string,
  options: MigrateOptions = {}
): MigrationOutcome {
  const now = options.now ?? Date.now;
  const plan = planGlobalEntryToWorkspaceMigration(repoRoot, options);
  const steps: string[] = [];
  const record = (msg: string): void => {
    steps.push(msg);
  };

  const outcome: MigrationOutcome = {
    ok: false,
    dryRun: Boolean(options.dryRun),
    plan,
    backups: [],
    movedServerNames: [],
    wroteDestination: false,
    removedFromSource: false,
    rolledBack: false,
    steps,
  };

  if (options.dryRun) {
    record(
      plan.willMoveEntry
        ? `dry-run: would move [${plan.serverNames.join(", ")}] from ${plan.sourcePath} to ${plan.destinationPath}`
        : `dry-run: nothing to migrate for ${repoRoot}`
    );
    outcome.ok = true;
    return outcome;
  }

  if (!plan.willMoveEntry) {
    // Idempotent: nothing in the source belongs to this repo (already migrated
    // or never present) — a no-op success that touches no file.
    record(`no matching Cognis entry in ${plan.sourcePath}; nothing to migrate`);
    outcome.ok = true;
    return outcome;
  }

  const lockKey = [
    path.resolve(plan.sourcePath),
    path.resolve(plan.destinationPath),
  ]
    .sort()
    .join("\u0000");
  if (migrationLocks.has(lockKey)) {
    outcome.error = "a migration for these config files is already in progress";
    record(outcome.error);
    return outcome;
  }
  migrationLocks.add(lockKey);

  // Files this run created that did not exist before (removed on rollback).
  const created = new Set<string>();
  const backupByOriginal = new Map<string, string>();

  const fault = (step: MigrationStep): void => {
    record(`step: ${step}`);
    options.faultInjection?.(step);
  };

  const rollback = (): void => {
    for (const original of [plan.destinationPath, plan.sourcePath]) {
      const backup = backupByOriginal.get(original);
      if (backup) {
        try {
          fs.copyFileSync(backup, original);
        } catch {
          /* best effort — backup is retained for manual recovery */
        }
      } else if (created.has(original)) {
        try {
          fs.rmSync(original, { force: true });
        } catch {
          /* best effort */
        }
      }
    }
    outcome.rolledBack = true;
    record("rolled back all touched files from backup");
  };

  try {
    fault("planned");

    // Step 2: byte-preserving backups of every file that currently exists.
    const timestamp = timestampFor(now);
    for (const filePath of [plan.sourcePath, plan.destinationPath]) {
      if (fs.existsSync(filePath)) {
        const backup = backupFile(filePath, timestamp);
        backupByOriginal.set(filePath, backup.backupPath);
        outcome.backups.push(backup);
      }
    }
    fault("backedUp");

    // Step 3: parse/validate both files (readJsonFile throws on malformed JSON).
    const sourceCfg = readJsonFile(plan.sourcePath);
    const sourceServers = toServerMap(sourceCfg);
    const destExistedBefore = fs.existsSync(plan.destinationPath);
    const destCfg = readJsonFile(plan.destinationPath);
    const destServers = toServerMap(destCfg);

    // Step 4: merge only the matched Cognis entry/entries into the destination,
    // preserving every existing (non-Cognis and unrelated) key.
    for (const name of plan.serverNames) {
      destServers[name] = sourceServers[name];
    }
    destCfg.mcpServers = destServers;
    if (!destExistedBefore) {
      created.add(plan.destinationPath);
    }
    writeJsonFile(plan.destinationPath, destCfg);
    outcome.wroteDestination = true;
    outcome.movedServerNames = [...plan.serverNames];
    fault("wroteDestination");

    // Step 5: verify the destination is host-visible and each moved entry's
    // repo env matches this repository (never trust the write blindly).
    const verifyCfg = readJsonFile(plan.destinationPath);
    const verifyServers = toServerMap(verifyCfg);
    for (const name of plan.serverNames) {
      const block = verifyServers[name];
      if (!block) {
        throw new Error(
          `destination verification failed: ${name} missing from ${plan.destinationPath}`
        );
      }
      // Only enforce repo-env matching for entries that carry a repo env; the
      // canonical fallback (matched purely by name) may be a URL/http block.
      const env = block.env ?? {};
      if (env.COGNIS_DB_PATH && !envMatchesRepo(repoRoot, env)) {
        throw new Error(
          `destination verification failed: ${name} env does not match repo ${repoRoot}`
        );
      }
    }
    fault("verifiedDestination");

    // Step 6: only NOW remove the entries from the source and write it back.
    let removedAny = false;
    for (const name of plan.serverNames) {
      if (name in sourceServers) {
        delete sourceServers[name];
        removedAny = true;
      }
    }
    sourceCfg.mcpServers = sourceServers;
    writeJsonFile(plan.sourcePath, sourceCfg);
    outcome.removedFromSource = removedAny;
    fault("removedSource");

    // Verify the source no longer advertises the moved entries.
    const sourceVerify = toServerMap(readJsonFile(plan.sourcePath));
    for (const name of plan.serverNames) {
      if (name in sourceVerify) {
        throw new Error(
          `source verification failed: ${name} still present in ${plan.sourcePath}`
        );
      }
    }
    fault("verifiedSource");

    // Verified success: drop the backups unless the caller wants them retained.
    if (!options.retainBackups) {
      for (const { backupPath } of outcome.backups) {
        try {
          fs.rmSync(backupPath, { force: true });
        } catch {
          /* best effort */
        }
      }
      outcome.backups = [];
      record("verified success; backups removed");
    } else {
      record("verified success; backups retained by request");
    }

    outcome.ok = true;
    return outcome;
  } catch (err) {
    outcome.error = err instanceof Error ? err.message : String(err);
    record(`error: ${outcome.error}`);
    rollback();
    // Backups are intentionally retained so the interrupted move stays
    // recoverable (Requirement 2.13). outcome.backups already lists them.
    return outcome;
  } finally {
    migrationLocks.delete(lockKey);
  }
}
