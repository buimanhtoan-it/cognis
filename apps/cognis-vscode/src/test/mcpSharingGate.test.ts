/**
 * Unit tests for the reversible sharing gate (Task 7.3 / Requirement 2.9).
 *
 * Property-based / E2E coverage lives in task 7.4; these tests pin the pure
 * fail-closed evaluation and evidence parsing that every call site depends on.
 */
import assert from "node:assert/strict";
import test from "node:test";

import {
  evaluateGateCheck,
  evaluateSharingGate,
  isSharedHttpAllowed,
  parseGateEvidenceDocument,
  parseGateEvidenceJson,
  REQUIRED_GATE_CHECKS,
  selectSharingTopology,
  type GateCheckEvidence,
  type GateCheckId,
} from "../mcpSharingGate";

/** Build a full evidence map where every required check passes with a pointer. */
function allPassingEvidence(): Record<GateCheckId, GateCheckEvidence> {
  const out = {} as Record<GateCheckId, GateCheckEvidence>;
  for (const id of REQUIRED_GATE_CHECKS) {
    out[id] = { passed: true, evidence: `test:${id}` };
  }
  return out;
}

// ---------------------------------------------------------------------------
// evaluateGateCheck — fail-closed per check
// ---------------------------------------------------------------------------

test("evaluateGateCheck fails when evidence is missing", () => {
  const r = evaluateGateCheck("semanticParity", undefined);
  assert.equal(r.passed, false);
  assert.match(r.reason ?? "", /missing evidence/i);
});

test("evaluateGateCheck fails when passed is false", () => {
  const r = evaluateGateCheck("semanticParity", {
    passed: false,
    evidence: "run-1",
    detail: "parity delta",
  });
  assert.equal(r.passed, false);
  assert.equal(r.evidence, "run-1");
});

test("evaluateGateCheck fails when passed is true but evidence pointer is empty", () => {
  const r = evaluateGateCheck("eightToolContracts", {
    passed: true,
    evidence: "   ",
  });
  assert.equal(r.passed, false);
  assert.match(r.reason ?? "", /evidence pointer/i);
});

test("evaluateGateCheck passes only with true + non-empty pointer", () => {
  const r = evaluateGateCheck("repositoryIsolation", {
    passed: true,
    evidence: "isolation.e2e#cross-repo",
  });
  assert.equal(r.passed, true);
  assert.equal(r.evidence, "isolation.e2e#cross-repo");
});

// ---------------------------------------------------------------------------
// evaluateSharingGate — flag default OFF, failed gate → stdio
// ---------------------------------------------------------------------------

test("gate defaults to thin-proxy-stdio when flag is OFF even with full evidence", () => {
  const decision = evaluateSharingGate(false, allPassingEvidence());
  assert.equal(decision.topology, "thin-proxy-stdio");
  assert.equal(decision.flagEnabled, false);
  assert.equal(decision.sharingEnabled, false);
  assert.ok(decision.fallbackReason);
  assert.match(decision.fallbackReason!, /flag is OFF/i);
  assert.equal(decision.checks.length, REQUIRED_GATE_CHECKS.length);
});

test("gate stays closed when flag is ON but evidence is empty", () => {
  const decision = evaluateSharingGate(true, {});
  assert.equal(decision.topology, "thin-proxy-stdio");
  assert.equal(decision.flagEnabled, true);
  assert.equal(decision.sharingEnabled, false);
  assert.match(decision.fallbackReason ?? "", /gate checks failed/i);
  assert.equal(
    decision.checks.filter((c) => !c.passed).length,
    REQUIRED_GATE_CHECKS.length
  );
});

test("gate stays closed when flag is ON and any single check fails", () => {
  const evidence = allPassingEvidence();
  evidence.modelFingerprintIsolation = {
    passed: false,
    evidence: "fingerprint-mismatch",
    detail: "dim mismatch",
  };
  const decision = evaluateSharingGate(true, evidence);
  assert.equal(decision.topology, "thin-proxy-stdio");
  assert.equal(decision.sharingEnabled, false);
  assert.match(decision.fallbackReason ?? "", /modelFingerprintIsolation/);
  assert.match(decision.fallbackReason ?? "", /no data loss/i);
});

test("gate opens shared-http only when flag is ON and all checks pass", () => {
  const decision = evaluateSharingGate(true, allPassingEvidence());
  assert.equal(decision.topology, "shared-http");
  assert.equal(decision.flagEnabled, true);
  assert.equal(decision.sharingEnabled, true);
  assert.equal(decision.fallbackReason, undefined);
  assert.ok(decision.checks.every((c) => c.passed));
});

test("selectSharingTopology and isSharedHttpAllowed mirror evaluateSharingGate", () => {
  assert.equal(selectSharingTopology(false, allPassingEvidence()), "thin-proxy-stdio");
  assert.equal(isSharedHttpAllowed(false, allPassingEvidence()), false);
  assert.equal(selectSharingTopology(true, allPassingEvidence()), "shared-http");
  assert.equal(isSharedHttpAllowed(true, allPassingEvidence()), true);
  assert.equal(isSharedHttpAllowed(true, {}), false);
});

// ---------------------------------------------------------------------------
// Evidence document parsing
// ---------------------------------------------------------------------------

test("parseGateEvidenceDocument accepts flat and wrapped forms", () => {
  const flat = parseGateEvidenceDocument({
    semanticParity: { passed: true, evidence: "a" },
    eightToolContracts: { passed: false, evidence: "b" },
  });
  assert.equal(flat.semanticParity?.passed, true);
  assert.equal(flat.eightToolContracts?.passed, false);

  const wrapped = parseGateEvidenceDocument({
    checks: {
      semanticParity: { passed: true, evidence: "c" },
    },
  });
  assert.equal(wrapped.semanticParity?.evidence, "c");
});

test("parseGateEvidenceJson returns empty map on invalid JSON (fail-closed)", () => {
  assert.deepEqual(parseGateEvidenceJson("not-json"), {});
  assert.deepEqual(parseGateEvidenceJson(""), {});
});

test("REQUIRED_GATE_CHECKS lists exactly the seven Requirement 2.9 checks", () => {
  assert.deepEqual(
    [...REQUIRED_GATE_CHECKS].sort(),
    [
      "cancellationFailure",
      "concurrentLoadEviction",
      "eightToolContracts",
      "modelFingerprintIsolation",
      "processPrivateByteImprovement",
      "repositoryIsolation",
      "semanticParity",
    ].sort()
  );
});
