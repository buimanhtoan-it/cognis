/**
 * Extension ↔ backend contract handshake (TypeScript side).
 *
 * The backend advertises the contract it implements via `cognis-cli handshake`
 * (see packages/core/cognis/contract.py). This module holds the version the
 * extension was *built against* and the pure logic that compares the two, so a
 * version-skewed install — the most common production state, and the one the
 * matched-version e2e suite never exercises — is detected and surfaced instead
 * of failing silently downstream.
 *
 * Keep EXPECTED_CONTRACT_VERSION in lockstep with CONTRACT_VERSION in
 * contract.py: bump both together whenever the cross-process JSON shape changes.
 */

/** The contract version this extension build understands. */
export const EXPECTED_CONTRACT_VERSION = 1;

/** CLI commands the extension invokes and therefore depends on. */
export const REQUIRED_CLI_COMMANDS = [
  "init",
  "bootstrap",
  "health",
  "paths",
  "doctor",
  "mcp-config",
] as const;

/** MCP tools the extension assumes the server exposes. */
export const REQUIRED_MCP_TOOLS = [
  "diffuse_context",
  "symbol_lookup",
  "symbol_search",
  "semantic_search",
] as const;

/** JSON from `cognis-cli handshake`. */
export interface HandshakePayload {
  contract_version: number;
  engine_version: string;
  cli_commands: string[];
  mcp_tools: string[];
}

export type ContractCompatibility =
  | "ok"
  | "backend-older"
  | "backend-newer"
  | "capabilities-missing"
  | "unreadable";

export interface HandshakeResult {
  compatibility: ContractCompatibility;
  /** The contract version the backend reported (undefined when unreadable). */
  backendContractVersion?: number;
  /** The contract version the extension expects. */
  expectedContractVersion: number;
  engineVersion?: string;
  /** Required CLI commands the backend did not advertise. */
  missingCommands: string[];
  /** Required MCP tools the backend did not advertise. */
  missingTools: string[];
  /** True when the extension can operate normally against this backend. */
  usable: boolean;
}

function missingFrom(required: readonly string[], advertised: unknown): string[] {
  const have = new Set(Array.isArray(advertised) ? advertised.map(String) : []);
  return required.filter((name) => !have.has(name));
}

/**
 * Compare a backend handshake against what this extension expects. Pure (no I/O)
 * so the decision matrix is unit-testable.
 *
 * - Same contract version + all required capabilities present → ``ok``.
 * - Backend contract older/newer than expected → flagged so the UI can prompt
 *   the matching update (this is the version-skew case).
 * - Required command/tool missing → ``capabilities-missing`` (a partial backend).
 *
 * ``usable`` stays true for a pure version-number drift when all required
 * capabilities are still advertised — we warn but don't block — and only goes
 * false when a capability the extension actually calls is absent.
 */
export function evaluateHandshake(payload: HandshakePayload): HandshakeResult {
  const backendContractVersion =
    typeof payload?.contract_version === "number"
      ? payload.contract_version
      : undefined;
  const missingCommands = missingFrom(REQUIRED_CLI_COMMANDS, payload?.cli_commands);
  const missingTools = missingFrom(REQUIRED_MCP_TOOLS, payload?.mcp_tools);

  if (backendContractVersion === undefined) {
    return {
      compatibility: "unreadable",
      expectedContractVersion: EXPECTED_CONTRACT_VERSION,
      missingCommands,
      missingTools,
      usable: false,
    };
  }

  const base = {
    backendContractVersion,
    expectedContractVersion: EXPECTED_CONTRACT_VERSION,
    engineVersion: payload.engine_version,
    missingCommands,
    missingTools,
  };

  if (missingCommands.length > 0 || missingTools.length > 0) {
    return { ...base, compatibility: "capabilities-missing", usable: false };
  }
  if (backendContractVersion < EXPECTED_CONTRACT_VERSION) {
    return { ...base, compatibility: "backend-older", usable: true };
  }
  if (backendContractVersion > EXPECTED_CONTRACT_VERSION) {
    return { ...base, compatibility: "backend-newer", usable: true };
  }
  return { ...base, compatibility: "ok", usable: true };
}

/** A short, user-facing explanation for a non-ok handshake, or undefined for ok. */
export function handshakeWarning(result: HandshakeResult): string | undefined {
  switch (result.compatibility) {
    case "ok":
      return undefined;
    case "backend-older":
      return (
        `The Cognis backend (contract v${result.backendContractVersion}) is older than this ` +
        `extension (v${result.expectedContractVersion}). Update the backend so features match — ` +
        "run Cognis: Install Backend."
      );
    case "backend-newer":
      return (
        `The Cognis backend (contract v${result.backendContractVersion}) is newer than this ` +
        `extension (v${result.expectedContractVersion}). Update the Cognis extension so they match.`
      );
    case "capabilities-missing": {
      const parts: string[] = [];
      if (result.missingCommands.length) {
        parts.push(`commands: ${result.missingCommands.join(", ")}`);
      }
      if (result.missingTools.length) {
        parts.push(`tools: ${result.missingTools.join(", ")}`);
      }
      return (
        "The Cognis backend is missing capabilities this extension needs " +
        `(${parts.join("; ")}). Reinstall the matching backend — run Cognis: Install Backend.`
      );
    }
    case "unreadable":
      return (
        "Could not read the Cognis backend handshake (the backend may be too old or broken). " +
        "Reinstall it — run Cognis: Install Backend."
      );
    default:
      return undefined;
  }
}
