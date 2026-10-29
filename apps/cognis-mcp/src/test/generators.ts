/**
 * Shared `fast-check` generators for the `cognis-one-toggle-engine` property
 * suite — the extension (TypeScript) side.
 *
 * **Dev-only.** Nothing in `src/extension.ts`'s import graph reaches this file;
 * it is imported exclusively from `src/test/*.test.ts`. It is the TypeScript
 * analogue of `crates/cognis-testgen/` (task 2.1) and follows the same rules:
 * one generator per design-table row, each documented with the property it
 * feeds, each producing the *trap* cases rather than only the easy ones, and
 * each with self-tests that prove the generator's own contract.
 *
 * Design reference: `.kiro/specs/cognis-one-toggle-engine/design.md`,
 * section "Generators".
 *
 * | Generator | Produces | Feeds |
 * | --- | --- | --- |
 * | {@link arbStatusInput} | full and partial `StatusInput`s including unreadable sources, every capability combination, and every degradation flag | P26, P27 |
 * | {@link arbToggleEventSequence} | toggle requests, reopens, settings syncs, and mid-transition interruptions | P28 |
 * | {@link arbMcpConfigDoc} | MCP config documents with foreign entries, legacy name-only Cognis entries, marked entries, and unusual key ordering | P31 |
 * | {@link arbAdvancedModeSequence} | `Advanced_Mode` values at activation plus later setting changes, with independent MCP/sync context | P34 |
 *
 * _Requirements: 2.1, 1.4, 21.7_
 *
 * # Types that land in later tasks
 *
 * `Status_Surface` (`src/status.ts`, task 16.2) now owns `StatusInput` /
 * `TransitionPhase`; `StatusState`, `StatusReport`, and `Capabilities` live in
 * `src/types.ts`. `Toggle_Control` (`src/toggle.ts`, task 16.3) owns
 * `ToggleState` and the canonical workspace key. Remaining consumers land in
 * later tasks: the two managers (tasks 16.5, 16.6).
 * Every value shape they will consume is mirrored
 * here as a **self-contained type carrying the design's exact field names**,
 * each with a `TODO(task 16.N)` note. When the production type lands the mirror
 * is re-exported from / aliased to it rather than replaced, so the property
 * tests written against these generators keep compiling.
 *
 * The one production type that already exists — {@link LeaseRecord} — is
 * imported rather than mirrored, so the generator cannot drift from it.
 */
import fc from "fast-check";

import type { LeaseRecord } from "../lease";
import {
  COGNIS_MANAGED_BY,
  COGNIS_MANAGED_BY_VALUE,
  COGNIS_MANAGED_WORKSPACE,
} from "../mcpManagedEntry";
import type {
  Capabilities,
  HealthReport,
  OwnerStatusReport,
  StatusState,
} from "../types";
import type { StatusInput, TransitionPhase } from "../status";
import {
  DEFAULT_TOGGLE_STATE,
  toggleWorkspaceKey,
  type ToggleState,
} from "../toggle";

export type { ToggleState } from "../toggle";
export { DEFAULT_TOGGLE_STATE, toggleWorkspaceKey } from "../toggle";
export type { Capabilities, StatusState } from "../types";
export type { StatusInput, TransitionPhase } from "../status";
export {
  COGNIS_MANAGED_BY,
  COGNIS_MANAGED_BY_VALUE,
  COGNIS_MANAGED_WORKSPACE,
} from "../mcpManagedEntry";

/**
 * Minimum iteration count for every property test in this feature (design:
 * "at a minimum of 100 iterations"). Exported so tasks 16.7–16.13 and 17.3–17.4
 * cannot silently under-run.
 */
export const PROPERTY_RUNS = 100;

/**
 * One observation of `Advanced_Mode` plus the other live `when`-clause
 * contexts that view/title menus combine with it. Palette and panel
 * visibility ignore the extra flags; view/title uses them. Feeding all three
 * together is what lets P34 prove that extra context cannot resurrect a
 * hidden Advanced_Command when the setting is off.
 */
export interface AdvancedModeStep {
  advancedMode: boolean;
  mcpServerRunning: boolean;
  syncPaused: boolean;
}

/**
 * Sequences of `Advanced_Mode` changes, including the value at activation.
 *
 * Feeds P34 (Advanced visibility is a pure function of the setting). The
 * first element is the activation-time value; later elements are live
 * setting changes. Empty histories are excluded so "current value" is always
 * defined.
 */
export function arbAdvancedModeSequence(): fc.Arbitrary<AdvancedModeStep[]> {
  const step = fc.record({
    advancedMode: fc.boolean(),
    mcpServerRunning: fc.boolean(),
    syncPaused: fc.boolean(),
  });
  return fc
    .tuple(step, fc.array(step, { minLength: 0, maxLength: 64 }))
    .map(([activation, changes]) => [activation, ...changes]);
}

/**
 * Iteration count for the generator self-tests that assert *coverage* (every
 * capability combination appears, every entry kind appears, …). Coverage claims
 * need more samples than a per-case invariant, so they get their own budget.
 */
export const COVERAGE_RUNS = 400;

// ===========================================================================
// Section 0 — shared workspace spellings
// ===========================================================================

/**
 * Canonical-workspace key — production {@link toggleWorkspaceKey} from
 * `src/toggle.ts`. Alias spellings collapse to one persisted entry
 * (Requirement 1.4).
 */
export function canonicalWorkspaceKey(spelling: string): string {
  return toggleWorkspaceKey(spelling);
}

/**
 * Five spellings of **one** workspace. Every one of these must collapse to a
 * single persisted entry (Requirement 1.4, property P28's "alias spellings
 * collapsing to one entry"); a `Map` keyed on the raw string would produce five.
 */
export const WORKSPACE_ALIASES: readonly string[] = [
  "D:\\work\\my-app",
  "d:\\work\\my-app",
  "D:/work/my-app",
  "D:\\work\\my-app\\",
  "\\\\?\\D:\\work\\my-app",
];

/**
 * A genuinely different workspace. Present so a property cannot pass by
 * collapsing *everything* into one entry — the failure mode opposite to the one
 * {@link WORKSPACE_ALIASES} catches.
 */
export const OTHER_WORKSPACE = "D:\\work\\other-app";

/** Canonical key shared by every member of {@link WORKSPACE_ALIASES}. */
export const PRIMARY_WORKSPACE_KEY = canonicalWorkspaceKey(WORKSPACE_ALIASES[0]);

/** Canonical key of {@link OTHER_WORKSPACE}. */
export const OTHER_WORKSPACE_KEY = canonicalWorkspaceKey(OTHER_WORKSPACE);

/** Any spelling of either workspace. */
export const arbWorkspaceSpelling: fc.Arbitrary<string> = fc.constantFrom(
  ...WORKSPACE_ALIASES,
  OTHER_WORKSPACE
);

// ===========================================================================
// Section 1 — `arb_status_input`
//
// Design: "full and partial `StatusInput`s including unreadable sources, all
// capability combinations, and every degradation flag". Feeds P26 (status
// soundness and totality) and P27 (a failing reporting path reports itself).
//
// Three things make this generator load-bearing rather than decorative:
//
//   1. **It generates the absence of things.** `health`, `owner`, and `lease`
//      are each independently optional, so the 8 presence combinations —
//      including all-absent — are all reachable. `deriveStatusState` must still
//      return exactly one state and must not throw (Requirement 2.1); a
//      generator that always supplied a full input would never reach the branch
//      where a derivation reads `input.health!.checks.db`.
//   2. **It generates disagreement between sources.** `health` and `owner` each
//      carry generations; a real system can have a stale `owner-status.json`
//      whose `sourceGeneration` differs from what health reports. Generating
//      them independently is what makes the derivation's choice of source of
//      truth observable.
//   3. **It generates the Ready_Full trap.** Requirement 2.2 is a five-way
//      conjunction. The weighting below deliberately produces many inputs that
//      satisfy four of the five conjuncts, so a derivation that forgot one
//      conjunct reports `Ready_Full` on a case this generator produces.
//
// ===========================================================================

/** The four states, for exhaustiveness assertions in P26. */
export const STATUS_STATES: readonly StatusState[] = [
  "Ready_Lexical",
  "Ready_Full",
  "Working",
  "Needs_Attention",
];

type HealthReportWithCapabilities = HealthReport;

/** The Activation_Manager's canonical turn-on order (Requirement 3.9). */
export const TURN_ON_ORDER: readonly TransitionPhase[] = [
  "verify_engine_binary",
  "verify_model_assets",
  "open_database",
  "reconcile_and_index",
  "start_watcher",
  "write_mcp_entry",
];

/** The Deactivation_Manager's order (Requirement 4.1–4.3). */
export const TURN_OFF_ORDER: readonly TransitionPhase[] = [
  "stop_watcher",
  "stop_daemons",
  "remove_mcp_entry",
];

/** Every phase, both directions. */
export const ALL_TRANSITION_PHASES: readonly TransitionPhase[] = [
  ...TURN_ON_ORDER,
  ...TURN_OFF_ORDER,
];

/**
 * The named sources the Status_Surface reads. Requirement 2.9 requires an
 * unreadable one to be *named* in the report, so the generator draws
 * `readErrors` from exactly this closed set — a property can then assert the
 * report contains the name, which a free-text generator could not support.
 */
export const STATUS_SOURCES = [
  "health",
  "owner-status.json",
  "owner.lease",
  "workspace-database",
] as const;

export type StatusSource = (typeof STATUS_SOURCES)[number];

/**
 * All four capability combinations, exhaustively. `constantFrom` over the
 * explicit tuple rather than two independent booleans, so the self-test can
 * assert coverage of every combination rather than hoping for it.
 *
 * `accelerated_vector_search: false` with `semantic_backend: true` is the
 * fallback case Requirement 6.5 / 14.3 want surfaced as "vector search running
 * in fallback mode" — a `Ready_Lexical` reason that is easy to forget because
 * semantic search still *works*.
 */
export const CAPABILITY_COMBINATIONS: readonly Capabilities[] = [
  { semantic_backend: false, accelerated_vector_search: false },
  { semantic_backend: false, accelerated_vector_search: true },
  { semantic_backend: true, accelerated_vector_search: false },
  { semantic_backend: true, accelerated_vector_search: true },
];

export const arbCapabilities: fc.Arbitrary<Capabilities> = fc.constantFrom(
  ...CAPABILITY_COMBINATIONS
);

/**
 * The five reasons Requirement 2.3 / 6.5 give for `Ready_Lexical` instead of
 * `Ready_Full`. Named so a property can assert the detail line identifies one.
 */
export const DEGRADATION_FLAGS = [
  "semantic_backend_absent",
  "vectors_pending",
  "semantic_generation_behind",
  "fingerprint_refused",
  "vector_search_fallback",
] as const;

export type DegradationFlag = (typeof DEGRADATION_FLAGS)[number];

/**
 * The blocking conditions of Requirement 2.5 — each needs a user decision, so
 * each must produce `Needs_Attention` with exactly one next action.
 */
export const BLOCKING_CONDITIONS = [
  "missing_required_index",
  "checksum_verification_failed",
  "database_unwritable",
] as const;

export type BlockingCondition = (typeof BLOCKING_CONDITIONS)[number];

const OK_CHECK = { status: "ok", message: "ok" } as const;

function healthChecks(
  overrides: Record<string, { status: string; message: string }> = {}
): Record<string, { status: string; message: string }> {
  return {
    config: OK_CHECK,
    db: OK_CHECK,
    index: OK_CHECK,
    vector: OK_CHECK,
    embedder: OK_CHECK,
    version: OK_CHECK,
    ...overrides,
  };
}

/**
 * A lease record. Both live (`expiry` in the future) and expired leases are
 * generated: an expired lease means "no owner", which is a different world from
 * "no lease file at all", and the derivation has to tell them apart.
 */
export const arbLeaseRecord: fc.Arbitrary<LeaseRecord> = fc
  .record({
    pid: fc.integer({ min: 1, max: 200_000 }),
    nonce: fc.hexaString({ minLength: 8, maxLength: 16 }),
    startId: fc.hexaString({ minLength: 4, maxLength: 12 }),
    heartbeatAt: fc.integer({ min: 1_700_000_000, max: 1_800_000_000 }),
    live: fc.boolean(),
  })
  .map(({ pid, nonce, startId, heartbeatAt, live }) => ({
    owner_nonce: nonce,
    pid,
    process_start_id: startId,
    heartbeat_at: heartbeatAt,
    // A live lease expires after the heartbeat; an expired one already has.
    expiry: live ? heartbeatAt + 30 : heartbeatAt - 30,
  }));

/**
 * Generations plus the derived counters, generated as a **coupled** tuple.
 *
 * Coupling matters: `semanticGeneration <= sourceGeneration` always holds in a
 * real database (the semantic generation only advances to a committed source
 * generation), and `pendingVectorCount > 0` is exactly the condition under
 * which the semantic generation lags. Generating the three independently would
 * spend most of the budget on states the storage layer cannot produce, and
 * would let a property "pass" on an unreachable input.
 *
 * The `behind` / `pending` shapes are generated on purpose so `Ready_Full`'s
 * conjunction is stressed from each direction separately.
 */
interface GenerationTriple {
  sourceGeneration: number;
  semanticGeneration: number;
  pendingVectorCount: number;
}

const arbGenerationTriple: fc.Arbitrary<GenerationTriple> = fc.oneof(
  // Fully caught up — the only shape compatible with Ready_Full.
  {
    weight: 3,
    arbitrary: fc
      .integer({ min: 0, max: 5_000 })
      .map((g) => ({
        sourceGeneration: g,
        semanticGeneration: g,
        pendingVectorCount: 0,
      })),
  },
  // Caught up in generation but with vectors still queued — the trap: four of
  // Ready_Full's five conjuncts hold.
  {
    weight: 2,
    arbitrary: fc
      .tuple(fc.integer({ min: 0, max: 5_000 }), fc.integer({ min: 1, max: 10_000 }))
      .map(([g, pending]) => ({
        sourceGeneration: g,
        semanticGeneration: g,
        pendingVectorCount: pending,
      })),
  },
  // Semantic generation behind, with queued work to match.
  {
    weight: 2,
    arbitrary: fc
      .tuple(
        fc.integer({ min: 1, max: 5_000 }),
        fc.integer({ min: 1, max: 5_000 }),
        fc.integer({ min: 1, max: 10_000 })
      )
      .map(([g, lag, pending]) => ({
        sourceGeneration: g,
        semanticGeneration: Math.max(0, g - lag),
        pendingVectorCount: pending,
      })),
  },
  // A brand-new workspace: nothing committed yet.
  {
    weight: 1,
    arbitrary: fc.constant({
      sourceGeneration: 0,
      semanticGeneration: 0,
      pendingVectorCount: 0,
    }),
  }
);

/** A health report, optionally carrying a blocking condition. */
const arbHealth: fc.Arbitrary<HealthReportWithCapabilities> = fc
  .record({
    capabilities: arbCapabilities,
    generations: arbGenerationTriple,
    semanticAvailable: fc.boolean(),
    fingerprintMatch: fc.boolean(),
    overall: fc.constantFrom<"ok" | "warn" | "fail">("ok", "warn", "fail"),
    blocking: fc.option(fc.constantFrom(...BLOCKING_CONDITIONS), { nil: undefined }),
  })
  .map(
    ({
      capabilities,
      generations,
      semanticAvailable,
      fingerprintMatch,
      overall,
      blocking,
    }): HealthReportWithCapabilities => ({
      runtime_version: "0.9.0",
      overall,
      checks: healthChecks(
        blocking === "database_unwritable"
          ? { db: { status: "fail", message: "database is not writable" } }
          : {}
      ),
      capabilities,
      source_generation: generations.sourceGeneration,
      semantic_generation: generations.semanticGeneration,
      pending_vector_count: generations.pendingVectorCount,
      // A withheld vector space or an absent backend both mean "no semantic".
      semantic_available:
        semanticAvailable && capabilities.semantic_backend && fingerprintMatch,
      fingerprint_match: fingerprintMatch,
      missing_indexes:
        blocking === "missing_required_index" ? ["idx_symbol_name_lower"] : [],
    })
  );

/** An `owner-status.json` view, generated independently of health on purpose. */
const arbOwnerStatus: fc.Arbitrary<OwnerStatusReport> = fc
  .record({
    active: fc.boolean(),
    phase: fc.constantFrom(
      "watching",
      "idle",
      "stopped",
      "provisioning",
      "indexing",
      "embedding",
      "incremental"
    ),
    pendingCount: fc.nat({ max: 500 }),
    inflightCount: fc.nat({ max: 16 }),
    watcherQueueDepth: fc.nat({ max: 8_192 }),
    generations: arbGenerationTriple,
    modelResident: fc.boolean(),
    fingerprintMatch: fc.boolean(),
    capabilities: arbCapabilities,
    workspace: fc.constantFrom(PRIMARY_WORKSPACE_KEY, OTHER_WORKSPACE_KEY),
  })
  .map(
    ({
      active,
      phase,
      pendingCount,
      inflightCount,
      watcherQueueDepth,
      generations,
      modelResident,
      fingerprintMatch,
      capabilities,
      workspace,
    }): OwnerStatusReport => ({
      active,
      phase,
      message: "",
      pendingCount,
      pendingFiles: [],
      inflightCount,
      inflightFiles: [],
      recentFiles: [],
      updatedAt: 1_730_000_000,
      sourceGeneration: generations.sourceGeneration,
      semanticGeneration: generations.semanticGeneration,
      pendingVectorCount: generations.pendingVectorCount,
      watcherQueueDepth,
      modelResident,
      fingerprintMatch,
      capabilities,
      canonicalWorkspace: workspace,
    })
  );

/**
 * A non-empty, duplicate-free subset of {@link STATUS_SOURCES}.
 *
 * `subarray` preserves the source order and never repeats, so the "names every
 * unreadable source" assertion in P27 is a set comparison rather than a
 * multiset one.
 */
export const arbUnreadableSources: fc.Arbitrary<StatusSource[]> = fc.subarray(
  [...STATUS_SOURCES],
  { minLength: 1 }
);

/**
 * Which of the three optional sources are present, as an explicit mask.
 *
 * Three independent `fc.option`s would make the all-absent input a ~1-in-125
 * draw — and all-absent is precisely the input that tests totality
 * (Requirement 2.1: "exactly one Status_State … and never throws"). Enumerating
 * the eight subsets makes every presence combination, including that one,
 * uniformly reachable.
 */
type PresenceMask = readonly [boolean, boolean, boolean];

const PRESENCE_MASKS: readonly PresenceMask[] = [
  [false, false, false],
  [false, false, true],
  [false, true, false],
  [false, true, true],
  [true, false, false],
  [true, false, true],
  [true, true, false],
  [true, true, true],
];

const arbPresenceMask: fc.Arbitrary<PresenceMask> = fc.constantFrom(
  ...PRESENCE_MASKS
);

function applyPresenceMask(
  [hasHealth, hasOwner, hasLease]: PresenceMask,
  health: HealthReportWithCapabilities,
  owner: OwnerStatusReport,
  lease: LeaseRecord
): Pick<StatusInput, "health" | "owner" | "lease"> {
  return {
    health: hasHealth ? health : undefined,
    owner: hasOwner ? owner : undefined,
    lease: hasLease ? lease : undefined,
  };
}

/**
 * `arb_status_input` — the whole `StatusInput` space.
 *
 * Weighted 4 : 3 : 2 : 2 across four regimes:
 *
 * - **Complete** inputs (every source readable, no transition). These reach the
 *   `Ready_Full` / `Ready_Lexical` decision, which is where the conjunction bug
 *   lives.
 * - **Partial** inputs, drawn over an explicit **presence mask** so all eight
 *   subsets of `{health, owner, lease}` — including the all-absent input — are
 *   uniformly reachable. `readErrors` stays empty: the source was *readable*,
 *   it just is not there yet (no owner has ever run). Totality
 *   (Requirement 2.1) is what these test, and the all-absent case is its
 *   sharpest instance, so it is generated by construction rather than left to
 *   an `fc.option` coincidence.
 * - **Unreadable** inputs (`readErrors` non-empty). Requirement 2.9's
 *   self-reporting path. The remaining sources are *still* generated as
 *   present, because the dangerous bug is preferring a stale readable source
 *   over reporting the unreadable one.
 * - **In-transition** inputs (`transition` set). Requirement 2.4 / 2.6's
 *   `Working` branch, including — deliberately — inputs whose readable values
 *   would otherwise satisfy `Ready_Full`, so decision *order* is tested and not
 *   just the individual branches.
 */
export function arbStatusInput(): fc.Arbitrary<StatusInput> {
  const complete: fc.Arbitrary<StatusInput> = fc
    .record({
      health: arbHealth,
      owner: arbOwnerStatus,
      lease: arbLeaseRecord,
    })
    .map((parts) => ({ ...parts, readErrors: [] }));

  const partial: fc.Arbitrary<StatusInput> = fc
    .record({
      presence: arbPresenceMask,
      health: arbHealth,
      owner: arbOwnerStatus,
      lease: arbLeaseRecord,
    })
    .map(({ presence, health, owner, lease }) => ({
      ...applyPresenceMask(presence, health, owner, lease),
      readErrors: [],
    }));

  const unreadable: fc.Arbitrary<StatusInput> = fc
    .record({
      presence: arbPresenceMask,
      health: arbHealth,
      owner: arbOwnerStatus,
      lease: arbLeaseRecord,
      readErrors: arbUnreadableSources,
    })
    .map(({ presence, health, owner, lease, readErrors }) => ({
      ...applyPresenceMask(presence, health, owner, lease),
      readErrors: [...readErrors],
    }));

  const inTransition: fc.Arbitrary<StatusInput> = fc
    .record({
      presence: arbPresenceMask,
      health: arbHealth,
      owner: arbOwnerStatus,
      lease: arbLeaseRecord,
      transition: fc.constantFrom(...ALL_TRANSITION_PHASES),
      // A transition can coincide with an unreadable source; Requirement 2.9
      // outranks 2.4, so this combination has to be reachable.
      readErrors: fc.oneof(
        { weight: 3, arbitrary: fc.constant<StatusSource[]>([]) },
        { weight: 1, arbitrary: arbUnreadableSources }
      ),
    })
    .map(({ presence, health, owner, lease, transition, readErrors }) => ({
      ...applyPresenceMask(presence, health, owner, lease),
      transition,
      readErrors: [...readErrors],
    }));

  return fc.oneof(
    { weight: 4, arbitrary: complete },
    { weight: 3, arbitrary: partial },
    { weight: 2, arbitrary: unreadable },
    { weight: 2, arbitrary: inTransition }
  );
}

/**
 * A `StatusInput` guaranteed to carry at least one unreadable source — the
 * precondition of P27. Offered separately so that property does not spend 70 %
 * of its budget on `fc.pre` rejections.
 */
export function arbUnreadableStatusInput(): fc.Arbitrary<StatusInput> {
  return fc
    .record({
      presence: arbPresenceMask,
      health: arbHealth,
      owner: arbOwnerStatus,
      lease: arbLeaseRecord,
      transition: fc.option(fc.constantFrom(...ALL_TRANSITION_PHASES), {
        nil: undefined,
      }),
      readErrors: arbUnreadableSources,
    })
    .map(({ presence, health, owner, lease, transition, readErrors }) => ({
      ...applyPresenceMask(presence, health, owner, lease),
      transition,
      readErrors: [...readErrors],
    }));
}

/**
 * Whether an input satisfies every conjunct Requirement 2.2 demands for
 * `Ready_Full`. Exported so P26 states the conjunction *once*, here, rather
 * than restating (and possibly mis-stating) it inside the test.
 *
 * This is the generator's model of the requirement, deliberately independent of
 * `deriveStatusState`'s implementation — comparing the two is the point.
 */
export function satisfiesReadyFullConjunction(input: StatusInput): boolean {
  if (input.readErrors.length > 0 || input.transition !== undefined) {
    return false;
  }
  const health = input.health;
  if (!health || !health.capabilities) {
    return false;
  }
  return (
    health.semantic_available === true &&
    health.pending_vector_count === 0 &&
    health.semantic_generation === health.source_generation &&
    health.fingerprint_match === true &&
    health.capabilities.semantic_backend
  );
}

// ===========================================================================
// Section 2 — `arb_toggle_event_sequence`
//
// Design: "toggle requests, reopens, settings syncs, and mid-transition
// interruptions". Feeds P28 (toggle convergence and persistence).
//
// P28 has four clauses and each needs a *different* generated shape, so the
// sequence builder below is stateful rather than a flat `fc.array`:
//
//   - "applying the current state changes nothing" (Req 1.7) needs requests
//     that repeat the value already held. A uniformly random On/Off stream
//     produces those only half the time and never in runs, so the builder
//     biases towards repeats.
//   - "a mid-transition request cancels and converges on the newest value"
//     (Req 1.8) needs a request that arrives while a transition is in flight.
//     That can only be produced by tracking in-flight state while building —
//     which is why every generated event carries {@link ToggleEvent.duringTransition}
//     as a fact about the sequence, not a guess.
//   - "displayed equals persisted at every quiescent point" (Req 1.5) needs
//     transitions that actually settle, so `settle` events are generated.
//   - "the persisted value round-trips through a reopen with alias spellings
//     collapsing to one entry" (Req 1.4) needs the same workspace addressed
//     through several spellings — {@link WORKSPACE_ALIASES}.
//
// P28 compares {@link applyToggleEvents} against `ToggleControl` (task 16.9).
// {@link applyToggleEvents} is a *reference model* of the required semantics,
// written from the acceptance criteria and deliberately independent of the
// implementation — comparing the two is what P28 does.
// ===========================================================================

/** Both toggle values — exactly two, per Requirement 1.1. */
export const TOGGLE_STATES: readonly ToggleState[] = ["On", "Off"];

/** How a toggle value arrived. Requirement 1.5 names all three. */
export type ToggleOrigin = "user" | "settingsSync" | "reopen";

/**
 * One event in a generated sequence.
 *
 * - `request` — the user (or a settings sync) sets the control to `value`.
 * - `settle` — the in-flight transition for the workspace finishes. This is the
 *   only event that creates a quiescent point after a value change.
 * - `reopen` — the workspace is closed and reopened. Any in-flight transition
 *   is lost with the extension host; the persisted value must be restored.
 */
export type ToggleEvent =
  | {
      kind: "request";
      /** A possibly non-canonical spelling — see {@link WORKSPACE_ALIASES}. */
      workspace: string;
      value: ToggleState;
      origin: Exclude<ToggleOrigin, "reopen">;
      /**
       * True iff a transition for this workspace was already in flight when
       * this request was emitted. Recorded by the builder, so a property can
       * select the Requirement 1.8 cases without re-deriving them.
       */
      duringTransition: boolean;
    }
  | { kind: "settle"; workspace: string }
  | { kind: "reopen"; workspace: string };

/** Per-workspace state in the reference model. */
interface ToggleWorkspaceState {
  /** The value written to `context.workspaceState`. */
  persisted: ToggleState;
  /** The target of an in-flight transition, or `undefined` when quiescent. */
  inFlight?: ToggleState;
}

/** The reference model's view after applying a prefix of a sequence. */
export interface ToggleModelState {
  /** Canonical workspace key → state. Alias spellings share one entry. */
  workspaces: Map<string, ToggleWorkspaceState>;
}

/**
 * The value the control must *display* for a workspace: the in-flight target
 * while a transition runs (the newest request always wins, Requirement 1.8),
 * and the persisted value otherwise (Requirement 1.5).
 */
export function displayedValue(state: ToggleWorkspaceState): ToggleState {
  return state.inFlight ?? state.persisted;
}

/**
 * Reference model for the toggle semantics of Requirement 1.4, 1.5, 1.7, 1.8.
 *
 * Deliberately written from the acceptance criteria rather than from
 * `src/toggle.ts`:
 *
 * - a `request` whose value equals the currently displayed value while no
 *   transition runs is a **no-op** (1.7): no transition starts, so the point
 *   stays quiescent;
 * - a `request` during an in-flight transition **replaces** the target (1.8) —
 *   it never queues, so the sequence converges on the newest value;
 * - `settle` commits the in-flight target to the persisted value (1.5);
 * - `reopen` **discards** any in-flight transition and restores the persisted
 *   value (1.4) — the extension host is gone, so an un-settled transition
 *   cannot survive it.
 *
 * Returns the state after every prefix, so a property can assert the
 * "displayed equals persisted at every quiescent point" clause at each step
 * rather than only at the end.
 */
export function applyToggleEvents(events: readonly ToggleEvent[]): {
  final: ToggleModelState;
  /** State after each event, same length as `events`. */
  steps: ToggleModelState[];
} {
  const workspaces = new Map<string, ToggleWorkspaceState>();
  const steps: ToggleModelState[] = [];

  const stateFor = (spelling: string): ToggleWorkspaceState => {
    const key = canonicalWorkspaceKey(spelling);
    let entry = workspaces.get(key);
    if (!entry) {
      entry = { persisted: DEFAULT_TOGGLE_STATE };
      workspaces.set(key, entry);
    }
    return entry;
  };

  const snapshot = (): ToggleModelState => ({
    workspaces: new Map(
      [...workspaces].map(([key, value]) => [key, { ...value }])
    ),
  });

  for (const event of events) {
    const state = stateFor(event.workspace);
    switch (event.kind) {
      case "request": {
        if (state.inFlight === undefined && state.persisted === event.value) {
          // Requirement 1.7 — setting the current value leaves everything as it
          // is, including the absence of a transition.
          break;
        }
        // Requirement 1.8 — the newest request replaces any in-flight target.
        state.inFlight = event.value;
        break;
      }
      case "settle": {
        if (state.inFlight !== undefined) {
          state.persisted = state.inFlight;
          state.inFlight = undefined;
        }
        break;
      }
      case "reopen": {
        // The host is gone: an un-settled transition does not survive.
        state.inFlight = undefined;
        break;
      }
    }
    steps.push(snapshot());
  }

  return { final: snapshot(), steps };
}

/** Canonical keys with no transition in flight — the quiescent points. */
export function quiescentWorkspaces(state: ToggleModelState): string[] {
  return [...state.workspaces]
    .filter(([, value]) => value.inFlight === undefined)
    .map(([key]) => key);
}

/**
 * Build a sequence, tracking in-flight state so `duringTransition` is a fact
 * and mid-transition interruptions are guaranteed rather than hoped for.
 *
 * `seeds` drives the choices; drawing one integer per event and interpreting it
 * here (rather than composing arbitraries) keeps the in-flight bookkeeping in
 * one place and keeps shrinking well-behaved: dropping a trailing seed drops a
 * trailing event.
 */
function buildToggleSequence(
  seeds: readonly number[],
  spellings: readonly string[]
): ToggleEvent[] {
  const events: ToggleEvent[] = [];
  const inFlight = new Map<string, ToggleState>();
  const persisted = new Map<string, ToggleState>();

  for (const [index, seed] of seeds.entries()) {
    const spelling = spellings[index % spellings.length];
    const key = canonicalWorkspaceKey(spelling);
    const current = persisted.get(key) ?? DEFAULT_TOGGLE_STATE;
    const pending = inFlight.get(key);
    const running = pending !== undefined;

    switch (seed % 6) {
      // 0, 1 — a request for the *other* value: starts (or redirects) work.
      case 0:
      case 1: {
        const value: ToggleState = (pending ?? current) === "On" ? "Off" : "On";
        events.push({
          kind: "request",
          workspace: spelling,
          value,
          origin: seed % 2 === 0 ? "user" : "settingsSync",
          duringTransition: running,
        });
        inFlight.set(key, value);
        break;
      }
      // 2 — a request repeating the current value: Requirement 1.7's no-op.
      case 2: {
        const value = pending ?? current;
        events.push({
          kind: "request",
          workspace: spelling,
          value,
          origin: "user",
          duringTransition: running,
        });
        if (running) {
          inFlight.set(key, value);
        }
        break;
      }
      // 3, 4 — settle whatever is in flight (a no-op when nothing is).
      case 3:
      case 4: {
        events.push({ kind: "settle", workspace: spelling });
        if (pending !== undefined) {
          persisted.set(key, pending);
          inFlight.delete(key);
        }
        break;
      }
      // 5 — reopen, possibly mid-transition.
      default: {
        events.push({ kind: "reopen", workspace: spelling });
        inFlight.delete(key);
        break;
      }
    }
  }

  return events;
}

/**
 * `arb_toggle_event_sequence` — toggle requests, reopens, settings syncs, and
 * mid-transition interruptions.
 *
 * Two spelling regimes are generated, weighted equally:
 *
 * - **Alias-heavy**: every event addresses the *same* workspace through a
 *   different spelling from {@link WORKSPACE_ALIASES}. This is the regime in
 *   which "alias spellings collapse to one entry" (Requirement 1.4) is a real
 *   constraint — a `Map` keyed on the raw spelling ends with five entries and
 *   five independent values.
 * - **Mixed**: aliases plus a genuinely different workspace, so a
 *   implementation cannot pass by collapsing everything into a single entry.
 */
export function arbToggleEventSequence(): fc.Arbitrary<ToggleEvent[]> {
  return arbToggleEventSequenceSized(1, 24);
}

/** {@link arbToggleEventSequence} with a caller-chosen length, for properties
 * that want to pin one regime. */
export function arbToggleEventSequenceSized(
  minLength: number,
  maxLength: number
): fc.Arbitrary<ToggleEvent[]> {
  return fc
    .tuple(
      fc.array(fc.nat({ max: 5 }), { minLength, maxLength }),
      fc.oneof(
        { weight: 1, arbitrary: fc.constant([...WORKSPACE_ALIASES]) },
        {
          weight: 1,
          arbitrary: fc.constant([...WORKSPACE_ALIASES, OTHER_WORKSPACE]),
        }
      )
    )
    .map(([drawn, spellings]) => buildToggleSequence(drawn, spellings));
}

/**
 * A sequence guaranteed to contain at least one mid-transition interruption —
 * the Requirement 1.8 precondition — so that clause of P28 is not left to
 * chance. Built by filtering rather than by construction so the interruption
 * still sits in a naturally-generated context; the length floor keeps the
 * filter's acceptance rate high enough that it never exhausts its skip budget.
 */
export function arbInterruptedToggleSequence(): fc.Arbitrary<ToggleEvent[]> {
  return arbToggleEventSequenceSized(6, 24).filter((events) =>
    events.some((event) => event.kind === "request" && event.duringTransition)
  );
}

// ===========================================================================
// Section 3 — `arb_mcp_config_doc`
//
// Design: "MCP config documents with foreign entries, legacy Cognis entries
// (name-only), marked entries, and unusual key ordering". Feeds P31 (foreign
// state is preserved).
//
// P31's clause is byte-identity: "removing Cognis entries leaves every unmarked
// entry byte-identical". Four generated shapes make that a real test:
//
//   1. **Marked entries for *this* workspace** — the only entries removal may
//      touch (Requirement 3.10, 4.3).
//   2. **Marked entries for a *different* workspace** — the trap. They carry
//      `COGNIS_MANAGED_BY`, so a marker-only check removes them; Requirement 4.3
//      scopes removal to *this* Canonical_Workspace, so they must survive.
//   3. **Legacy name-only Cognis entries** — written by an older extension with
//      no marker. The design's fallback heuristic (`isCognisMcpServerName`) is
//      what keeps existing installs cleanable, so these must remain removable.
//   4. **Foreign entries**, including **Cognis look-alikes** (`cognisearch`,
//      `my-cognis`, `Cognis`) that neither the marker nor the legacy heuristic
//      matches. A removal that pattern-matched on the substring `cognis`
//      instead of the exact heuristic would delete a stranger's server.
//
// Unusual key ordering is generated at both levels — top-level document keys
// and `mcpServers` insertion order — because JSON key order is preserved by
// `JSON.stringify`, so a rewrite that rebuilds the object in a different order
// breaks byte-identity even though it deletes nothing.
//
// Consumed by `propForeignState.test.ts` (task 16.12 / P31) against production
// `filterOutManagedMcpEntries` + `writeJsonFile`. Marker constants are
// re-exported from `src/mcpManagedEntry.ts` (task 16.4).
// ===========================================================================

/**
 * How a generated entry relates to Cognis ownership. Carried alongside the
 * document so a property can state its expectation per entry instead of
 * re-deriving ownership with the very logic under test.
 */
export type McpEntryKind =
  /** Marker present, workspace matches — removable for this workspace. */
  | "markedThisWorkspace"
  /** Marker present, workspace is a *different* one — must survive. */
  | "markedOtherWorkspace"
  /** No marker, name matches the legacy heuristic — removable via fallback. */
  | "legacyCognisName"
  /** No marker, no Cognis-ish name — must survive. */
  | "foreign"
  /** No marker; name merely *contains* "cognis" — must survive. */
  | "foreignCognisLookalike";

/** Entry kinds a correct removal leaves byte-identical. */
export const PRESERVED_ENTRY_KINDS: readonly McpEntryKind[] = [
  "markedOtherWorkspace",
  "foreign",
  "foreignCognisLookalike",
];

/** Entry kinds a correct removal deletes for this workspace. */
export const REMOVABLE_ENTRY_KINDS: readonly McpEntryKind[] = [
  "markedThisWorkspace",
  "legacyCognisName",
];

/** A loosely-typed on-disk MCP server block — exactly what is read from JSON. */
export interface McpEntryBlock {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  type?: string;
  autoApprove?: string[];
}

/** One generated entry plus the classification a property asserts against. */
export interface McpEntrySpec {
  name: string;
  kind: McpEntryKind;
  block: McpEntryBlock;
}

/** A whole generated `mcp.json`, plus the workspace removal is scoped to. */
export interface McpConfigDoc {
  /** The document exactly as it would be parsed from disk, key order included. */
  doc: Record<string, unknown>;
  /** The entries inside `doc.mcpServers`, in insertion order. */
  entries: McpEntrySpec[];
  /** The Canonical_Workspace key the removal under test is scoped to. */
  workspaceKey: string;
}

/**
 * Names that are *not* Cognis but sit close enough to break a sloppy match.
 * `isCognisMcpServerName` is `name === "cognis" || name.startsWith("cognis-")`,
 * so none of these match it — and none may be removed.
 */
const LOOKALIKE_NAMES = [
  "cognisearch",
  "cognison",
  "my-cognis",
  "Cognis",
  "COGNIS-UPPER",
  "not-cognis-really",
] as const;

/** Ordinary third-party MCP servers. */
const FOREIGN_NAMES = [
  "github",
  "filesystem",
  "postgres",
  "puppeteer",
  "brave-search",
] as const;

/** Names an older Cognis wrote, before the marker existed. */
const LEGACY_COGNIS_NAMES = [
  "cognis",
  "cognis-my-app-3f9a2c",
  "cognis-other-app-9d1e77",
] as const;

/** Names the current Cognis writes (marker-bearing). */
const MANAGED_COGNIS_NAMES = [
  "cognis-my-app-3f9a2c",
  "cognis-work-repo-11ab22",
] as const;

/**
 * Env blocks are generated with **varying key order** on purpose: the marker
 * may be written before or after the pre-existing keys, and a removal that
 * rewrites the env (rather than leaving the entry alone) will reorder it.
 */
function envWithMarker(
  workspaceKey: string,
  markerFirst: boolean
): Record<string, string> {
  const marker: Record<string, string> = {
    [COGNIS_MANAGED_BY]: COGNIS_MANAGED_BY_VALUE,
    [COGNIS_MANAGED_WORKSPACE]: workspaceKey,
  };
  const rest: Record<string, string> = {
    COGNIS_DB_PATH: `${workspaceKey}/.cognis/uckg.db`,
    COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
  };
  return markerFirst ? { ...marker, ...rest } : { ...rest, ...marker };
}

const arbForeignBlock: fc.Arbitrary<McpEntryBlock> = fc.oneof(
  fc.constant<McpEntryBlock>({
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-github"],
    env: { GITHUB_TOKEN: "redacted" },
  }),
  fc.constant<McpEntryBlock>({ type: "http", url: "http://127.0.0.1:9999/mcp" }),
  // No `env` at all — an entry a marker check must handle without throwing.
  fc.constant<McpEntryBlock>({ command: "node", args: ["server.js"] })
);

function entryOfKind(
  kind: McpEntryKind,
  nameOrdinal: number,
  markerFirst: boolean,
  workspaceKey: string,
  otherWorkspaceKey: string
): McpEntrySpec {
  switch (kind) {
    case "markedThisWorkspace":
      return {
        name: MANAGED_COGNIS_NAMES[nameOrdinal % MANAGED_COGNIS_NAMES.length],
        kind,
        block: {
          command: "D:/tools/cognis.exe",
          args: ["mcpd", "--proxy"],
          env: envWithMarker(workspaceKey, markerFirst),
        },
      };
    case "markedOtherWorkspace":
      return {
        name: `cognis-elsewhere-${nameOrdinal % 3}0ab1c`,
        kind,
        block: {
          command: "D:/tools/cognis.exe",
          args: ["mcpd", "--proxy"],
          env: envWithMarker(otherWorkspaceKey, markerFirst),
        },
      };
    case "legacyCognisName":
      return {
        name: LEGACY_COGNIS_NAMES[nameOrdinal % LEGACY_COGNIS_NAMES.length],
        kind,
        block: {
          command: "cognis",
          args: ["mcpd"],
          // Legacy entries carry a DB path but no marker — that absence is the
          // whole point of the fallback heuristic.
          env: { COGNIS_DB_PATH: `${workspaceKey}/.cognis/uckg.db` },
        },
      };
    case "foreignCognisLookalike":
      return {
        name: LOOKALIKE_NAMES[nameOrdinal % LOOKALIKE_NAMES.length],
        kind,
        block: {
          command: "npx",
          args: ["-y", "cognisearch-mcp"],
          // Deliberately carries a COGNIS_-prefixed key that is *not* a marker.
          env: { COGNIS_LIKE_BUT_NOT_A_MARKER: "1" },
        },
      };
    case "foreign":
    default:
      return {
        name: FOREIGN_NAMES[nameOrdinal % FOREIGN_NAMES.length],
        kind: "foreign",
        block: { command: "npx", args: ["-y", "server"] },
      };
  }
}

/** Top-level keys a real `mcp.json` may carry beside `mcpServers`. */
const FOREIGN_TOP_LEVEL: Record<string, unknown> = {
  $schema: "https://example.invalid/mcp.schema.json",
  inputs: [{ id: "token", type: "promptString" }],
};

/**
 * `arb_mcp_config_doc` — MCP config documents with foreign entries, legacy
 * name-only Cognis entries, marked entries, and unusual key ordering.
 *
 * Every document is guaranteed to contain **at least one preserved entry**, so
 * P31's byte-identity clause is never vacuously true. The rest of the entry
 * multiset, their order, the `mcpServers` position among the top-level keys,
 * and the marker's position inside `env` are all generated.
 */
export function arbMcpConfigDoc(): fc.Arbitrary<McpConfigDoc> {
  return fc
    .record({
      kinds: fc.array(
        fc.constantFrom<McpEntryKind>(
          "markedThisWorkspace",
          "markedOtherWorkspace",
          "legacyCognisName",
          "foreign",
          "foreignCognisLookalike"
        ),
        { minLength: 0, maxLength: 6 }
      ),
      // Guarantees the document is never vacuously preserving.
      guaranteedPreserved: fc.constantFrom(...PRESERVED_ENTRY_KINDS),
      ordinals: fc.array(fc.nat({ max: 5 }), { minLength: 7, maxLength: 7 }),
      markerFirst: fc.boolean(),
      mcpServersFirst: fc.boolean(),
      includeForeignTopLevel: fc.boolean(),
      workspaceKey: fc.constantFrom(PRIMARY_WORKSPACE_KEY, OTHER_WORKSPACE_KEY),
    })
    .map(
      ({
        kinds,
        guaranteedPreserved,
        ordinals,
        markerFirst,
        mcpServersFirst,
        includeForeignTopLevel,
        workspaceKey,
      }): McpConfigDoc => {
        const otherWorkspaceKey =
          workspaceKey === PRIMARY_WORKSPACE_KEY
            ? OTHER_WORKSPACE_KEY
            : PRIMARY_WORKSPACE_KEY;

        const requested: McpEntryKind[] = [guaranteedPreserved, ...kinds];
        const entries: McpEntrySpec[] = [];
        const seen = new Set<string>();
        for (const [index, kind] of requested.entries()) {
          const entry = entryOfKind(
            kind,
            ordinals[index % ordinals.length],
            markerFirst,
            workspaceKey,
            otherWorkspaceKey
          );
          // A JSON object cannot hold two identical keys; the first writer of a
          // name wins, exactly as it would on disk.
          if (seen.has(entry.name)) {
            continue;
          }
          seen.add(entry.name);
          entries.push(entry);
        }

        const mcpServers: Record<string, McpEntryBlock> = {};
        for (const entry of entries) {
          mcpServers[entry.name] = entry.block;
        }

        const doc: Record<string, unknown> = {};
        if (mcpServersFirst) {
          doc.mcpServers = mcpServers;
        }
        if (includeForeignTopLevel) {
          Object.assign(doc, FOREIGN_TOP_LEVEL);
        }
        if (!mcpServersFirst) {
          doc.mcpServers = mcpServers;
        }

        return { doc, entries, workspaceKey };
      }
    );
}

/**
 * Serialize a document exactly as `writeJsonFile` in `src/mcpConfig.ts` does
 * (2-space indent, single trailing newline), so P31's "byte-identical" clause
 * is a real byte comparison against the bytes that reach disk rather than a
 * structural `deepEqual` that would tolerate a key reordering.
 */
export function serializeMcpConfigDoc(doc: Record<string, unknown>): string {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

/** The entries a correct removal must leave untouched, in document order. */
export function preservedEntries(document: McpConfigDoc): McpEntrySpec[] {
  return document.entries.filter((entry) =>
    PRESERVED_ENTRY_KINDS.includes(entry.kind)
  );
}

/** The entries a correct removal must delete for `document.workspaceKey`. */
export function removableEntries(document: McpConfigDoc): McpEntrySpec[] {
  return document.entries.filter((entry) =>
    REMOVABLE_ENTRY_KINDS.includes(entry.kind)
  );
}

/**
 * The marker test, stated once here from the design's rule so a property does
 * not restate it: an entry is Cognis-owned *for this workspace* iff its `env`
 * carries {@link COGNIS_MANAGED_BY} with Cognis's value and
 * {@link COGNIS_MANAGED_WORKSPACE} equal to the workspace key.
 *
 * The generator's independent model of the marker test. Compared against
 * `isManagedForWorkspace` in production so a drift between the two is visible.
 */
export function isMarkedForWorkspace(
  block: McpEntryBlock,
  workspaceKey: string
): boolean {
  const env = block.env;
  if (!env) {
    return false;
  }
  return (
    env[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE &&
    env[COGNIS_MANAGED_WORKSPACE] === workspaceKey
  );
}
