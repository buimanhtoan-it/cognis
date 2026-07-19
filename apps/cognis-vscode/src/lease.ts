/**
 * Cross-process, repository-scoped ownership lease (TypeScript boundary).
 *
 * This is the extension-side counterpart to the Rust lease module
 * (`crates/cognis-core/src/lease.rs`, Task 6.1). Heavy daemons (`indexd`,
 * `mcpd`) historically recorded ownership only in an in-memory `processes`
 * map plus a best-effort status-file pid. After a reload/crash the map is gone
 * and only the status pid survives — so a live orphan cannot be told apart
 * from an unrelated (possibly PID-reused) process, and there is no lease to
 * reclaim (bug facet `repoHasDuplicateHeavyDaemonOrOrphan`).
 *
 * The on-disk schema matches the Rust producer byte-for-byte so either side
 * can read the other's lease:
 *
 * ```json
 * {
 *   "owner_nonce": "<string>",
 *   "pid": 12345,
 *   "process_start_id": "<string>",
 *   "heartbeat_at": 1710000000.0,
 *   "expiry": 1710000030.0
 * }
 * ```
 *
 * Requirements 2.7, 2.13; Correctness Property 8; preservation 3.6, 3.9.
 */
import { spawnSync } from "node:child_process";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";

/** The per-repo runtime directory (mirrors `CONFIG_DIR_NAME` in cognis-core). */
const CONFIG_DIR_NAME = ".cognis";

/** Heavy-daemon roles that own a repository-scoped lease. */
export type LeaseRole = "indexd" | "mcpd";

const LEASE_FILES: Record<LeaseRole, string> = {
  indexd: "indexd.lease",
  mcpd: "mcpd.lease",
};

/**
 * Default lease TTL in seconds. Matches `DEFAULT_LEASE_TTL` in the Rust module.
 * The owner refreshes well inside this window; readers treat `now >= expiry`
 * as reclaimable.
 */
export const DEFAULT_LEASE_TTL_SECONDS = 30;

/** On-disk lease record (snake_case JSON keys, aligned with the Rust producer). */
export interface LeaseRecord {
  owner_nonce: string;
  pid: number;
  process_start_id: string;
  heartbeat_at: number;
  expiry: number;
}

/** Result of comparing a live pid against a recorded lease owner. */
export type OwnerVerification =
  | "match" // the process at `pid` is provably the recorded owner
  | "mismatch" // the pid was reused by an unrelated process — DO NOT kill
  | "unknown"; // identity could not be confirmed either way

/** Marker prefix used when a real OS process-start id could not be obtained. */
const UNVERIFIED_PREFIX = "unverified-";

/** Absolute path of the lease file for `role` under `<repoRoot>/.cognis/`. */
export function leasePath(repoRoot: string, role: LeaseRole): string {
  return path.join(repoRoot, CONFIG_DIR_NAME, LEASE_FILES[role]);
}

function nowSeconds(): number {
  return Date.now() / 1000;
}

/**
 * Read and validate a lease file. Returns `undefined` when the file is absent
 * or its contents are not a well-formed lease record (a corrupt lease is
 * treated as reclaimable by callers, never trusted).
 */
export function readLease(repoRoot: string, role: LeaseRole): LeaseRecord | undefined {
  const file = leasePath(repoRoot, role);
  try {
    if (!fs.existsSync(file)) {
      return undefined;
    }
    const raw = fs.readFileSync(file, "utf8").trim();
    if (!raw) {
      return undefined;
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (
      typeof parsed.owner_nonce !== "string" ||
      typeof parsed.pid !== "number" ||
      (typeof parsed.process_start_id !== "string" &&
        typeof parsed.process_start_id !== "number") ||
      typeof parsed.heartbeat_at !== "number" ||
      typeof parsed.expiry !== "number"
    ) {
      return undefined;
    }
    return {
      owner_nonce: parsed.owner_nonce,
      pid: parsed.pid,
      process_start_id: String(parsed.process_start_id),
      heartbeat_at: parsed.heartbeat_at,
      expiry: parsed.expiry,
    };
  } catch {
    return undefined;
  }
}

/**
 * Write a lease record atomically (temp file + rename with a short retry),
 * mirroring the indexd status-file write so a concurrent reader never observes
 * a truncated JSON body. Best-effort: I/O failures are swallowed (the lease is
 * an optimization for safe reclaim, never a correctness gate for retrieval).
 */
export function writeLeaseAtomic(
  repoRoot: string,
  role: LeaseRole,
  record: LeaseRecord
): boolean {
  const file = leasePath(repoRoot, role);
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const text = `${JSON.stringify(record, null, 2)}\n`;
    const tmp = `${file}.tmp.${process.pid}-${crypto.randomBytes(4).toString("hex")}`;
    fs.writeFileSync(tmp, text, "utf8");
    let lastErr: unknown;
    for (let attempt = 0; attempt < 10; attempt += 1) {
      try {
        fs.renameSync(tmp, file);
        return true;
      } catch (err) {
        lastErr = err;
        // Windows can transiently fail a rename over an existing file; retry.
        try {
          const start = Date.now();
          while (Date.now() - start < 20 * (attempt + 1)) {
            /* short spin-wait: setTimeout is async; this write path is sync */
          }
        } catch {
          /* ignore */
        }
      }
    }
    try {
      fs.rmSync(tmp, { force: true });
    } catch {
      /* ignore */
    }
    void lastErr;
    return false;
  } catch {
    return false;
  }
}

/** True when a lease's heartbeat window has elapsed (reclaimable). */
export function isLeaseExpired(record: LeaseRecord, now: number = nowSeconds()): boolean {
  return now >= record.expiry;
}

/** A fresh owner nonce — collision-resistant without a heavy RNG dependency. */
function generateOwnerNonce(): string {
  return crypto.randomUUID();
}

/**
 * Best-effort OS process-start identity for `pid`.
 *
 * This is the guard against PID reuse: two different processes that happen to
 * share a pid will (almost always) have different creation times, so recording
 * and later re-checking the start id lets cleanup refuse to terminate an
 * unrelated process that merely inherited a dead owner's pid.
 *
 * Returns `undefined` when the identity cannot be obtained (missing tool,
 * permission, or the pid is already gone) — callers then treat verification as
 * `unknown` and favor safe non-destruction.
 */
export function queryProcessStartId(pid: number): string | undefined {
  if (!pid || pid <= 0) {
    return undefined;
  }
  try {
    if (process.platform === "win32") {
      // wmic is fast and present on most Windows; fall back to a CIM query.
      const wmic = spawnSync(
        "wmic",
        ["process", "where", `ProcessId=${pid}`, "get", "CreationDate", "/format:list"],
        { encoding: "utf8", timeout: 4000, windowsHide: true }
      );
      if (wmic.status === 0 && typeof wmic.stdout === "string") {
        const m = wmic.stdout.match(/CreationDate=(\S+)/);
        if (m && m[1]) {
          return `win-${m[1]}`;
        }
      }
      const ps = spawnSync(
        "powershell",
        [
          "-NoProfile",
          "-NonInteractive",
          "-Command",
          `$p = Get-CimInstance Win32_Process -Filter "ProcessId=${pid}" -ErrorAction SilentlyContinue; if ($p) { $p.CreationDate.ToString('o') }`,
        ],
        { encoding: "utf8", timeout: 8000, windowsHide: true }
      );
      if (ps.status === 0 && typeof ps.stdout === "string" && ps.stdout.trim()) {
        return `win-${ps.stdout.trim()}`;
      }
      return undefined;
    }
    if (process.platform === "linux") {
      const stat = fs.readFileSync(`/proc/${pid}/stat`, "utf8");
      // Field 22 (starttime) sits after the parenthesized comm field, which may
      // itself contain spaces/parens — split on the trailing ')'.
      const afterComm = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/);
      const starttime = afterComm[19]; // 22nd field overall ⇒ index 19 post-comm
      if (starttime) {
        return `proc-${starttime}`;
      }
      return undefined;
    }
    // macOS / BSD: `ps -o lstart=` prints the wall-clock start time.
    const ps = spawnSync("ps", ["-o", "lstart=", "-p", String(pid)], {
      encoding: "utf8",
      timeout: 4000,
    });
    if (ps.status === 0 && typeof ps.stdout === "string" && ps.stdout.trim()) {
      return `ps-${ps.stdout.trim().replace(/\s+/g, "_")}`;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

/**
 * Record (or refresh) a repository-scoped lease for a *live orphan* — a daemon
 * we observe as alive (via its status-file pid) but do not own an in-memory
 * handle for, e.g. after an extension reload.
 *
 * Idempotent: if a live, non-expired lease already records this exact pid with
 * a real (verified) process-start id, nothing is written. Otherwise a fresh
 * lease is written capturing pid + process-start identity + owner nonce so the
 * owner is identifiable and can later be reclaimed safely.
 *
 * Best-effort: never throws (retrieval must not depend on lease I/O).
 */
export function reconcileOrphanLease(
  repoRoot: string,
  role: LeaseRole,
  pid: number | undefined,
  ttlSeconds: number = DEFAULT_LEASE_TTL_SECONDS
): void {
  try {
    if (!pid || pid <= 0) {
      return;
    }
    const now = nowSeconds();
    const existing = readLease(repoRoot, role);
    if (
      existing &&
      existing.pid === pid &&
      !isLeaseExpired(existing, now) &&
      !existing.process_start_id.startsWith(UNVERIFIED_PREFIX)
    ) {
      // A live, identity-bearing lease already records this owner — done.
      return;
    }
    const startId =
      queryProcessStartId(pid) ?? `${UNVERIFIED_PREFIX}${Math.floor(now * 1000)}`;
    const record: LeaseRecord = {
      owner_nonce: generateOwnerNonce(),
      pid,
      process_start_id: startId,
      heartbeat_at: now,
      expiry: now + ttlSeconds,
    };
    writeLeaseAtomic(repoRoot, role, record);
  } catch {
    // Swallow: lease reconciliation is an optimization, not a correctness gate.
  }
}

/**
 * Compare the process currently living at `pid` against the recorded lease
 * owner for `role`.
 *
 * * `"match"`    — the lease records this pid and the live process-start id
 *                  equals the recorded one (provably the same process).
 * * `"mismatch"` — the lease records this pid but the live process-start id
 *                  differs (the pid was reused by an unrelated process). The
 *                  caller MUST NOT terminate it (safe non-destruction, 3.9).
 * * `"unknown"`  — no lease, a different pid, or identity could not be
 *                  confirmed either way (missing tool / unverified marker).
 */
export function verifyLeaseOwner(
  repoRoot: string,
  role: LeaseRole,
  pid: number | undefined
): OwnerVerification {
  if (!pid || pid <= 0) {
    return "unknown";
  }
  const lease = readLease(repoRoot, role);
  if (!lease || lease.pid !== pid) {
    return "unknown";
  }
  if (lease.process_start_id.startsWith(UNVERIFIED_PREFIX)) {
    return "unknown";
  }
  const current = queryProcessStartId(pid);
  if (!current) {
    return "unknown";
  }
  return current === lease.process_start_id ? "match" : "mismatch";
}

/**
 * Remove the lease file for `role` when it still records `pid` as the owner.
 * Never clobbers a lease owned by a *different* pid (safe non-destruction).
 * Best-effort; missing files are treated as success.
 */
export function removeLeaseForPid(
  repoRoot: string,
  role: LeaseRole,
  pid: number | undefined
): void {
  try {
    const lease = readLease(repoRoot, role);
    if (!lease) {
      return;
    }
    if (pid !== undefined && lease.pid !== pid) {
      return; // belongs to another owner — leave it.
    }
    fs.rmSync(leasePath(repoRoot, role), { force: true });
  } catch {
    /* best effort */
  }
}
