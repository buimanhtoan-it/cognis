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
