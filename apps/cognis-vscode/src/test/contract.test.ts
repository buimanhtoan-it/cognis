import assert from "node:assert/strict";
import test from "node:test";

import {
  EXPECTED_CONTRACT_VERSION,
  evaluateHandshake,
  handshakeWarning,
  type HandshakePayload,
} from "../contract";

function payload(over: Partial<HandshakePayload> = {}): HandshakePayload {
  return {
    contract_version: EXPECTED_CONTRACT_VERSION,
    engine_version: "0.6.2",
    cli_commands: ["init", "bootstrap", "health", "paths", "doctor", "mcp-config", "index"],
    mcp_tools: [
      "diffuse_context",
      "symbol_lookup",
      "symbol_search",
      "discover_symbols",
      "semantic_search",
      "resolve_symbols",
      "dependency_trace",
      "retrieve_context_capsule",
    ],
    ...over,
  };
}

test("a matching backend is ok and usable with no warning", () => {
  const result = evaluateHandshake(payload());
  assert.equal(result.compatibility, "ok");
  assert.equal(result.usable, true);
  assert.equal(handshakeWarning(result), undefined);
});

test("an older backend contract is flagged but still usable", () => {
  const result = evaluateHandshake(payload({ contract_version: EXPECTED_CONTRACT_VERSION - 1 }));
  assert.equal(result.compatibility, "backend-older");
  assert.equal(result.usable, true);
  assert.match(handshakeWarning(result) ?? "", /older|Install Backend/i);
});

test("a newer backend contract asks the user to update the extension", () => {
  const result = evaluateHandshake(payload({ contract_version: EXPECTED_CONTRACT_VERSION + 1 }));
  assert.equal(result.compatibility, "backend-newer");
  assert.equal(result.usable, true);
  assert.match(handshakeWarning(result) ?? "", /extension/i);
});

test("a missing required command blocks (capabilities-missing)", () => {
  const result = evaluateHandshake(
    payload({ cli_commands: ["init", "bootstrap", "health", "paths", "doctor"] })
  );
  assert.equal(result.compatibility, "capabilities-missing");
  assert.equal(result.usable, false);
  assert.deepEqual(result.missingCommands, ["mcp-config"]);
});

test("a missing required MCP tool blocks (capabilities-missing)", () => {
  const result = evaluateHandshake(
    payload({ mcp_tools: ["symbol_lookup", "symbol_search", "semantic_search"] })
  );
  assert.equal(result.compatibility, "capabilities-missing");
  assert.equal(result.usable, false);
  assert.deepEqual(result.missingTools, ["diffuse_context"]);
});

test("an unreadable handshake (no contract_version) is not usable", () => {
  const result = evaluateHandshake({ ...payload(), contract_version: undefined as unknown as number });
  assert.equal(result.compatibility, "unreadable");
  assert.equal(result.usable, false);
  assert.match(handshakeWarning(result) ?? "", /reinstall/i);
});

// ---------------------------------------------------------------------------
// Engine-build version gate. The contract version stays 1 across engine
// releases, so a stale binary (e.g. 0.8.4 while the extension ships 0.8.10)
// passes the contract-version check but runs old code. evaluateHandshake now
// also compares the engine build version when the extension passes its own.
// ---------------------------------------------------------------------------

test("a stale engine build (older than the extension) is flagged as engine-outdated", () => {
  const result = evaluateHandshake(payload({ engine_version: "0.8.4" }), "0.8.10");
  assert.equal(result.compatibility, "engine-outdated");
  assert.equal(result.usable, true);
  assert.equal(result.engineVersion, "0.8.4");
  assert.equal(result.expectedEngineVersion, "0.8.10");
  assert.match(handshakeWarning(result) ?? "", /0\.8\.4.*0\.8\.10|Install Backend/i);
});

test("a matching engine build stays ok", () => {
  const result = evaluateHandshake(payload({ engine_version: "0.8.10" }), "0.8.10");
  assert.equal(result.compatibility, "ok");
  assert.equal(result.usable, true);
  assert.equal(handshakeWarning(result), undefined);
});

test("an engine build newer than the extension asks the user to update the extension", () => {
  const result = evaluateHandshake(payload({ engine_version: "0.9.0" }), "0.8.10");
  assert.equal(result.compatibility, "engine-newer");
  assert.equal(result.usable, true);
  assert.match(handshakeWarning(result) ?? "", /extension/i);
});

test("engine version is ignored when the extension does not supply an expected version", () => {
  // Back-compat: callers that don't pass an expected engine version get the
  // old contract-only behaviour (no engine skew detection).
  const result = evaluateHandshake(payload({ engine_version: "0.8.4" }));
  assert.equal(result.compatibility, "ok");
  assert.equal(result.usable, true);
});

test("engine skew only applies once the contract version matches", () => {
  // A contract mismatch takes precedence — we don't downgrade a real contract
  // problem to a mere engine-version note.
  const result = evaluateHandshake(
    payload({ contract_version: EXPECTED_CONTRACT_VERSION + 1, engine_version: "0.8.4" }),
    "0.8.10"
  );
  assert.equal(result.compatibility, "backend-newer");
});

test("engine version comparison ignores a leading v and pre-release suffix", () => {
  assert.equal(
    evaluateHandshake(payload({ engine_version: "v0.8.10" }), "0.8.10").compatibility,
    "ok"
  );
  assert.equal(
    evaluateHandshake(payload({ engine_version: "0.8.10-rc1" }), "0.8.10").compatibility,
    "ok"
  );
});
