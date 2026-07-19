import * as fs from "fs";
import * as path from "path";

import { deriveMcpServerName } from "./mcpServerName";

/**
 * Canonicalization of repository identity for MCP ownership dedupe (Task 3.4,
 * Requirements 2.3, 2.11; preservation 3.6).
 *
 * The bug facet `repoHasDuplicateHeavyDaemonOrOrphan` is driven in part by
 * ownership being keyed on a *raw* path. Two aliases of the same repository —
 * a symlink and its target, or `D:\Repo` vs `d:\repo` on case-insensitive
 * Windows — look like two different repos, so the extension writes two Cognis
 * entries and two heavy daemons attach. Conversely, a naive lowercase-only key
 * that ignores symlinks can *miss* a real alias.
 *
 * This module derives a single canonical identity for a repository:
 *   - absolute path,
 *   - symlink-resolved (via `realpathSync.native` where the path exists),
 *   - case/slash-normalized consistently with the rest of the extension
 *     (`normalizePathForCompare` in mcpEnv.ts, `shortPathHash` in
 *     mcpServerName.ts, which lowercase on every platform),
 * for both the repository root and its `COGNIS_DB_PATH`. Owners are deduped by
 * that identity, while distinct repositories keep distinct identities (and thus
 * separate DB paths, server names, and retrieval results — preservation 3.6).
 *
 * All helpers are pure over the filesystem (no `vscode`), so they are
 * unit-testable in plain Node without a harness.
 */

/** Default database path for a repository when no explicit override is set. */
export function defaultDbPathForRepo(repoRoot: string): string {
  return path.join(repoRoot, ".cognis", "uckg.db");
}

/**
 * Resolve `target` to its canonical, symlink-and-case-resolved absolute form.
 *
 * `realpathSync.native` collapses symlinks and, on case-insensitive volumes
 * (Windows/macOS), returns the on-disk casing; when the path does not exist yet
 * (a fresh repo enable, or a DB file not created until first index) we fall
 * back to the plain absolute path. Either way we finish with the same
 * slash+case normalization the extension already uses for equality
 * (`normalizePathForCompare`, `shortPathHash`) so two spellings of one location
 * always collapse to one key.
 */
export function canonicalizePath(target: string): string {
  const absolute = path.resolve(target);
  let resolved = absolute;
  try {
    resolved = fs.realpathSync.native(absolute);
  } catch {
    // Path may not exist yet — canonicalize the nearest existing ancestor so a
    // symlinked parent directory still collapses, then re-append the tail.
    resolved = canonicalizeViaExistingAncestor(absolute);
  }
  return normalizeCanonical(resolved);
}

/**
 * When a path does not exist, walk up to the nearest existing ancestor,
 * canonicalize *that* (resolving any symlinked parent), then re-append the
 * non-existent tail. This makes a fresh repo whose parent directory is a
 * symlink still share identity with the same repo reached through the target.
 */
function canonicalizeViaExistingAncestor(absolute: string): string {
  let ancestor = absolute;
  const tail: string[] = [];
  // Bound the walk by path depth so a pathological input cannot loop forever.
  for (let i = 0; i < 4096; i += 1) {
    if (fs.existsSync(ancestor)) {
      break;
    }
    const parent = path.dirname(ancestor);
    if (parent === ancestor) {
      // Reached the filesystem root without finding an existing ancestor.
      return absolute;
    }
    tail.unshift(path.basename(ancestor));
    ancestor = parent;
  }
  try {
    const realAncestor = fs.realpathSync.native(ancestor);
    return tail.length ? path.join(realAncestor, ...tail) : realAncestor;
  } catch {
    return absolute;
  }
}

/** Slash + case normalization shared with the rest of the extension. */
function normalizeCanonical(resolved: string): string {
  return resolved.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

/**
 * A repository's canonical identity: the symlink/case-resolved root and DB
 * path, plus a single `key` string that uniquely and stably identifies the
 * repository for ownership dedupe. Two aliases of one repository share a `key`;
 * two distinct repositories never do.
 */
export interface RepoCanonicalIdentity {
  /** Canonical (symlink/case-resolved) absolute repository root. */
  root: string;
  /** Canonical (symlink/case-resolved) absolute `COGNIS_DB_PATH`. */
  dbPath: string;
  /** Stable dedupe key combining root + DB path. */
  key: string;
}

/**
 * Derive the canonical identity for a repository. `dbPathOverride` is the
 * `COGNIS_DB_PATH` when the caller has one (from an existing server block's
 * env); otherwise the repository's default `.cognis/uckg.db` is used.
 *
 * The `key` folds both the canonical root and the canonical DB path so that a
 * repository re-pointed at a different database (a legitimately distinct
 * identity for isolation, preservation 3.6) does not alias one left at the
 * default, while symlink/case aliases of the *same* root+DB collapse together.
 */
export function canonicalRepoIdentity(
  repoRoot: string,
  dbPathOverride?: string
): RepoCanonicalIdentity {
  const root = canonicalizePath(repoRoot);
  const dbPath = canonicalizePath(dbPathOverride ?? defaultDbPathForRepo(repoRoot));
  return { root, dbPath, key: `${root}\u0000${dbPath}` };
}

/** True when two `COGNIS_DB_PATH` values name the same canonical database. */
export function sameCanonicalDb(a: string, b: string): boolean {
  return canonicalizePath(a) === canonicalizePath(b);
}

/**
 * Extract the `COGNIS_DB_PATH` from an mcp.json server block's env, if present.
 * Tolerates the loosely-typed on-disk shape.
 */
function dbPathFromBlock(block: unknown): string | undefined {
  if (!block || typeof block !== "object") {
    return undefined;
  }
  const env = (block as { env?: Record<string, unknown> }).env;
  const dbPath = env?.COGNIS_DB_PATH;
  return typeof dbPath === "string" ? dbPath : undefined;
}

/**
 * Remove every Cognis-managed server entry that resolves to the *same canonical
 * repository* as `identity`, except the one named `keepServerName`. Mutates
 * `servers` in place and returns the removed names.
 *
 * `isCognisName` is injected so this stays free of the server-name module's
 * transitive imports and reusable from tests; callers pass
 * `isCognisMcpServerName`.
 *
 * This replaces the previous raw-`path.resolve` comparison (which was
 * case-preserving and symlink-blind, so a `D:\Repo` alias of `d:\repo` was
 * treated as a *different* repo and left a duplicate heavy owner behind).
 * Deduping by canonical identity makes repeated enables idempotent: a second
 * enable of the same repo (through any alias) collapses onto the single kept
 * entry rather than accumulating `cognis-*` duplicates.
 */
export function dedupeCognisOwnersByIdentity(
  servers: Record<string, unknown>,
  identity: RepoCanonicalIdentity,
  keepServerName: string,
  isCognisName: (name: string) => boolean
): string[] {
  const removed: string[] = [];
  for (const [name, value] of Object.entries(servers)) {
    if (name === keepServerName || !isCognisName(name)) {
      continue;
    }
    const dbPath = dbPathFromBlock(value);
    if (!dbPath) {
      continue;
    }
    if (canonicalizePath(dbPath) === identity.dbPath) {
      delete servers[name];
      removed.push(name);
    }
  }
  return removed;
}

/** One resolved (repository, host) ownership slot in a multi-root/host plan. */
export interface OwnershipPlanEntry {
  /** Canonical identity this slot owns. */
  identity: RepoCanonicalIdentity;
  /** The (possibly non-canonical) repo root this slot was planned from. */
  repoRoot: string;
  /** MCP host this slot targets. */
  host: string;
  /** Deterministic Cognis server name for this repository. */
  serverName: string;
}

/**
 * Plan MCP ownership across multiple roots and hosts deterministically
 * (Requirement 2.3). Given the set of open (root, host) pairs, produce exactly
 * one ownership slot per (canonical identity, host):
 *
 *   - Repositories reached through different aliases (symlink / casing) of the
 *     same location collapse to one identity, so a single heavy owner serves
 *     all of them on a given host (dedupe, no `host × repository` fan-out for
 *     the same repo).
 *   - Each *distinct* host still gets its own slot for a repository, because a
 *     client connection in each host is legitimately required (2.3 "while
 *     allowing each required client connection").
 *   - Distinct repositories keep distinct identities and server names
 *     (preservation 3.6).
 *
 * The result is sorted by `(key, host)` so repeated or concurrent runs over the
 * same inputs yield an identical plan (idempotent).
 */
export function planOwnership(
  inputs: Array<{ repoRoot: string; host: string; dbPath?: string }>
): OwnershipPlanEntry[] {
  const bySlot = new Map<string, OwnershipPlanEntry>();
  for (const input of inputs) {
    const identity = canonicalRepoIdentity(input.repoRoot, input.dbPath);
    const slotKey = `${identity.key}\u0000${input.host}`;
    if (bySlot.has(slotKey)) {
      // A collision on the same canonical identity + host: already planned.
      // Keeping the first-seen entry (after the stable sort below the choice is
      // deterministic regardless of input order) makes the plan idempotent.
      continue;
    }
    bySlot.set(slotKey, {
      identity,
      repoRoot: input.repoRoot,
      host: input.host,
      // The server name is derived from the *canonical* root so two aliases of
      // one repo produce the same key on disk (no duplicate `cognis-*` entry).
      serverName: deriveMcpServerName(identity.root),
    });
  }
  return [...bySlot.values()].sort((a, b) => {
    if (a.identity.key !== b.identity.key) {
      return a.identity.key < b.identity.key ? -1 : 1;
    }
    if (a.host !== b.host) {
      return a.host < b.host ? -1 : 1;
    }
    return 0;
  });
}
