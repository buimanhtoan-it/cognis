/**
 * Property 27: A failing reporting path reports itself (task 16.8).
 *
 * For any unreadable subset of the required status sources, once the 5-second
 * read budget has elapsed the Status_Surface reports `Needs_Attention` and
 * names every unreadable source rather than a stale Ready state. For any
 * sequence of reported states, the diagnostics log contains one timestamped
 * entry per transition carrying the values the state was derived from.
 *
 * Uses `arb_status_input` / `arbUnreadableStatusInput`.
 *
 * Validates: Requirements 2.9, 2.10
 * Properties: P27
 */
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { trace } from "../diagnostics";
import {
  deriveStatusSnapshot,
  deriveStatusState,
  statusReadBudgetMs,
  type StatusInput,
} from "../status";
import type { HealthReport, StatusReport } from "../types";
import {
  PROPERTY_RUNS,
  arbUnreadableStatusInput,
  arbUnreadableSources,
} from "./generators";

const SNAPSHOT_SOURCES = ["health", "owner-status.json", "owner.lease"] as const;

function makeTempRepo(): string {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p27-"));
  fs.mkdirSync(path.join(repo, ".cognis"), { recursive: true });
  return repo;
}

function cleanup(repoRoot: string): void {
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

function initTrace(): { dir: string; file: string; read: () => Array<Record<string, unknown>> } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p27-diag-"));
  trace.init({ globalStorageUri: { fsPath: dir } } as never, "0.8.13");
  trace.setMinLevel("debug");
  const file = path.join(dir, "diagnostics.jsonl");
  return {
    dir,
    file,
    read: () =>
      fs.existsSync(file)
        ? fs
            .readFileSync(file, "utf8")
            .trim()
            .split("\n")
            .filter(Boolean)
            .map((line) => JSON.parse(line) as Record<string, unknown>)
        : [],
  };
}

function okHealth(overrides: Partial<HealthReport> = {}): HealthReport {
  return {
    runtime_version: "0.9.0",
    overall: "ok",
    checks: {
      config: { status: "ok", message: "ok" },
      db: { status: "ok", message: "ok" },
      index: { status: "ok", message: "ok" },
    },
    capabilities: {
      semantic_backend: true,
      accelerated_vector_search: true,
    },
    source_generation: 4,
    semantic_generation: 4,
    pending_vector_count: 0,
    semantic_available: true,
    fingerprint_match: true,
    ...overrides,
  };
}

function namesSource(report: StatusReport, source: string): boolean {
  return (
    report.detail.includes(source) || (report.blockingCondition ?? "").includes(source)
  );
}

function reportKey(report: StatusReport): string {
  return JSON.stringify({
    state: report.state,
    detail: report.detail,
    phase: report.phase,
    watcherQueueDepth: report.watcherQueueDepth,
    embeddingQueueDepth: report.embeddingQueueDepth,
    blockingCondition: report.blockingCondition,
    nextAction: report.nextAction,
  });
}

function corruptSource(repoRoot: string, source: string): void {
  const dir = path.join(repoRoot, ".cognis");
  if (source === "owner-status.json") {
    fs.writeFileSync(path.join(dir, "owner-status.json"), "not-json", "utf8");
  }
  if (source === "owner.lease") {
    fs.writeFileSync(path.join(dir, "owner.lease"), "{bad", "utf8");
  }
}

test("P27: an unreadable StatusInput never reports a stale Ready state", () => {
  fc.assert(
    fc.property(arbUnreadableStatusInput(), (input: StatusInput) => {
      const report = deriveStatusState(input);
      assert.equal(report.state, "Needs_Attention");
      assert.notEqual(report.state, "Ready_Full");
      assert.notEqual(report.state, "Ready_Lexical");
      const sources = [...new Set(input.readErrors.filter((value) => value.trim()))];
      assert.ok(sources.length > 0);
      for (const source of sources) {
        assert.ok(namesSource(report, source), `must name unreadable source "${source}"`);
      }
      assert.deepEqual(deriveStatusState(input), report);
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("P27: after the 5s budget every unreadable snapshot source is named", () => {
  fc.assert(
    fc.property(arbUnreadableSources, (sources) => {
      const snapshotSources = sources.filter((source): source is (typeof SNAPSHOT_SOURCES)[number] =>
        (SNAPSHOT_SOURCES as readonly string[]).includes(source)
      );
      if (snapshotSources.length === 0) {
        // `workspace-database` is named by deriveStatusState (covered above).
        return;
      }
      const repo = makeTempRepo();
      try {
        for (const source of snapshotSources) {
          corruptSource(repo, source);
        }
        const t0 = 20_000;
        deriveStatusSnapshot({
          repoRoot: repo,
          health: okHealth(),
          healthReadFailed: snapshotSources.includes("health"),
          mcpEnabled: true,
          configured: true,
          nowMs: t0,
        });
        const late = deriveStatusSnapshot({
          repoRoot: repo,
          health: okHealth(),
          healthReadFailed: snapshotSources.includes("health"),
          mcpEnabled: true,
          configured: true,
          nowMs: t0 + statusReadBudgetMs,
        });
        assert.equal(late.report.state, "Needs_Attention");
        assert.notEqual(late.report.state, "Ready_Full");
        assert.notEqual(late.report.state, "Ready_Lexical");
        for (const source of snapshotSources) {
          assert.ok(
            late.input.readErrors.includes(source),
            `readErrors must include ${source} after the 5s budget`
          );
          assert.ok(namesSource(late.report, source), `report must name ${source}`);
        }
      } finally {
        cleanup(repo);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("P27: diagnostics log holds one timestamped derived-from entry per transition", () => {
  fc.assert(
    fc.property(fc.array(fc.nat({ max: 8 }), { minLength: 2, maxLength: 6 }), (pendings) => {
      const repo = makeTempRepo();
      const { dir, read } = initTrace();
      try {
        const reports: StatusReport[] = [];
        for (const pending of pendings) {
          reports.push(
            deriveStatusSnapshot({
              repoRoot: repo,
              health: okHealth({ pending_vector_count: pending }),
              mcpEnabled: true,
              configured: true,
            }).report
          );
        }

        let expected = 1;
        for (let i = 1; i < reports.length; i += 1) {
          if (reportKey(reports[i]!) !== reportKey(reports[i - 1]!)) {
            expected += 1;
          }
        }

        const transitions = read().filter((entry) => {
          const data = entry.data as { repoRoot?: string } | undefined;
          return (
            entry.scope === "status" &&
            entry.message === "Status surface transition" &&
            data?.repoRoot === repo
          );
        });
        assert.equal(transitions.length, expected);
        for (const entry of transitions) {
          assert.equal(typeof entry.ts, "string");
          assert.ok((entry.ts as string).length > 0);
          const data = entry.data as Record<string, unknown>;
          assert.ok(data.derivedFrom && typeof data.derivedFrom === "object");
          assert.ok(typeof data.state === "string");
        }
      } finally {
        cleanup(repo);
        cleanup(dir);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
