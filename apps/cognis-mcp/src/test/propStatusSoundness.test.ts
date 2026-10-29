/**
 * Property 26: Status soundness and totality (task 16.7).
 *
 * For any `StatusInput` drawn from `arb_status_input`: the derivation returns
 * exactly one of the four `Status_State` values and never throws; `Ready_Full`
 * implies the Requirement 2.2 conjunction (as modeled by
 * {@link satisfiesReadyFullConjunction}); `Ready_Lexical` is reported when
 * lexical retrieval serves and at least one degradation holds, with the detail
 * line naming it — including "vector search running in fallback mode" when
 * accelerated vector search is absent; `Working` carries a phase name and both
 * queue depths; `Needs_Attention` names the blocking condition and offers
 * exactly one next action.
 *
 * Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 3.7, 3.8, 6.4, 6.5,
 * 14.3, 20.6
 * Properties: P26
 */
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { deriveStatusState, type StatusInput } from "../status";
import type { StatusReport, StatusState } from "../types";
import {
  PROPERTY_RUNS,
  STATUS_STATES,
  arbStatusInput,
  satisfiesReadyFullConjunction,
} from "./generators";

const STATUS_SET = new Set<StatusState>(STATUS_STATES);

const WORKING_OWNER_PHASES = new Set([
  "starting",
  "provisioning",
  "indexing",
  "embedding",
  "incremental",
  "cold_index",
  "rebuild",
  "sweep",
  "branch_change",
]);

const DEGRADATION_DETAIL = [
  /semantic backend is unavailable/i,
  /embeddings are still pending/i,
  /semantic generation is still catching up/i,
  /fingerprint does not match/i,
  /vector search running in fallback mode/i,
  /semantic search is temporarily/i,
];

function lexicalServes(input: StatusInput): boolean {
  const checks = input.health?.checks;
  return (
    checks?.config?.status === "ok" &&
    checks.db?.status === "ok" &&
    checks.index?.status === "ok"
  );
}

function isUnreadable(input: StatusInput): boolean {
  return input.readErrors.some((source) => source.trim().length > 0);
}

function isWorkingInput(input: StatusInput): boolean {
  if (input.transition !== undefined) {
    return true;
  }
  const phase = input.owner?.phase;
  return typeof phase === "string" && WORKING_OWNER_PHASES.has(phase);
}

function isBlockingInput(input: StatusInput): boolean {
  if ((input.health?.missing_indexes?.length ?? 0) > 0) {
    return true;
  }
  const messages: Array<string | undefined> = [
    input.owner?.message,
    input.owner?.lastError,
    ...Object.values(input.health?.checks ?? {}).map((check) => check.message),
  ];
  if (messages.some((message) => typeof message === "string" && /checksum/i.test(message))) {
    return true;
  }
  const db = input.health?.checks?.db;
  return (
    db?.status === "fail" &&
    typeof db.message === "string" &&
    /(not writable|read-only|readonly)/i.test(db.message)
  );
}

function namesADegradation(detail: string): boolean {
  return DEGRADATION_DETAIL.some((pattern) => pattern.test(detail));
}

function assertExactlyOneNextAction(report: StatusReport): void {
  assert.ok(report.nextAction, `${report.state} must offer a next action`);
  assert.ok(report.nextAction.title.length > 0, "next action must have a title");
  assert.ok(
    report.nextAction.command.startsWith("cognis."),
    `next action command must be a Cognis command, got ${report.nextAction.command}`
  );
  assert.equal(Object.keys(report.nextAction).length, 2, "exactly one next action (title + command)");
}

test("P26: status derivation is total, sound, and names every class of state", () => {
  fc.assert(
    fc.property(arbStatusInput(), (input: StatusInput) => {
      let report: StatusReport;
      try {
        report = deriveStatusState(input);
      } catch (error) {
        assert.fail(`deriveStatusState threw: ${String(error)}`);
      }

      assert.ok(STATUS_SET.has(report.state), `unknown state ${report.state}`);
      assert.equal(
        STATUS_STATES.filter((state) => state === report.state).length,
        1
      );
      assert.equal(typeof report.detail, "string");
      assert.ok(report.detail.length > 0);
      assert.equal(typeof report.observedAt, "number");

      if (report.state === "Ready_Full") {
        assert.ok(
          satisfiesReadyFullConjunction(input),
          "Ready_Full must imply the Requirement 2.2 conjunction"
        );
        assert.equal(input.health?.semantic_available, true);
        assert.equal(input.health?.pending_vector_count, 0);
        assert.equal(input.health?.semantic_generation, input.health?.source_generation);
        assert.equal(input.health?.fingerprint_match, true);
        assert.equal(input.health?.capabilities?.semantic_backend, true);
        assert.ok(lexicalServes(input), "Ready_Full requires lexical retrieval to serve");
      }

      if (report.state === "Ready_Lexical") {
        assert.ok(lexicalServes(input), "Ready_Lexical requires lexical retrieval to serve");
        assert.ok(
          namesADegradation(report.detail),
          `Ready_Lexical must name a degradation, got: ${report.detail}`
        );
        const caps = input.health?.capabilities ?? input.owner?.capabilities;
        const pending =
          input.health?.pending_vector_count ?? input.owner?.pendingVectorCount ?? 0;
        const source =
          input.health?.source_generation ?? input.owner?.sourceGeneration;
        const semantic =
          input.health?.semantic_generation ?? input.owner?.semanticGeneration;
        const fingerprint =
          input.health?.fingerprint_match ?? input.owner?.fingerprintMatch;
        const fallbackOnly =
          caps?.semantic_backend === true &&
          caps.accelerated_vector_search === false &&
          input.health?.semantic_available === true &&
          pending === 0 &&
          fingerprint === true &&
          source !== undefined &&
          semantic !== undefined &&
          semantic === source;
        if (fallbackOnly) {
          assert.match(report.detail, /vector search running in fallback mode/i);
        }
      }

      if (report.state === "Working") {
        assert.ok(report.phase && report.phase.length > 0, "Working must carry a phase name");
        assert.equal(typeof report.watcherQueueDepth, "number");
        assert.equal(typeof report.embeddingQueueDepth, "number");
        assert.ok((report.watcherQueueDepth ?? -1) >= 0);
        assert.ok((report.embeddingQueueDepth ?? -1) >= 0);
        assert.match(report.detail, /watcher queue/i);
        assert.match(report.detail, /embedding queue/i);
      }

      if (report.state === "Needs_Attention") {
        assert.ok(
          report.blockingCondition && report.blockingCondition.length > 0,
          "Needs_Attention must name the blocking condition"
        );
        assertExactlyOneNextAction(report);
      }

      // Completeness of the decision order (Req 2.1–2.6): each input class
      // maps to exactly the state the acceptance criteria name.
      if (isUnreadable(input)) {
        assert.equal(report.state, "Needs_Attention");
        for (const source of new Set(input.readErrors.filter((value) => value.trim()))) {
          assert.ok(
            report.detail.includes(source) ||
              (report.blockingCondition ?? "").includes(source),
            `unreadable source "${source}" must be named`
          );
        }
        return;
      }
      if (isBlockingInput(input)) {
        assert.equal(report.state, "Needs_Attention");
        return;
      }
      if (isWorkingInput(input)) {
        assert.equal(report.state, "Working");
        return;
      }
      if (lexicalServes(input)) {
        const full =
          satisfiesReadyFullConjunction(input) &&
          input.health?.capabilities?.accelerated_vector_search === true;
        if (full) {
          assert.equal(report.state, "Ready_Full");
        } else {
          assert.equal(report.state, "Ready_Lexical");
        }
        return;
      }
      assert.equal(report.state, "Needs_Attention");
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
