import type { ContractCompatibility, HandshakeResult } from "./contract";

/** Lifecycle phase of a compatibility verdict for one workspace. */
export type CompatibilityPhase = "checking" | "confirmed" | "unavailable";

interface CompatibilitySnapshotBase {
  /** Monotonic token assigned to the probe that produced this state. */
  generation: number;
  /** Epoch milliseconds when this state was observed. */
  observedAt: number;
}

export interface CheckingCompatibilitySnapshot
  extends CompatibilitySnapshotBase {
  phase: "checking";
  result?: never;
}

export interface ConfirmedCompatibilitySnapshot
  extends CompatibilitySnapshotBase {
  phase: "confirmed";
  /** The complete verdict returned by the existing handshake evaluator. */
  result: HandshakeResult;
}

export interface UnavailableCompatibilitySnapshot
  extends CompatibilitySnapshotBase {
  phase: "unavailable";
  result?: never;
}

/**
 * Deterministic state for a context created before its first compatibility
 * probe. Generation zero is reserved for this unprobed state and observedAt
 * zero makes it unambiguously stale once coordinator-backed probing is wired.
 */
export const FIRST_PROBE_COMPATIBILITY_SNAPSHOT: UnavailableCompatibilitySnapshot =
  Object.freeze({
    phase: "unavailable",
    generation: 0,
    observedAt: 0,
  });

/**
 * Discriminated compatibility state committed for one workspace.
 *
 * Only a confirmed state carries a handshake result. An unavailable state
 * means no verdict was obtained and must not be interpreted as a mismatch.
 */
export type CompatibilitySnapshot =
  | CheckingCompatibilitySnapshot
  | ConfirmedCompatibilitySnapshot
  | UnavailableCompatibilitySnapshot;

/** A handshake verdict that is confirmed and not compatible. */
export type ConfirmedMismatchResult = HandshakeResult & {
  compatibility: Exclude<ContractCompatibility, "ok">;
};

/**
 * Stable dedupe key for a confirmed compatibility verdict. Two verdicts share
 * an identity when they describe the *same* actionable skew, so the extension
 * can show at most one notification per identity per activation session
 * (Requirement 4.2/4.5). The identity intentionally spans both the engine-build
 * pair and the contract-version pair: a later engine bump or contract change
 * yields a new identity and therefore re-prompts even if the previous identity
 * was dismissed.
 */
export interface CompatibilityIdentity {
  kind: ContractCompatibility;
  engineVersion?: string;
  expectedEngineVersion?: string;
  backendContractVersion?: number;
  expectedContractVersion: number;
}

/** Purely project a handshake verdict onto its {@link CompatibilityIdentity}. */
export function compatibilityIdentity(
  result: HandshakeResult
): CompatibilityIdentity {
  return {
    kind: result.compatibility,
    engineVersion: result.engineVersion,
    expectedEngineVersion: result.expectedEngineVersion,
    backendContractVersion: result.backendContractVersion,
    expectedContractVersion: result.expectedContractVersion,
  };
}

/**
 * Canonical string form of a {@link CompatibilityIdentity}, suitable for a
 * session-scoped `Set` used to dedupe notifications. Field order is fixed and
 * every field participates, so distinct kinds or version pairs never collide.
 */
export function compatibilityIdentityKey(result: HandshakeResult): string {
  const id = compatibilityIdentity(result);
  return [
    id.kind,
    id.engineVersion ?? "",
    id.expectedEngineVersion ?? "",
    id.backendContractVersion ?? "",
    id.expectedContractVersion,
  ].join("|");
}
/**
 * Purely map an obtained handshake result into a confirmed snapshot.
 * The result object is preserved by reference so no existing fields are lost.
 */
export function compatibilitySnapshotFromHandshake(
  result: HandshakeResult,
  generation: number,
  observedAt: number
): ConfirmedCompatibilitySnapshot {
  return {
    phase: "confirmed",
    result,
    generation,
    observedAt,
  };
}

/** Alias with result-first naming for coordinator and publishing call sites. */
export const confirmedCompatibilitySnapshot = compatibilitySnapshotFromHandshake;

/** Narrow a snapshot to a confirmed handshake verdict. */
export function isConfirmedCompatibility(
  snapshot: CompatibilitySnapshot
): snapshot is ConfirmedCompatibilitySnapshot {
  return snapshot.phase === "confirmed";
}

/** True only when the committed verdict is confirmed and non-ok. */
export function isConfirmedMismatch(
  snapshot: CompatibilitySnapshot
): snapshot is ConfirmedCompatibilitySnapshot & {
  result: ConfirmedMismatchResult;
} {
  return (
    snapshot.phase === "confirmed" &&
    snapshot.result.compatibility !== "ok"
  );
}

/**
 * The single safe remediation the Compatibility_Primary_Action offers for a
 * confirmed mismatch. Its `actionId` is one of the three permitted remediation
 * commands (`installBackend` | `updateExtension` | `reinstallEngine`) and never
 * a Cold Restart / rebuild / remove action (Requirement 3.7). Only `Repair
 * Engine` (the `unreadable` case) is destructive and therefore modal-gated.
 */
export interface CompatibilityRemediation {
  actionId: "installBackend" | "updateExtension" | "reinstallEngine";
  label: "Update Engine" | "Update Extension" | "Repair Engine";
  destructive: boolean;
}

/**
 * Purely map a Compatibility_Kind to its remediation, matching the Requirement
 * 3 decision table 1:1. Returns `undefined` for `ok` (no remediation — the
 * operational control stays in effect).
 *
 * | Compatibility_Kind    | actionId         | label            | destructive |
 * | --------------------- | ---------------- | ---------------- | ----------- |
 * | engine-outdated       | installBackend   | Update Engine    | no          |
 * | backend-older         | installBackend   | Update Engine    | no          |
 * | capabilities-missing  | installBackend   | Update Engine    | no          |
 * | engine-newer          | updateExtension  | Update Extension | no          |
 * | backend-newer         | updateExtension  | Update Extension | no          |
 * | unreadable            | reinstallEngine  | Repair Engine    | yes (modal) |
 * | ok                    | (operational)    | —                | —           |
 *
 * This is the canonical 1:1 mapping every Compatibility_Kind resolves through
 * (Requirement 3.4–3.7). Every `actionId` is one of the three permitted
 * remediation commands and never a Cold Restart / rebuild / remove action;
 * every `label` uses the user vocabulary "Engine"/"Extension" and never
 * "Backend". Only the `unreadable` / Repair Engine case is destructive and is
 * therefore modal-gated. Exhaustively validated in `compatibility.test.ts`.
 */
export function deriveRemediation(
  result: HandshakeResult
): CompatibilityRemediation | undefined {
  switch (result.compatibility) {
    case "engine-outdated":
    case "backend-older":
    case "capabilities-missing":
      return {
        actionId: "installBackend",
        label: "Update Engine",
        destructive: false,
      };
    case "engine-newer":
    case "backend-newer":
      return {
        actionId: "updateExtension",
        label: "Update Extension",
        destructive: false,
      };
    case "unreadable":
      return {
        actionId: "reinstallEngine",
        label: "Repair Engine",
        destructive: true,
      };
    case "ok":
    default:
      return undefined;
  }
}
