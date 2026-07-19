import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import {
  canonicalRepoIdentity,
  canonicalizePath,
  dedupeCognisOwnersByIdentity,
  defaultDbPathForRepo,
  planOwnership,
  sameCanonicalDb,
} from "../mcpCanonical";
import { deriveMcpServerName, isCognisMcpServerName } from "../mcpServerName";

// ---------------------------------------------------------------------------
// Task 3.4 — Canonicalize repository identity and dedupe ownership
// (Requirements 2.3, 2.11; preservation 3.6).
//
// Unit coverage for the canonicalization + dedupe helpers backing the fix for
// the `repoHasDuplicateHeavyDaemonOrOrphan` bug facet: symlink/case-resolved
// identity, deterministic multi-root/multi-host planning, and idempotent
// ownership dedupe that never collapses two *distinct* repositories.
// ---------------------------------------------------------------------------

function mkTempRepo(tag: string): string {
  return fs.realpathSync.native(
    fs.mkdtempSync(path.join(os.tmpdir(), `cognis-canon-${tag}-`))
  );
}

function dbBlock(dbPath: string): Record<string, unknown> {
  return { command: "cognis", args: ["mcpd"], env: { COGNIS_DB_PATH: dbPath } };
}

test("canonicalizePath is slash- and case-insensitive for the same location", () => {
  const repo = mkTempRepo("case");
  try {
    const a = canonicalizePath(repo);
    const b = canonicalizePath(repo.replace(/\//g, "\\"));
    const c = canonicalizePath(repo.toUpperCase());
    // Slash style must not change the key; case must not either (the extension
    // lowercases everywhere for path equality).
    assert.equal(a, b);
    assert.equal(a, c);
    // Result is lowercased, forward-slashed, and trailing-slash trimmed.
    assert.equal(a, a.toLowerCase());
    assert.ok(!a.endsWith("/"));
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("canonicalizePath resolves a symlink to its target (same identity)", (t) => {
  const target = mkTempRepo("target");
  const linkParent = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-canon-link-"));
  const link = path.join(linkParent, "alias");
  try {
    fs.symlinkSync(target, link, "junction");
  } catch (err) {
    // Creating symlinks/junctions can require privileges on Windows; skip when
    // the environment does not permit it rather than failing spuriously.
    t.skip(`symlink not permitted in this environment: ${(err as Error).message}`);
    fs.rmSync(target, { recursive: true, force: true });
    fs.rmSync(linkParent, { recursive: true, force: true });
    return;
  }
  try {
    // The symlink and its target must canonicalize to the SAME identity so only
    // one heavy owner is created for the repository regardless of which alias
    // the window opened it through.
    assert.equal(canonicalizePath(link), canonicalizePath(target));
  } finally {
    fs.rmSync(link, { force: true });
    fs.rmSync(target, { recursive: true, force: true });
    fs.rmSync(linkParent, { recursive: true, force: true });
  }
});

test("canonicalizePath collapses a non-existent tail through an existing symlinked parent", (t) => {
  const target = mkTempRepo("nx-target");
  const linkParent = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-canon-nxlink-"));
  const link = path.join(linkParent, "alias");
  try {
    fs.symlinkSync(target, link, "junction");
  } catch (err) {
    t.skip(`symlink not permitted in this environment: ${(err as Error).message}`);
    fs.rmSync(target, { recursive: true, force: true });
    fs.rmSync(linkParent, { recursive: true, force: true });
    return;
  }
  try {
    // A DB path that does not exist yet (fresh repo, DB not created) still
    // canonicalizes through the symlinked parent so it aliases the target.
    const viaLink = canonicalizePath(path.join(link, ".cognis", "uckg.db"));
    const viaTarget = canonicalizePath(path.join(target, ".cognis", "uckg.db"));
    assert.equal(viaLink, viaTarget);
  } finally {
    fs.rmSync(link, { force: true });
    fs.rmSync(target, { recursive: true, force: true });
    fs.rmSync(linkParent, { recursive: true, force: true });
  }
});

test("canonicalRepoIdentity keys on root + DB path; distinct repos differ", () => {
  const repoA = mkTempRepo("ida");
  const repoB = mkTempRepo("idb");
  try {
    const a = canonicalRepoIdentity(repoA);
    const b = canonicalRepoIdentity(repoB);
    // Distinct repositories must never share an identity key (preservation 3.6:
    // separate DB paths, server names, retrieval results).
    assert.notEqual(a.key, b.key);
    // The default DB path is folded into the identity.
    assert.equal(a.dbPath, canonicalizePath(defaultDbPathForRepo(repoA)));
    // Aliases of the same repo collapse to one key.
    const aAlias = canonicalRepoIdentity(repoA.toUpperCase());
    assert.equal(a.key, aAlias.key);
  } finally {
    fs.rmSync(repoA, { recursive: true, force: true });
    fs.rmSync(repoB, { recursive: true, force: true });
  }
});

test("canonicalRepoIdentity distinguishes a repo re-pointed at a different DB", () => {
  const repo = mkTempRepo("dbswap");
  try {
    const def = canonicalRepoIdentity(repo);
    const swapped = canonicalRepoIdentity(
      repo,
      path.join(repo, ".cognis", "other.db")
    );
    // A different COGNIS_DB_PATH is a legitimately distinct identity for
    // isolation — it must not alias the default-DB identity.
    assert.notEqual(def.key, swapped.key);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("sameCanonicalDb treats aliases of one DB as equal and distinct DBs as unequal", () => {
  const repo = mkTempRepo("samedb");
  try {
    const db = path.join(repo, ".cognis", "uckg.db");
    assert.equal(sameCanonicalDb(db, db.replace(/\//g, "\\")), true);
    assert.equal(
      sameCanonicalDb(db, path.join(repo, ".cognis", "other.db")),
      false
    );
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("dedupeCognisOwnersByIdentity removes duplicate owners for one repo, keeps the chosen entry", () => {
  const repo = mkTempRepo("dedupe");
  try {
    const identity = canonicalRepoIdentity(repo);
    const db = defaultDbPathForRepo(repo);
    const keep = deriveMcpServerName(repo);
    const servers: Record<string, unknown> = {
      // The kept entry (canonical server name).
      [keep]: dbBlock(db),
      // A stale duplicate written through an aliased path (backslashes / case)
      // — same canonical DB, so it must be removed.
      "cognis-alias-000000": dbBlock(db.replace(/\//g, "\\").toUpperCase()),
      // A different repo's Cognis entry — must be preserved (isolation 3.6).
      "cognis-other-111111": dbBlock(
        path.join(mkTempRepoInline(), ".cognis", "uckg.db")
      ),
      // A non-Cognis server — must always be preserved (3.1).
      "brave-search": { command: "node", args: ["brave.js"], env: {} },
    };
    const removed = dedupeCognisOwnersByIdentity(
      servers,
      identity,
      keep,
      isCognisMcpServerName
    );
    assert.deepEqual(removed, ["cognis-alias-000000"]);
    assert.ok(keep in servers, "kept entry survives");
    assert.ok("cognis-other-111111" in servers, "distinct repo entry survives");
    assert.ok("brave-search" in servers, "non-Cognis entry survives");
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("dedupeCognisOwnersByIdentity is idempotent on repeated runs", () => {
  const repo = mkTempRepo("idem");
  try {
    const identity = canonicalRepoIdentity(repo);
    const db = defaultDbPathForRepo(repo);
    const keep = deriveMcpServerName(repo);
    const servers: Record<string, unknown> = {
      [keep]: dbBlock(db),
      "cognis-dup-222222": dbBlock(db),
    };
    const first = dedupeCognisOwnersByIdentity(
      servers,
      identity,
      keep,
      isCognisMcpServerName
    );
    const second = dedupeCognisOwnersByIdentity(
      servers,
      identity,
      keep,
      isCognisMcpServerName
    );
    assert.deepEqual(first, ["cognis-dup-222222"]);
    assert.deepEqual(second, []); // nothing left to remove
    assert.deepEqual(Object.keys(servers), [keep]);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("planOwnership dedupes aliases per host and allows one slot per distinct host", () => {
  const repo = mkTempRepo("plan");
  try {
    const plan = planOwnership([
      { repoRoot: repo, host: "cursor" },
      // Same repo through an alias + same host → collapses (no host×repo fan-out).
      { repoRoot: repo.toUpperCase(), host: "cursor" },
      // Same repo, different host → a distinct required client connection.
      { repoRoot: repo, host: "vscode" },
    ]);
    // Two slots: (repo, cursor) and (repo, vscode) — the aliased duplicate is gone.
    assert.equal(plan.length, 2);
    const hosts = plan.map((p) => p.host).sort();
    assert.deepEqual(hosts, ["cursor", "vscode"]);
    // Both slots share the same canonical identity and server name.
    assert.equal(plan[0].identity.key, plan[1].identity.key);
    assert.equal(plan[0].serverName, plan[1].serverName);
    assert.equal(plan[0].serverName, deriveMcpServerName(canonicalizePath(repo)));
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("planOwnership is order-independent (idempotent/deterministic plan)", () => {
  const repoA = mkTempRepo("ord-a");
  const repoB = mkTempRepo("ord-b");
  try {
    const forward = planOwnership([
      { repoRoot: repoA, host: "cursor" },
      { repoRoot: repoB, host: "vscode" },
    ]);
    const reversed = planOwnership([
      { repoRoot: repoB, host: "vscode" },
      { repoRoot: repoA, host: "cursor" },
    ]);
    // The plan is sorted by (key, host) so input order cannot change it.
    assert.deepEqual(
      forward.map((p) => [p.identity.key, p.host]),
      reversed.map((p) => [p.identity.key, p.host])
    );
  } finally {
    fs.rmSync(repoA, { recursive: true, force: true });
    fs.rmSync(repoB, { recursive: true, force: true });
  }
});

test("planOwnership keeps distinct repositories separate (isolation 3.6)", () => {
  const repoA = mkTempRepo("iso-a");
  const repoB = mkTempRepo("iso-b");
  try {
    const plan = planOwnership([
      { repoRoot: repoA, host: "cursor" },
      { repoRoot: repoB, host: "cursor" },
    ]);
    assert.equal(plan.length, 2);
    assert.notEqual(plan[0].identity.key, plan[1].identity.key);
    assert.notEqual(plan[0].serverName, plan[1].serverName);
  } finally {
    fs.rmSync(repoA, { recursive: true, force: true });
    fs.rmSync(repoB, { recursive: true, force: true });
  }
});

/** Inline temp repo for a nested distinct-repo fixture (auto-cleaned by OS). */
function mkTempRepoInline(): string {
  return fs.realpathSync.native(
    fs.mkdtempSync(path.join(os.tmpdir(), "cognis-canon-inline-"))
  );
}
