// Harness first: installs the vscode stub before indexd.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { resetHarness } from "./testHarness";
import { isLiveIndexing, getLiveIndexStatus } from "../indexd";

// ---------------------------------------------------------------------------
// Bug facet #6 — Orphan daemon / no cross-process lease to reclaim (Requirements
// 1.7, 2.7; preservation clause 3.9).
//
// This is a BUG-CONDITION EXPLORATION test. It encodes the *expected* (fixed)
// behavior — a live daemon must be reclaimable after an extension reload
// because ownership is recorded in a cross-process, repository-scoped lease
// (owner nonce + pid + process-start identity + heartbeat/expiry) — and
// therefore MUST FAIL on the unfixed code.
//
// On the unfixed code indexd ownership lives only in an in-memory `processes`
// map plus a best-effort status-file pid. After a reload the in-memory map is
// gone; only the status file's pid remains, with no lease file, no owner nonce,
// and no process-start identity. The next owner therefore cannot safely tell a
// live orphan of its own from an unrelated (possibly PID-reused) process, and
// there is no lease to reclaim.
// ---------------------------------------------------------------------------

function mkRepo(): string {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-orphan-repo-"));
  fs.mkdirSync(path.join(repo, ".cognis"), { recursive: true });
  return repo;
}

/**
 * Simulate the post-reload state: the extension's in-memory `processes` map is
 * empty (fresh module state — this test imports indexd.ts without ever calling
 * startLiveIndexing), and only a status file survives on disk, reporting an
 * ACTIVE daemon at a LIVE pid. We use the current test process's pid as the
 * "live orphan" so `isPidAlive` genuinely reports it alive without spawning a
 * real child.
 */
function writeLiveOrphanStatus(repo: string): number {
  const livePid = process.pid; // guaranteed alive for the duration of the test
  const statusPath = path.join(repo, ".cognis", "indexd-status.json");
  fs.writeFileSync(
    statusPath,
    JSON.stringify({
      pid: livePid,
      active: true,
      phase: "watching",
      message: "watching",
      updated_at: Date.now() / 1000,
    }),
    "utf8"
  );
  return livePid;
}

test("a live orphan daemon left after reload is reclaimable via a cross-process lease", () => {
  const repo = mkRepo();
  resetHarness(repo, {
    appName: "Cursor",
    config: { cognis: { mcpHost: "cursor", mcpConfigScope: "workspace" } },
  });

  const livePid = writeLiveOrphanStatus(repo);

  // The status file alone makes the extension believe a daemon is live (it
  // reads the status-file pid), but no in-memory handle exists after reload…
  assert.equal(
    isLiveIndexing(repo),
    true,
    "the status-file pid should make the reloaded extension see a live daemon"
  );
  // …and there is no published in-memory status handle for it (the map is gone).
  assert.equal(
    getLiveIndexStatus(repo),
    undefined,
    "after a simulated reload the in-memory status handle is absent"
  );

  // EXPECTED (fixed): a repository-scoped lease file records the owner so the
  // next owner can verify identity (pid + process-start id + nonce) and safely
  // reclaim/attach instead of spawning a duplicate. On unfixed code no lease
  // file is ever written, so the live orphan cannot be reclaimed safely.
  const leasePath = path.join(repo, ".cognis", "indexd.lease");
  const leaseExists = fs.existsSync(leasePath);

  let leaseIdentifiesOwner = false;
  if (leaseExists) {
    try {
      const lease = JSON.parse(fs.readFileSync(leasePath, "utf8")) as Record<
        string,
        unknown
      >;
      leaseIdentifiesOwner =
        typeof lease.pid === "number" &&
        lease.pid === livePid &&
        (typeof lease.owner_nonce === "string" ||
          typeof lease.process_start_id === "string" ||
          typeof lease.process_start_id === "number");
    } catch {
      leaseIdentifiesOwner = false;
    }
  }

  fs.rmSync(repo, { recursive: true, force: true });

  assert.ok(
    leaseExists && leaseIdentifiesOwner,
    "expected a repository-scoped indexd.lease recording the live owner (pid + " +
      "owner nonce / process-start identity) so a reloaded extension can reclaim " +
      "the orphan safely; unfixed code tracks ownership only in an in-memory map " +
      "plus a status-file pid, with no lease to reclaim a live orphan and no " +
      "process-start identity to guard against PID reuse"
  );
});
