/**
 * Self-tests for the shared `fast-check` generators (task 16.1).
 *
 * A generator is test infrastructure, and untested test infrastructure is the
 * cheapest possible way to make a property suite look green while proving
 * nothing. These tests assert the three things a generator can silently get
 * wrong:
 *
 *   1. **Contract** — every produced value is well-formed against the shape the
 *      design specifies (a `StatusInput` that never carries capabilities would
 *      make P26's `Ready_Full` conjunction unreachable).
 *   2. **Coverage** — the trap cases the design names really are produced. Every
 *      capability combination, every degradation flag, mid-transition
 *      interruptions, alias spellings, marked-for-another-workspace entries.
 *      These are the cases a naive generator omits.
 *   3. **Model soundness** — the reference models exported alongside the
 *      generators (`applyToggleEvents`, `satisfiesReadyFullConjunction`,
 *      `isMarkedForWorkspace`) satisfy the acceptance criteria they encode, so
 *      a property comparing an implementation against them is comparing against
 *      something correct.
 *
 * These generators feed tasks 16.7–16.13; the components they describe land in
 * tasks 16.2–16.6.
 *
 * Validates: Requirements 2.1, 1.4, 21.7
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  ALL_TRANSITION_PHASES,
  BLOCKING_CONDITIONS,
  CAPABILITY_COMBINATIONS,
  COGNIS_MANAGED_BY,
  COGNIS_MANAGED_BY_VALUE,
  COGNIS_MANAGED_WORKSPACE,
  COVERAGE_RUNS,
  DEFAULT_TOGGLE_STATE,
  DEGRADATION_FLAGS,
  OTHER_WORKSPACE,
  OTHER_WORKSPACE_KEY,
  PRESERVED_ENTRY_KINDS,
  PRIMARY_WORKSPACE_KEY,
  PROPERTY_RUNS,
  REMOVABLE_ENTRY_KINDS,
  STATUS_SOURCES,
  STATUS_STATES,
  TOGGLE_STATES,
  TURN_OFF_ORDER,
  TURN_ON_ORDER,
  WORKSPACE_ALIASES,
  applyToggleEvents,
  arbInterruptedToggleSequence,
  arbMcpConfigDoc,
  arbStatusInput,
  arbToggleEventSequence,
  arbUnreadableStatusInput,
  arbAdvancedModeSequence,
  canonicalWorkspaceKey,
  displayedValue,
  isMarkedForWorkspace,
  preservedEntries,
  quiescentWorkspaces,
  removableEntries,
  satisfiesReadyFullConjunction,
  serializeMcpConfigDoc,
  type McpConfigDoc,
  type StatusInput,
  type ToggleEvent,
} from "./generators";

// ---------------------------------------------------------------------------
// Shared: the workspace-key mirror
// ---------------------------------------------------------------------------

test("every WORKSPACE_ALIASES spelling collapses to one canonical key", () => {
  const keys = new Set(WORKSPACE_ALIASES.map(canonicalWorkspaceKey));
  assert.equal(
    keys.size,
    1,
    `aliases must share one key, got ${[...keys].join(", ")}`
  );
  assert.equal([...keys][0], PRIMARY_WORKSPACE_KEY);
});

test("OTHER_WORKSPACE is genuinely a different key", () => {
  assert.notEqual(OTHER_WORKSPACE_KEY, PRIMARY_WORKSPACE_KEY);
  assert.equal(canonicalWorkspaceKey(OTHER_WORKSPACE), OTHER_WORKSPACE_KEY);
});

// ---------------------------------------------------------------------------
// arb_status_input — contract
// ---------------------------------------------------------------------------

test("arb_status_input: every value is a well-formed StatusInput", () => {
  fc.assert(
    fc.property(arbStatusInput(), (input: StatusInput) => {
      assert.ok(Array.isArray(input.readErrors));
      for (const source of input.readErrors) {
        assert.ok(
          (STATUS_SOURCES as readonly string[]).includes(source),
          `readErrors must name a known source, got "${source}"`
        );
      }
      assert.equal(
        new Set(input.readErrors).size,
        input.readErrors.length,
        "readErrors must not repeat a source"
      );
      if (input.transition !== undefined) {
        assert.ok(ALL_TRANSITION_PHASES.includes(input.transition));
      }
      if (input.health) {
        assert.ok(input.health.capabilities, "health must carry capabilities");
        assert.ok(
          (input.health.pending_vector_count ?? 0) >= 0,
          "pending_vector_count is a count"
        );
        // The coupling the storage layer guarantees: the semantic generation
        // never runs ahead of the source generation.
        assert.ok(
          (input.health.semantic_generation ?? 0) <=
            (input.health.source_generation ?? 0),
          "semantic_generation must not exceed source_generation"
        );
      }
      if (input.owner) {
        // The pre-feature IndexStatusReport keys survive (design: superset).
        for (const key of [
          "active",
          "phase",
          "message",
          "pendingCount",
          "pendingFiles",
          "inflightCount",
          "inflightFiles",
          "recentFiles",
          "updatedAt",
        ]) {
          assert.ok(
            key in input.owner,
            `owner-status.json must preserve the pre-feature key "${key}"`
          );
        }
        assert.ok((input.owner.watcherQueueDepth ?? 0) >= 0);
      }
      if (input.lease) {
        assert.ok(input.lease.pid > 0);
        assert.equal(typeof input.lease.owner_nonce, "string");
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_status_input: semantic_available implies a compiled backend and a matching fingerprint", () => {
  // The trap this guards against is a generator that claims semantic search is
  // available while the backend is absent — an unreachable state that would let
  // a broken Ready_Full derivation "pass".
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      if (input.health?.semantic_available) {
        assert.equal(input.health.capabilities?.semantic_backend, true);
        assert.equal(input.health.fingerprint_match, true);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

// ---------------------------------------------------------------------------
// arb_status_input — coverage of the design's named cases
// ---------------------------------------------------------------------------

test("arb_status_input: produces every capability combination", () => {
  const seen = new Set<string>();
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      const caps = input.health?.capabilities;
      if (caps) {
        seen.add(`${caps.semantic_backend}/${caps.accelerated_vector_search}`);
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  assert.equal(
    seen.size,
    CAPABILITY_COMBINATIONS.length,
    `expected all ${CAPABILITY_COMBINATIONS.length} capability combinations, saw ${[
      ...seen,
    ].join(", ")}`
  );
});

test("arb_status_input: produces every degradation flag", () => {
  const seen = new Set<string>();
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      const health = input.health;
      if (!health || input.readErrors.length > 0 || input.transition) {
        return;
      }
      if (health.capabilities && !health.capabilities.semantic_backend) {
        seen.add("semantic_backend_absent");
      }
      if ((health.pending_vector_count ?? 0) > 0) {
        seen.add("vectors_pending");
      }
      if ((health.semantic_generation ?? 0) < (health.source_generation ?? 0)) {
        seen.add("semantic_generation_behind");
      }
      if (health.fingerprint_match === false) {
        seen.add("fingerprint_refused");
      }
      if (
        health.capabilities?.semantic_backend &&
        !health.capabilities.accelerated_vector_search
      ) {
        seen.add("vector_search_fallback");
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  for (const flag of DEGRADATION_FLAGS) {
    assert.ok(seen.has(flag), `degradation flag "${flag}" was never generated`);
  }
});

test("arb_status_input: produces every blocking condition", () => {
  const seen = new Set<string>();
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      const health = input.health;
      if (!health) {
        return;
      }
      if ((health.missing_indexes ?? []).length > 0) {
        seen.add("missing_required_index");
      }
      if (health.checks.db?.status === "fail") {
        seen.add("database_unwritable");
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  // `checksum_verification_failed` is an Activation_Manager fact, not a health
  // fact, so it reaches the derivation through `transition` rather than
  // `health`; assert the two health-borne conditions here and the transition
  // coverage separately below.
  for (const condition of ["missing_required_index", "database_unwritable"]) {
    assert.ok(
      seen.has(condition),
      `blocking condition "${condition}" was never generated`
    );
  }
  assert.ok(BLOCKING_CONDITIONS.includes("checksum_verification_failed"));
});

test("arb_status_input: reaches all eight source-presence combinations and the transition branch", () => {
  const presence = new Set<string>();
  let sawTransition = false;
  let sawUnreadableWithReadableSiblings = false;
  let sawReadyFullCandidate = false;
  let sawNearMissReadyFull = false;

  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      presence.add(
        `${Boolean(input.health)}/${Boolean(input.owner)}/${Boolean(input.lease)}`
      );
      if (input.transition) {
        sawTransition = true;
      }
      if (input.readErrors.length > 0 && input.health) {
        sawUnreadableWithReadableSiblings = true;
      }
      if (satisfiesReadyFullConjunction(input)) {
        sawReadyFullCandidate = true;
      } else if (
        input.readErrors.length === 0 &&
        input.transition === undefined &&
        input.health?.capabilities?.semantic_backend &&
        input.health.semantic_generation === input.health.source_generation &&
        input.health.fingerprint_match === true
      ) {
        // Four of the five conjuncts hold — the case that catches a derivation
        // which forgot `pending_vector_count === 0` or `semantic_available`.
        sawNearMissReadyFull = true;
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );

  assert.equal(
    presence.size,
    8,
    `expected all 8 source-presence combinations, saw ${[...presence].join(", ")}`
  );
  assert.ok(sawTransition, "no in-transition input was generated");
  assert.ok(
    sawUnreadableWithReadableSiblings,
    "no input combined an unreadable source with a readable one — the stale-Ready trap"
  );
  assert.ok(sawReadyFullCandidate, "no Ready_Full-eligible input was generated");
  assert.ok(
    sawNearMissReadyFull,
    "no near-miss Ready_Full input was generated — the conjunction would be untested"
  );
});

test("arb_status_input: exercises every transition phase in both directions", () => {
  const seen = new Set<string>();
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      if (input.transition) {
        seen.add(input.transition);
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  for (const phase of [...TURN_ON_ORDER, ...TURN_OFF_ORDER]) {
    assert.ok(seen.has(phase), `transition phase "${phase}" was never generated`);
  }
});

test("arbUnreadableStatusInput always carries at least one unreadable source", () => {
  fc.assert(
    fc.property(arbUnreadableStatusInput(), (input) => {
      assert.ok(input.readErrors.length > 0);
      assert.ok(
        input.readErrors.every((source) =>
          (STATUS_SOURCES as readonly string[]).includes(source)
        )
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("satisfiesReadyFullConjunction is false whenever a source is unreadable or a transition runs", () => {
  fc.assert(
    fc.property(arbStatusInput(), (input) => {
      if (input.readErrors.length > 0 || input.transition !== undefined) {
        assert.equal(satisfiesReadyFullConjunction(input), false);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("the four Status_State values are exactly the design's set", () => {
  assert.deepEqual(
    [...STATUS_STATES].sort(),
    ["Needs_Attention", "Ready_Full", "Ready_Lexical", "Working"]
  );
});

// ---------------------------------------------------------------------------
// arb_toggle_event_sequence — contract
// ---------------------------------------------------------------------------

test("arb_toggle_event_sequence: every event is well-formed", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events: ToggleEvent[]) => {
      assert.ok(events.length > 0);
      for (const event of events) {
        assert.ok(
          [...WORKSPACE_ALIASES, OTHER_WORKSPACE].includes(event.workspace),
          `unknown workspace spelling "${event.workspace}"`
        );
        if (event.kind === "request") {
          assert.ok(TOGGLE_STATES.includes(event.value));
          assert.ok(["user", "settingsSync"].includes(event.origin));
          assert.equal(typeof event.duringTransition, "boolean");
        }
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_toggle_event_sequence: duringTransition is a fact, not a guess", () => {
  // The `duringTransition` flag is recorded by the sequence *builder* from its
  // own bookkeeping. Every Requirement 1.8 assertion written against that flag
  // is worthless if the bookkeeping is wrong, so cross-check it against the
  // reference model — an independently written implementation of the same
  // semantics. A disagreement means one of the two is wrong, which is exactly
  // what we want to hear about.
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const { steps } = applyToggleEvents(events);
      for (const [index, event] of events.entries()) {
        if (event.kind !== "request") {
          continue;
        }
        const key = canonicalWorkspaceKey(event.workspace);
        const before = index === 0 ? undefined : steps[index - 1].workspaces.get(key);
        const modelSaysRunning = before?.inFlight !== undefined;
        assert.equal(
          event.duringTransition,
          modelSaysRunning,
          `event ${index}: builder recorded duringTransition=${event.duringTransition} but the reference model has inFlight=${String(before?.inFlight)}`
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

// ---------------------------------------------------------------------------
// arb_toggle_event_sequence — coverage
// ---------------------------------------------------------------------------

test("arb_toggle_event_sequence: produces requests, settles, reopens, both origins, and interruptions", () => {
  const kinds = new Set<string>();
  const origins = new Set<string>();
  let sawInterruption = false;
  let sawNoOpRequest = false;
  let sawMultipleAliases = false;

  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const spellings = new Set<string>();
      for (const event of events) {
        kinds.add(event.kind);
        spellings.add(event.workspace);
        if (event.kind === "request") {
          origins.add(event.origin);
          if (event.duringTransition) {
            sawInterruption = true;
          }
        }
      }
      const aliasSpellings = [...spellings].filter((spelling) =>
        WORKSPACE_ALIASES.includes(spelling)
      );
      if (aliasSpellings.length > 1) {
        sawMultipleAliases = true;
      }
      // A no-op request: one that leaves the model completely unchanged.
      const { steps } = applyToggleEvents(events);
      for (const [index, event] of events.entries()) {
        if (event.kind !== "request" || index === 0) {
          continue;
        }
        const before = steps[index - 1].workspaces.get(
          canonicalWorkspaceKey(event.workspace)
        );
        const after = steps[index].workspaces.get(
          canonicalWorkspaceKey(event.workspace)
        );
        if (
          before &&
          after &&
          before.persisted === after.persisted &&
          before.inFlight === after.inFlight &&
          after.inFlight === undefined
        ) {
          sawNoOpRequest = true;
        }
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );

  assert.deepEqual([...kinds].sort(), ["reopen", "request", "settle"]);
  assert.deepEqual([...origins].sort(), ["settingsSync", "user"]);
  assert.ok(sawInterruption, "no mid-transition interruption was generated");
  assert.ok(sawNoOpRequest, "no Requirement 1.7 no-op request was generated");
  assert.ok(
    sawMultipleAliases,
    "no sequence addressed one workspace through several spellings"
  );
});

test("arbInterruptedToggleSequence always contains a mid-transition request", () => {
  fc.assert(
    fc.property(arbInterruptedToggleSequence(), (events) => {
      assert.ok(
        events.some(
          (event) => event.kind === "request" && event.duringTransition
        )
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

// ---------------------------------------------------------------------------
// arb_toggle_event_sequence — reference-model soundness
// ---------------------------------------------------------------------------

test("applyToggleEvents: alias spellings collapse to exactly one persisted entry (Req 1.4)", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const { final } = applyToggleEvents(events);
      const touchedAliases = events.some((event) =>
        WORKSPACE_ALIASES.includes(event.workspace)
      );
      const keys = [...final.workspaces.keys()];
      if (touchedAliases) {
        assert.equal(
          keys.filter((key) => key === PRIMARY_WORKSPACE_KEY).length,
          1,
          `expected exactly one entry for the aliased workspace, got ${keys.join(", ")}`
        );
      }
      // Never more entries than there are distinct real workspaces.
      assert.ok(keys.length <= 2, `unexpected entries: ${keys.join(", ")}`);
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("applyToggleEvents: displayed equals persisted at every quiescent point (Req 1.5)", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const { steps } = applyToggleEvents(events);
      for (const step of steps) {
        for (const key of quiescentWorkspaces(step)) {
          const state = step.workspaces.get(key)!;
          assert.equal(
            displayedValue(state),
            state.persisted,
            `quiescent workspace ${key} displays a value it did not persist`
          );
        }
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("applyToggleEvents: a request equal to the current value is a no-op (Req 1.7)", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const { steps } = applyToggleEvents(events);
      for (const [index, event] of events.entries()) {
        if (event.kind !== "request") {
          continue;
        }
        const key = canonicalWorkspaceKey(event.workspace);
        const before = index === 0 ? undefined : steps[index - 1].workspaces.get(key);
        const wasQuiescent = before === undefined || before.inFlight === undefined;
        const currentValue = before?.persisted ?? DEFAULT_TOGGLE_STATE;
        if (!wasQuiescent || currentValue !== event.value) {
          continue;
        }
        const after = steps[index].workspaces.get(key)!;
        assert.equal(
          after.inFlight,
          undefined,
          "setting the current value must not start a transition"
        );
        assert.equal(after.persisted, currentValue);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("applyToggleEvents: a mid-transition request converges on the newest value (Req 1.8)", () => {
  fc.assert(
    fc.property(arbInterruptedToggleSequence(), (events) => {
      const { steps } = applyToggleEvents(events);
      for (const [index, event] of events.entries()) {
        if (event.kind !== "request" || !event.duringTransition) {
          continue;
        }
        const key = canonicalWorkspaceKey(event.workspace);
        const after = steps[index].workspaces.get(key)!;
        assert.equal(
          after.inFlight,
          event.value,
          "the newest request must replace the in-flight target, not queue behind it"
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("applyToggleEvents: a reopen restores the persisted value and drops in-flight work (Req 1.4)", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const { steps } = applyToggleEvents(events);
      for (const [index, event] of events.entries()) {
        if (event.kind !== "reopen") {
          continue;
        }
        const key = canonicalWorkspaceKey(event.workspace);
        const after = steps[index].workspaces.get(key)!;
        assert.equal(after.inFlight, undefined);
        assert.ok(TOGGLE_STATES.includes(after.persisted));
        const before = index === 0 ? undefined : steps[index - 1].workspaces.get(key);
        assert.equal(
          after.persisted,
          before?.persisted ?? DEFAULT_TOGGLE_STATE,
          "a reopen must not change the persisted value"
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("applyToggleEvents is deterministic", () => {
  fc.assert(
    fc.property(arbToggleEventSequence(), (events) => {
      const a = applyToggleEvents(events).final;
      const b = applyToggleEvents(events).final;
      assert.deepEqual([...a.workspaces].sort(), [...b.workspaces].sort());
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

// ---------------------------------------------------------------------------
// arb_mcp_config_doc — contract
// ---------------------------------------------------------------------------

test("arb_mcp_config_doc: documents are valid JSON with unique server names", () => {
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document: McpConfigDoc) => {
      const serialized = serializeMcpConfigDoc(document.doc);
      const reparsed = JSON.parse(serialized) as Record<string, unknown>;
      assert.deepEqual(reparsed, document.doc);
      assert.ok(serialized.endsWith("\n"));

      const servers = document.doc.mcpServers as Record<string, unknown>;
      assert.ok(servers, "every document has an mcpServers object");
      const names = document.entries.map((entry) => entry.name);
      assert.equal(
        new Set(names).size,
        names.length,
        `duplicate server name in ${names.join(", ")}`
      );
      assert.deepEqual(Object.keys(servers), names, "entry order must match doc order");
      assert.ok(
        [PRIMARY_WORKSPACE_KEY, OTHER_WORKSPACE_KEY].includes(document.workspaceKey)
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_mcp_config_doc: every entry's marker state matches its declared kind", () => {
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      for (const entry of document.entries) {
        const marked = isMarkedForWorkspace(entry.block, document.workspaceKey);
        switch (entry.kind) {
          case "markedThisWorkspace":
            assert.ok(marked, `${entry.name} must carry this workspace's marker`);
            break;
          case "markedOtherWorkspace":
            assert.equal(
              marked,
              false,
              `${entry.name} carries a marker for another workspace and must not match`
            );
            assert.equal(
              entry.block.env?.[COGNIS_MANAGED_BY],
              COGNIS_MANAGED_BY_VALUE,
              "the other-workspace trap must still carry COGNIS_MANAGED_BY"
            );
            assert.notEqual(
              entry.block.env?.[COGNIS_MANAGED_WORKSPACE],
              document.workspaceKey
            );
            break;
          default:
            assert.equal(marked, false, `${entry.name} must carry no marker`);
            assert.equal(entry.block.env?.[COGNIS_MANAGED_BY], undefined);
            break;
        }
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_mcp_config_doc: the legacy name heuristic separates legacy entries from look-alikes", () => {
  // Mirrors `isCognisMcpServerName` — the fallback the design keeps for entries
  // written before the marker existed. A look-alike must never match it.
  const isLegacyCognisName = (name: string): boolean =>
    name === "cognis" || name.startsWith("cognis-");

  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      for (const entry of document.entries) {
        if (entry.kind === "legacyCognisName") {
          assert.ok(
            isLegacyCognisName(entry.name),
            `legacy entry "${entry.name}" must match the fallback heuristic`
          );
        }
        if (
          entry.kind === "foreign" ||
          entry.kind === "foreignCognisLookalike"
        ) {
          assert.equal(
            isLegacyCognisName(entry.name),
            false,
            `foreign entry "${entry.name}" must not match the fallback heuristic`
          );
        }
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_mcp_config_doc: every document contains at least one preserved entry", () => {
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      assert.ok(
        preservedEntries(document).length > 0,
        "a document with nothing to preserve makes P31 vacuous"
      );
      // Preserved and removable sets partition the entries.
      assert.equal(
        preservedEntries(document).length + removableEntries(document).length,
        document.entries.length
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

// ---------------------------------------------------------------------------
// arb_mcp_config_doc — coverage
// ---------------------------------------------------------------------------

test("arb_mcp_config_doc: produces every entry kind, both key orders, and both marker positions", () => {
  const kinds = new Set<string>();
  let sawMcpServersFirst = false;
  let sawMcpServersLast = false;
  let sawMarkerFirst = false;
  let sawMarkerLast = false;
  let sawForeignTopLevel = false;
  let sawEnvlessEntry = false;
  const sawBothWorkspaceScopes = new Set<string>();

  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      sawBothWorkspaceScopes.add(document.workspaceKey);
      const topKeys = Object.keys(document.doc);
      if (topKeys[0] === "mcpServers") {
        sawMcpServersFirst = true;
      } else {
        sawMcpServersLast = true;
      }
      if (topKeys.includes("$schema")) {
        sawForeignTopLevel = true;
      }
      for (const entry of document.entries) {
        kinds.add(entry.kind);
        const envKeys = Object.keys(entry.block.env ?? {});
        if (envKeys.length === 0) {
          sawEnvlessEntry = true;
        }
        if (envKeys[0] === COGNIS_MANAGED_BY) {
          sawMarkerFirst = true;
        } else if (envKeys.includes(COGNIS_MANAGED_BY)) {
          sawMarkerLast = true;
        }
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );

  for (const kind of [...PRESERVED_ENTRY_KINDS, ...REMOVABLE_ENTRY_KINDS]) {
    assert.ok(kinds.has(kind), `entry kind "${kind}" was never generated`);
  }
  assert.ok(sawMcpServersFirst && sawMcpServersLast, "key ordering was not varied");
  assert.ok(sawMarkerFirst && sawMarkerLast, "marker position in env was not varied");
  assert.ok(sawForeignTopLevel, "no document carried foreign top-level keys");
  assert.ok(sawEnvlessEntry, "no entry without an env block was generated");
  assert.equal(
    sawBothWorkspaceScopes.size,
    2,
    "removal was only ever scoped to one workspace"
  );
});

test("arb_mcp_config_doc: serialization is byte-stable across repeated calls", () => {
  // P31 compares bytes, so the generator's own serialization must be a
  // function of the document alone.
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      assert.equal(
        serializeMcpConfigDoc(document.doc),
        serializeMcpConfigDoc(document.doc)
      );
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("arb_mcp_config_doc: a marker-only removal would delete an other-workspace entry (trap is live)", () => {
  // A regression guard on the generator itself: if the other-workspace entries
  // ever stopped carrying COGNIS_MANAGED_BY, the workspace-scoping half of
  // Requirement 4.3 would silently stop being tested.
  let sawTrap = false;
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      for (const entry of document.entries) {
        if (
          entry.kind === "markedOtherWorkspace" &&
          entry.block.env?.[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE &&
          !isMarkedForWorkspace(entry.block, document.workspaceKey)
        ) {
          sawTrap = true;
        }
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  assert.ok(sawTrap, "the marked-for-another-workspace trap was never generated");
});

test("arb_advanced_mode_sequence: non-empty histories covering both polarities", () => {
  let sawTrue = false;
  let sawFalse = false;
  fc.assert(
    fc.property(arbAdvancedModeSequence(), (history) => {
      assert.ok(history.length >= 1, "activation value must always be present");
      for (const step of history) {
        assert.equal(typeof step.advancedMode, "boolean");
        assert.equal(typeof step.mcpServerRunning, "boolean");
        assert.equal(typeof step.syncPaused, "boolean");
        if (step.advancedMode) {
          sawTrue = true;
        } else {
          sawFalse = true;
        }
      }
    }),
    { numRuns: COVERAGE_RUNS }
  );
  assert.ok(sawTrue, "Advanced_Mode true was never generated");
  assert.ok(sawFalse, "Advanced_Mode false was never generated");
});
