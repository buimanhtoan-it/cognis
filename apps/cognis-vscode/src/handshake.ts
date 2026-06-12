import { runCliJson } from "./cli";
import { trace } from "./diagnostics";
import {
  evaluateHandshake,
  type HandshakePayload,
  type HandshakeResult,
} from "./contract";

/**
 * Run the backend handshake for *repoRoot* and evaluate compatibility.
 *
 * Returns ``undefined`` when the backend can't be reached (not installed yet on
 * a fresh machine, or spawn failed) — the caller treats that as "no verdict"
 * rather than a mismatch. Always records the outcome to the diagnostics trace so
 * a skew that bites in production is reconstructable after the fact.
 */
export async function performHandshake(
  repoRoot: string
): Promise<HandshakeResult | undefined> {
  let payload: HandshakePayload;
  try {
    payload = await runCliJson<HandshakePayload>(repoRoot, ["handshake"]);
  } catch (err) {
    trace.warn("handshake", "backend handshake unavailable", {
      error: err instanceof Error ? err.message : String(err),
    });
    return undefined;
  }
  const result = evaluateHandshake(payload);
  const data = {
    compatibility: result.compatibility,
    backendContractVersion: result.backendContractVersion,
    expectedContractVersion: result.expectedContractVersion,
    engineVersion: result.engineVersion,
    missingCommands: result.missingCommands,
    missingTools: result.missingTools,
  };
  if (result.compatibility === "ok") {
    trace.info("handshake", "backend contract matches", data);
  } else {
    trace.error("handshake", "backend contract mismatch", data);
  }
  return result;
}
