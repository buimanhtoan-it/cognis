// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required by the ACTION_COMMANDS assertions below.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import type { ContractCompatibility, HandshakeResult } from "../contract";
import {
  compatibilityIdentity,
  compatibilityIdentityKey,
  compatibilitySnapshotFromHandshake,
  deriveRemediation,
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  isConfirmedCompatibility,
  isConfirmedMismatch,
  type CompatibilityRemediation,
  type CompatibilitySnapshot,
} from "../compatibility";
import { ACTION_COMMANDS } from "../panel";

function handshakeResult(
  compatibility: ContractCompatibility
): HandshakeResult {
  return {
    compatibility,
    backendContractVersion: 1,
    expectedContractVersion: 1,
    engineVersion: "0.8.10",
    expectedEngineVersion: "0.8.11",
    missingCommands: ["mcp-config"],
    missingTools: ["semantic_search"],
    usable: compatibility !== "unreadable",
  };
}

test("first-probe default is deterministic, unavailable, and immutable", () => {
  assert.deepEqual(FIRST_PROBE_COMPATIBILITY_SNAPSHOT, {
    phase: "unavailable",
    generation: 0,
    observedAt: 0,
  });
  assert.equal(Object.isFrozen(FIRST_PROBE_COMPATIBILITY_SNAPSHOT), true);
  assert.equal(isConfirmedCompatibility(FIRST_PROBE_COMPATIBILITY_SNAPSHOT), false);
  assert.equal(isConfirmedMismatch(FIRST_PROBE_COMPATIBILITY_SNAPSHOT), false);
});

test("maps a HandshakeResult to a confirmed snapshot without losing result data", () => {
  const result = handshakeResult("capabilities-missing");
  const snapshot = compatibilitySnapshotFromHandshake(result, 7, 1_730_000_000_000);

  assert.deepEqual(snapshot, {
    phase: "confirmed",
    result,
    generation: 7,
    observedAt: 1_730_000_000_000,
  });
  assert.strictEqual(snapshot.result, result);
  assert.deepEqual(snapshot.result.missingCommands, ["mcp-config"]);
  assert.deepEqual(snapshot.result.missingTools, ["semantic_search"]);
});

const COMPATIBILITY_KINDS: ContractCompatibility[] = [
  "ok",
  "backend-older",
  "backend-newer",
  "engine-outdated",
  "engine-newer",
  "capabilities-missing",
  "unreadable",
];

for (const kind of COMPATIBILITY_KINDS) {
  test(`confirmed ${kind} is ${kind === "ok" ? "not " : ""}a mismatch`, () => {
    const snapshot = compatibilitySnapshotFromHandshake(
      handshakeResult(kind),
      1,
      100
    );

    assert.equal(isConfirmedCompatibility(snapshot), true);
    assert.equal(isConfirmedMismatch(snapshot), kind !== "ok");
  });
}

test("checking and unavailable states do not imply a confirmed verdict", () => {
  const snapshots: CompatibilitySnapshot[] = [
    { phase: "checking", generation: 2, observedAt: 0 },
    { phase: "unavailable", generation: 3, observedAt: 200 },
  ];

  for (const snapshot of snapshots) {
    assert.equal(isConfirmedCompatibility(snapshot), false);
    assert.equal(isConfirmedMismatch(snapshot), false);
    assert.equal("result" in snapshot, false);
  }
});

test("confirmed type narrowing exposes the complete HandshakeResult", () => {
  const snapshot: CompatibilitySnapshot = compatibilitySnapshotFromHandshake(
    handshakeResult("engine-outdated"),
    4,
    300
  );

  assert.equal(isConfirmedMismatch(snapshot), true);
  if (!isConfirmedMismatch(snapshot)) {
    assert.fail("expected a confirmed mismatch");
  }

  assert.equal(snapshot.result.compatibility, "engine-outdated");
  assert.equal(snapshot.result.engineVersion, "0.8.10");
  assert.equal(snapshot.result.expectedEngineVersion, "0.8.11");
  assert.equal(snapshot.result.expectedContractVersion, 1);
});

// ---------------------------------------------------------------------------
// deriveRemediation: the canonical 1:1 decision table for every
// Compatibility_Kind (Requirement 3.4–3.7, design.md "deriveRemediation").
//
// Validates: Requirements 3.4, 3.5, 3.6, 3.7, 3.8, 4.1
//
// Every kind maps 1:1 to the correct remediation (id/label/destructive); labels
// use the user terminology Engine/Extension and never "Backend"; only the
// `unreadable`/Repair Engine case is destructive (modal-gated); `ok` returns
// undefined so the operational control stays in effect; and every actionId
// resolves to a registered command via ACTION_COMMANDS.
// ---------------------------------------------------------------------------

interface RemediationDecisionCase {
  kind: ContractCompatibility;
  expected: CompatibilityRemediation | undefined;
}

/**
 * The complete decision table, one row per Compatibility_Kind. This is the
 * canonical oracle for `deriveRemediation` — kept exhaustive so a new kind (or
 * a changed mapping) breaks a test rather than silently slipping through.
 */
const REMEDIATION_DECISION_TABLE: RemediationDecisionCase[] = [
  {
    kind: "engine-outdated",
    expected: { actionId: "installBackend", label: "Update Engine", destructive: false },
  },
  {
    kind: "backend-older",
    expected: { actionId: "installBackend", label: "Update Engine", destructive: false },
  },
  {
    kind: "capabilities-missing",
    expected: { actionId: "installBackend", label: "Update Engine", destructive: false },
  },
  {
    kind: "engine-newer",
    expected: { actionId: "updateExtension", label: "Update Extension", destructive: false },
  },
  {
    kind: "backend-newer",
    expected: { actionId: "updateExtension", label: "Update Extension", destructive: false },
  },
  {
    kind: "unreadable",
    expected: { actionId: "reinstallEngine", label: "Repair Engine", destructive: true },
  },
  { kind: "ok", expected: undefined },
];

// The decision table must stay exhaustive: one row for every Compatibility_Kind
// the contract defines, no more, no less.
test("deriveRemediation decision table covers every Compatibility_Kind exactly once", () => {
  const tableKinds = REMEDIATION_DECISION_TABLE.map((row) => row.kind).sort();
  const allKinds = [...COMPATIBILITY_KINDS].sort();
  assert.deepEqual(tableKinds, allKinds);
  assert.equal(new Set(tableKinds).size, tableKinds.length, "duplicate kind in table");
});

/** Remediations that resolve to a non-destructive Engine update. */
const UPDATE_ENGINE_KINDS = new Set<ContractCompatibility>([
  "engine-outdated",
  "backend-older",
  "capabilities-missing",
]);
/** Remediations that resolve to a non-destructive Extension update. */
const UPDATE_EXTENSION_KINDS = new Set<ContractCompatibility>([
  "engine-newer",
  "backend-newer",
]);

/** Commands the Compatibility_Primary_Action must never resolve to (R3.7). */
const FORBIDDEN_ACTION_IDS = new Set([
  "coldRestart",
  "clearReindex",
  "remove",
  "prepareUninstall",
]);

for (const { kind, expected } of REMEDIATION_DECISION_TABLE) {
  test(`deriveRemediation(${kind}) maps 1:1 to the canonical remediation`, () => {
    const remediation = deriveRemediation(handshakeResult(kind));

    // Exact id/label/destructive triple — the whole row at once.
    assert.deepEqual(remediation, expected);

    if (expected === undefined) {
      // `ok` yields no remediation so the operational control stays in effect.
      assert.equal(kind, "ok");
      return;
    }

    // Label uses user terminology and never leaks the internal "Backend" word.
    assert.doesNotMatch(
      remediation!.label,
      /backend/i,
      `label "${remediation!.label}" must not contain "Backend"`
    );

    // Engine/Extension update kinds are non-destructive; only Repair Engine
    // (unreadable) is destructive and therefore modal-gated.
    if (UPDATE_ENGINE_KINDS.has(kind)) {
      assert.equal(remediation!.actionId, "installBackend");
      assert.equal(remediation!.label, "Update Engine");
      assert.equal(remediation!.destructive, false);
    } else if (UPDATE_EXTENSION_KINDS.has(kind)) {
      assert.equal(remediation!.actionId, "updateExtension");
      assert.equal(remediation!.label, "Update Extension");
      assert.equal(remediation!.destructive, false);
    } else {
      assert.equal(kind, "unreadable");
      assert.equal(remediation!.actionId, "reinstallEngine");
      assert.equal(remediation!.label, "Repair Engine");
      assert.equal(remediation!.destructive, true);
    }

    // R3.7: actionId is one of the three permitted remediation commands and
    // never a Cold Restart / rebuild / remove action.
    assert.ok(
      ["installBackend", "updateExtension", "reinstallEngine"].includes(
        remediation!.actionId
      ),
      `actionId "${remediation!.actionId}" is not a permitted remediation command`
    );
    assert.equal(
      FORBIDDEN_ACTION_IDS.has(remediation!.actionId),
      false,
      `actionId "${remediation!.actionId}" is a forbidden destructive command`
    );

    // R3.8: every actionId resolves to a registered command via ACTION_COMMANDS.
    assert.ok(
      Object.prototype.hasOwnProperty.call(ACTION_COMMANDS, remediation!.actionId),
      `actionId "${remediation!.actionId}" is not a key of ACTION_COMMANDS`
    );
    assert.equal(typeof ACTION_COMMANDS[remediation!.actionId], "string");
    assert.match(ACTION_COMMANDS[remediation!.actionId], /^cognis\./);
  });
}

test("deriveRemediation never resolves to a forbidden destructive command", () => {
  for (const kind of COMPATIBILITY_KINDS) {
    const remediation = deriveRemediation(handshakeResult(kind));
    if (!remediation) {
      continue;
    }
    assert.equal(
      FORBIDDEN_ACTION_IDS.has(remediation.actionId),
      false,
      `${kind} resolved to forbidden command "${remediation.actionId}"`
    );
  }
});

test("deriveRemediation labels never contain 'Backend' across all kinds", () => {
  for (const kind of COMPATIBILITY_KINDS) {
    const remediation = deriveRemediation(handshakeResult(kind));
    if (!remediation) {
      continue;
    }
    assert.doesNotMatch(remediation.label, /backend/i, `${kind} label leaks "Backend"`);
  }
});

test("only the unreadable/Repair Engine remediation is destructive", () => {
  for (const kind of COMPATIBILITY_KINDS) {
    const remediation = deriveRemediation(handshakeResult(kind));
    if (!remediation) {
      continue;
    }
    assert.equal(
      remediation.destructive,
      kind === "unreadable",
      `${kind} has unexpected destructive flag`
    );
  }
});

// ---------------------------------------------------------------------------
// Compatibility_Identity dedupe key (task 5.1): a stable key that dedupes one
// notification per identity per session, and re-prompts when the actionable
// skew (kind or version pair) changes.
// ---------------------------------------------------------------------------

test("compatibilityIdentity projects the full actionable skew", () => {
  const result = handshakeResult("engine-outdated");
  assert.deepEqual(compatibilityIdentity(result), {
    kind: "engine-outdated",
    engineVersion: "0.8.10",
    expectedEngineVersion: "0.8.11",
    backendContractVersion: 1,
    expectedContractVersion: 1,
  });
});

test("identity key is stable for the same verdict", () => {
  const a = compatibilityIdentityKey(handshakeResult("engine-outdated"));
  const b = compatibilityIdentityKey(handshakeResult("engine-outdated"));
  assert.equal(a, b);
});

test("identity key differs when the compatibility kind changes", () => {
  const outdated = compatibilityIdentityKey(handshakeResult("engine-outdated"));
  const newer = compatibilityIdentityKey(handshakeResult("engine-newer"));
  assert.notEqual(outdated, newer);
});

test("identity key differs when the engine version pair changes", () => {
  const base = handshakeResult("engine-outdated");
  const bumped: HandshakeResult = { ...base, engineVersion: "0.8.12" };
  assert.notEqual(
    compatibilityIdentityKey(base),
    compatibilityIdentityKey(bumped),
    "a later engine bump must yield a new identity so it re-prompts"
  );
});

test("identity key differs when the contract version pair changes", () => {
  const base = handshakeResult("backend-older");
  const bumped: HandshakeResult = { ...base, backendContractVersion: 2 };
  assert.notEqual(
    compatibilityIdentityKey(base),
    compatibilityIdentityKey(bumped)
  );
});

test("distinct kinds and version pairs never collide in the key", () => {
  const keys = new Set<string>();
  const kinds: ContractCompatibility[] = [
    "backend-older",
    "backend-newer",
    "engine-outdated",
    "engine-newer",
    "capabilities-missing",
    "unreadable",
  ];
  for (const kind of kinds) {
    keys.add(compatibilityIdentityKey(handshakeResult(kind)));
  }
  assert.equal(keys.size, kinds.length, "each kind must produce a unique key");
});
