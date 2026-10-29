/**
 * Reversible sharing gate for one-heavy-daemon-per-repository topology
 * (Task 7.3 / Requirement 2.9; Correctness Property 10; preservation 3.8).
 *
 * Direct HTTP sharing, session sharing, and a model broker stay disabled
 * behind this gate until:
 *   1. the user/config explicitly opts in (flag default OFF), AND
 *   2. every required evidence check passes (semantic parity, eight-tool
 *      contracts, cancellation/failure behavior, concurrent load/eviction
 *      safety, repository isolation, model-fingerprint isolation, and
 *      statistically reproducible process/private-byte improvement).
 *
 * A failed (or closed) gate always retains the compatible stdio path —
 * thin-proxy / per-repository-daemon — with no data loss. Shared HTTP is
 * never selected while the gate is closed.
 *
 * Pure evaluation lives at the top so unit tests can drive the decision
 * without a VS Code harness. VS Code / env resolution is thin and
 * side-effect free on the read path.
 */
import * as fs from "node:fs";
import * as path from "node:path";

/**
 * Lazy VS Code import so pure evaluation / evidence parsing stay loadable
 * under plain Node unit tests (no VS Code harness). The host-bound resolvers
 * (`resolveMcpSharedHttpFlag`, etc.) are the only call sites that touch this.
 */
function tryGetVscode(): typeof import("vscode") | undefined {
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require("vscode") as typeof import("vscode");
  } catch {
    return undefined;
  }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/**
 * Transport topology selected by the gate.
 *
 * * ``"thin-proxy-stdio"`` — compatible stdio path (thin proxy by default,
 *   or legacy heavy stdio when ``cognis.mcpStdioMode = "heavy"``). This is
 *   the gate-OFF / failed-gate topology (preservation 3.8).
 * * ``"shared-http"`` — host-verified bounded-concurrent loopback HTTP to
 *   one heavy repository daemon. Only selected when the flag is ON and
 *   every required gate check has evidence of pass.
 */
export type SharingTopology = "thin-proxy-stdio" | "shared-http";

/**
 * The seven evidence checks required by Requirement 2.9 before shared HTTP
 * (or a model broker) may be enabled. Missing evidence is treated as fail
 * (fail-closed).
 */
export type GateCheckId =
  | "semanticParity"
  | "eightToolContracts"
  | "cancellationFailure"
  | "concurrentLoadEviction"
  | "repositoryIsolation"
  | "modelFingerprintIsolation"
  | "processPrivateByteImprovement";

/** Ordered list of required checks (stable for logs / tests). */
export const REQUIRED_GATE_CHECKS: readonly GateCheckId[] = [
  "semanticParity",
  "eightToolContracts",
  "cancellationFailure",
  "concurrentLoadEviction",
  "repositoryIsolation",
  "modelFingerprintIsolation",
  "processPrivateByteImprovement",
] as const;

/** One evidence record for a single gate check. */
export interface GateCheckEvidence {
  /** True when the check has been demonstrated to pass. */
  passed: boolean;
  /**
   * Free-form evidence pointer (test name, measurement report path, commit
   * SHA, …). Required for a `passed: true` claim to count — an empty claim
   * is treated as missing evidence.
   */
  evidence?: string;
  /** Optional human detail for diagnostics. */
  detail?: string;
}

/** Result of evaluating a single check. */
export interface GateCheckResult {
  id: GateCheckId;
  passed: boolean;
  evidence?: string;
  detail?: string;
  /** Why this check failed, when it did. */
  reason?: string;
}

/**
 * Full gate decision. Callers use `topology` / `sharingEnabled` to pick the
 * transport; `checks` and `summary` are for logs and the panel.
 */
export interface SharingGateDecision {
  /** Selected topology after applying the flag + checks. */
  topology: SharingTopology;
  /** True when the user/config opted into shared HTTP (flag ON). */
  flagEnabled: boolean;
  /**
   * True only when the flag is ON and every required check passed with
   * non-empty evidence. Equivalent to `topology === "shared-http"`.
   */
  sharingEnabled: boolean;
  /** Per-check outcomes (always populated for the seven required ids). */
  checks: GateCheckResult[];
  /**
   * Why we retained the stdio path, when `sharingEnabled` is false.
   * Undefined when sharing is enabled.
   */
  fallbackReason?: string;
  /** One-line summary suitable for the output channel. */
  summary: string;
}

/**
 * Env var that opts into shared HTTP (must still pass every gate check).
 * Values accepted as true: ``1``, ``true``, ``on``, ``yes``.
 * Default (absent / anything else) is OFF.
 */
export const SHARED_HTTP_FLAG_ENV = "COGNIS_MCP_SHARED_HTTP";

/**
 * Optional path (or inline JSON) of recorded gate evidence. When set, the
 * resolver loads it so E2E / measurement harnesses can open the gate after
 * producing the required evidence. Schema:
 *
 * ```json
 * {
 *   "semanticParity": { "passed": true, "evidence": "…" },
 *   …
 * }
 * ```
 */
export const GATE_EVIDENCE_ENV = "COGNIS_MCP_SHARING_GATE_EVIDENCE";

/**
 * On-disk evidence file under a repository's ``.cognis/`` directory. Loaded
 * only when present; missing file means no evidence (fail-closed).
 */
export const GATE_EVIDENCE_FILENAME = "sharing-gate-evidence.json";

// ---------------------------------------------------------------------------
// Pure evaluation (no VS Code, no I/O) — unit-testable in plain Node.
// ---------------------------------------------------------------------------

/**
 * Evaluate one check against its evidence. Fail-closed:
 * * missing evidence → fail
 * * ``passed: false`` → fail
 * * ``passed: true`` without a non-empty ``evidence`` string → fail
 *   (a bare claim without a pointer is not evidence)
 */
export function evaluateGateCheck(
  id: GateCheckId,
  evidence: GateCheckEvidence | undefined
): GateCheckResult {
  if (!evidence) {
    return {
      id,
      passed: false,
      reason: "missing evidence",
    };
  }
  if (evidence.passed !== true) {
    return {
      id,
      passed: false,
      evidence: evidence.evidence,
      detail: evidence.detail,
      reason: evidence.detail ?? "check reported failure",
    };
  }
  const pointer = (evidence.evidence ?? "").trim();
  if (!pointer) {
    return {
      id,
      passed: false,
      detail: evidence.detail,
      reason: "passed claim lacks evidence pointer",
    };
  }
  return {
    id,
    passed: true,
    evidence: pointer,
    detail: evidence.detail,
  };
}

/**
 * Pure gate evaluation.
 *
 * * ``flagEnabled === false`` (the default) → always ``thin-proxy-stdio``,
 *   regardless of evidence. Sharing stays disabled until the flag is on
 *   *and* every check passes (Requirement 2.9).
 * * ``flagEnabled === true`` → every required check must pass with evidence;
 *   any failure retains ``thin-proxy-stdio`` with a concrete fallback reason.
 *   No data is rewritten here — the decision is pure; callers apply it by
 *   writing the compatible stdio path (no data loss; preservation 3.8).
 */
export function evaluateSharingGate(
  flagEnabled: boolean,
  evidence: Partial<Record<GateCheckId, GateCheckEvidence>> = {}
): SharingGateDecision {
  const checks = REQUIRED_GATE_CHECKS.map((id) =>
    evaluateGateCheck(id, evidence[id])
  );

  if (!flagEnabled) {
    return {
      topology: "thin-proxy-stdio",
      flagEnabled: false,
      sharingEnabled: false,
      checks,
      fallbackReason:
        "shared HTTP flag is OFF (default); sharing topology disabled until explicitly enabled and all gate checks pass",
      summary:
        "sharing gate CLOSED (flag OFF) → thin-proxy/per-repository-daemon stdio path",
    };
  }

  const failed = checks.filter((c) => !c.passed);
  if (failed.length > 0) {
    const failedIds = failed.map((c) => c.id).join(", ");
    return {
      topology: "thin-proxy-stdio",
      flagEnabled: true,
      sharingEnabled: false,
      checks,
      fallbackReason: `gate checks failed: ${failedIds}; retaining thin-proxy/per-repository-daemon stdio path (no data loss)`,
      summary: `sharing gate FAILED (${failed.length}/${checks.length} checks) → stdio fallback`,
    };
  }

  return {
    topology: "shared-http",
    flagEnabled: true,
    sharingEnabled: true,
    checks,
    summary:
      "sharing gate OPEN (flag ON, all checks passed) → shared HTTP topology",
  };
}

/**
 * Convenience: select the topology from a pre-computed decision (or re-run
 * evaluation). Exists so call sites read as ``selectSharingTopology(…)``
 * rather than digging into the decision object.
 */
export function selectSharingTopology(
  flagEnabled: boolean,
  evidence: Partial<Record<GateCheckId, GateCheckEvidence>> = {}
): SharingTopology {
  return evaluateSharingGate(flagEnabled, evidence).topology;
}

/**
 * True when shared HTTP may be written / started. Equivalent to
 * ``evaluateSharingGate(...).sharingEnabled``. Fail-closed.
 */
export function isSharedHttpAllowed(
  flagEnabled: boolean,
  evidence: Partial<Record<GateCheckId, GateCheckEvidence>> = {}
): boolean {
  return evaluateSharingGate(flagEnabled, evidence).sharingEnabled;
}

// ---------------------------------------------------------------------------
// Evidence parsing (pure given a string / object)
// ---------------------------------------------------------------------------

/**
 * Parse a JSON evidence document into a partial evidence map. Unknown keys
 * are ignored; malformed entries are dropped (treated as missing → fail).
 * Accepts either the full map form or a ``{ checks: { … } }`` wrapper.
 */
export function parseGateEvidenceDocument(
  raw: unknown
): Partial<Record<GateCheckId, GateCheckEvidence>> {
  if (typeof raw !== "object" || raw === null) {
    return {};
  }
  const root = raw as Record<string, unknown>;
  const source =
    typeof root.checks === "object" && root.checks !== null
      ? (root.checks as Record<string, unknown>)
      : root;

  const out: Partial<Record<GateCheckId, GateCheckEvidence>> = {};
  for (const id of REQUIRED_GATE_CHECKS) {
    const entry = source[id];
    if (typeof entry !== "object" || entry === null) {
      continue;
    }
    const e = entry as Record<string, unknown>;
    const passed = e.passed === true || e.passed === "true" || e.passed === 1;
    const evidence =
      typeof e.evidence === "string"
        ? e.evidence
        : typeof e.evidence === "number"
          ? String(e.evidence)
          : undefined;
    const detail = typeof e.detail === "string" ? e.detail : undefined;
    out[id] = { passed, evidence, detail };
  }
  return out;
}

/**
 * Parse a JSON string (file contents or inline env value) into evidence.
 * Returns ``{}`` on parse failure (fail-closed).
 */
export function parseGateEvidenceJson(
  text: string
): Partial<Record<GateCheckId, GateCheckEvidence>> {
  try {
    return parseGateEvidenceDocument(JSON.parse(text));
  } catch {
    return {};
  }
}

// ---------------------------------------------------------------------------
// Config / env resolution (thin VS Code + process.env layer)
// ---------------------------------------------------------------------------

/**
 * Resolve the shared-HTTP opt-in flag.
 *
 * Default is OFF. Opt in via:
 * * setting ``cognis.mcpSharedHttpEnabled`` = ``true``, or
 * * env ``COGNIS_MCP_SHARED_HTTP=1`` (also ``true`` / ``on`` / ``yes``).
 *
 * Env wins over the setting so CI / E2E harnesses can force a value without
 * mutating workspace settings. The flag alone never enables sharing — every
 * gate check must still pass (Requirement 2.9).
 */
export function resolveMcpSharedHttpFlag(): boolean {
  const env = (process.env[SHARED_HTTP_FLAG_ENV] ?? "").trim().toLowerCase();
  if (env === "1" || env === "true" || env === "on" || env === "yes") {
    return true;
  }
  if (env === "0" || env === "false" || env === "off" || env === "no") {
    return false;
  }
  const vscode = tryGetVscode();
  if (!vscode) {
    // Outside a VS Code host (unit tests / plain Node) the default is OFF.
    return false;
  }
  try {
    return (
      vscode.workspace
        .getConfiguration("cognis")
        .get<boolean>("mcpSharedHttpEnabled", false) === true
    );
  } catch {
    return false;
  }
}

/**
 * Load gate evidence from, in order:
 * 1. ``COGNIS_MCP_SHARING_GATE_EVIDENCE`` — absolute/relative path to a JSON
 *    file, or an inline JSON object string;
 * 2. ``<repoRoot>/.cognis/sharing-gate-evidence.json`` when ``repoRoot`` is
 *    provided and the file exists.
 *
 * Missing / unreadable sources contribute nothing (fail-closed). Sources are
 * merged with later sources overriding earlier ones for the same check id.
 */
export function loadGateEvidence(
  repoRoot?: string
): Partial<Record<GateCheckId, GateCheckEvidence>> {
  let merged: Partial<Record<GateCheckId, GateCheckEvidence>> = {};

  const envValue = (process.env[GATE_EVIDENCE_ENV] ?? "").trim();
  if (envValue) {
    if (envValue.startsWith("{")) {
      merged = { ...merged, ...parseGateEvidenceJson(envValue) };
    } else {
      try {
        const text = fs.readFileSync(envValue, "utf8");
        merged = { ...merged, ...parseGateEvidenceJson(text) };
      } catch {
        // Unreadable path → no evidence from this source.
      }
    }
  }

  if (repoRoot) {
    const filePath = path.join(repoRoot, ".cognis", GATE_EVIDENCE_FILENAME);
    try {
      if (fs.existsSync(filePath)) {
        const text = fs.readFileSync(filePath, "utf8");
        merged = { ...merged, ...parseGateEvidenceJson(text) };
      }
    } catch {
      // Unreadable file → no evidence from this source.
    }
  }

  return merged;
}

/**
 * Resolve the live sharing-gate decision for the current process / workspace.
 *
 * Combines ``resolveMcpSharedHttpFlag`` with ``loadGateEvidence``. This is the
 * single entry point used by config writers and the Start-MCP-server flow.
 */
export function resolveSharingGate(repoRoot?: string): SharingGateDecision {
  const flagEnabled = resolveMcpSharedHttpFlag();
  const evidence = loadGateEvidence(repoRoot);
  return evaluateSharingGate(flagEnabled, evidence);
}

/**
 * True when the live configuration allows writing / starting shared HTTP for
 * ``repoRoot``. Fail-closed: default OFF, missing evidence → false.
 */
export function isLiveSharedHttpAllowed(repoRoot?: string): boolean {
  return resolveSharingGate(repoRoot).sharingEnabled;
}
