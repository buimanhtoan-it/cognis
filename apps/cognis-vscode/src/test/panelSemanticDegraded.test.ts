// Harness first: installs the vscode stub before panel.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  deriveStatusLine,
  deriveStatusHint,
  isSemanticOnlyDegraded,
  type PanelContext,
} from "../panel";
import type { HealthReport } from "../types";

// ---------------------------------------------------------------------------
// Feature: safe index self-recovery — the minimal panel must NOT dead-end on a
// rebuilding/degraded semantic (vector) layer. When the core index (config, db,
// index checks) is healthy and only the vector layer is `warn` (still
// building), the status line reads "Ready" with a tailored hint, so a normal
// user is never sent to the alarming "Needs attention" for a background rebuild.
//
// Genuine config/db/index problems still yield "Needs attention".
// ---------------------------------------------------------------------------

const SEMANTIC_HINT =
  "Ready — semantic search is still building in the background; lexical and structural search work now.";

const ok = { status: "ok", message: "ok" };

/** A provisioned, idle, running workspace whose health is `health`. */
function runningCtx(health: HealthReport): PanelContext {
  return {
    status: "ready",
    advancedMode: false,
    liveIndexing: true,
    mcpEnabled: true,
    syncPaused: false,
    configured: true,
    backendAvailable: true,
    health,
  } as PanelContext;
}

function health(
  overall: "ok" | "warn" | "fail",
  checks: HealthReport["checks"]
): HealthReport {
  return { runtime_version: "0.8.6", overall, checks };
}

// ---------------------------------------------------------------------------
// isSemanticOnlyDegraded — the pure helper.
// ---------------------------------------------------------------------------

test("isSemanticOnlyDegraded: index ok + vector warn is semantic-only", () => {
  const ctx = runningCtx(
    health("warn", { config: ok, db: ok, index: ok, vector: { status: "warn", message: "rebuilding" } })
  );
  assert.equal(isSemanticOnlyDegraded(ctx), true);
});

test("isSemanticOnlyDegraded: false when health absent or overall ok", () => {
  assert.equal(isSemanticOnlyDegraded({ status: "ready" } as PanelContext), false);
  assert.equal(
    isSemanticOnlyDegraded(
      runningCtx(health("ok", { config: ok, db: ok, index: ok, vector: ok }))
    ),
    false
  );
});

test("isSemanticOnlyDegraded: false when db warn/fail even if vector warn", () => {
  for (const dbStatus of ["warn", "fail"] as const) {
    const ctx = runningCtx(
      health("warn", {
        config: ok,
        db: { status: dbStatus, message: "db problem" },
        index: ok,
        vector: { status: "warn", message: "rebuilding" },
      })
    );
    assert.equal(isSemanticOnlyDegraded(ctx), false, `db=${dbStatus}`);
  }
});

test("isSemanticOnlyDegraded: false when index warn/fail/absent (index is the core)", () => {
  for (const indexStatus of ["warn", "fail"] as const) {
    const ctx = runningCtx(
      health("warn", {
        config: ok,
        db: ok,
        index: { status: indexStatus, message: "index problem" },
        vector: { status: "warn", message: "rebuilding" },
      })
    );
    assert.equal(isSemanticOnlyDegraded(ctx), false, `index=${indexStatus}`);
  }
  // Absent index must NOT be tolerated as ok.
  const absentIndex = runningCtx(
    health("warn", { config: ok, db: ok, vector: { status: "warn", message: "rebuilding" } })
  );
  assert.equal(isSemanticOnlyDegraded(absentIndex), false, "absent index");
});

test("isSemanticOnlyDegraded: false when vector is fail (hard failure, not a rebuild)", () => {
  const ctx = runningCtx(
    health("fail", {
      config: ok,
      db: ok,
      index: ok,
      vector: { status: "fail", message: "vector broken" },
    })
  );
  assert.equal(isSemanticOnlyDegraded(ctx), false);
});

test("isSemanticOnlyDegraded: false when a non-vector check (version) is also non-ok", () => {
  const ctx = runningCtx(
    health("warn", {
      config: ok,
      db: ok,
      index: ok,
      vector: { status: "warn", message: "rebuilding" },
      version: { status: "warn", message: "stale" },
    })
  );
  assert.equal(isSemanticOnlyDegraded(ctx), false);
});

test("isSemanticOnlyDegraded: tolerant of absent config/db when index ok + vector warn", () => {
  const ctx = runningCtx(
    health("warn", { index: ok, vector: { status: "warn", message: "rebuilding" } })
  );
  assert.equal(isSemanticOnlyDegraded(ctx), true);
});

// ---------------------------------------------------------------------------
// deriveStatusLine / deriveStatusHint — semantic-only degraded reads as Ready.
// ---------------------------------------------------------------------------

test("deriveStatusLine: index ok + vector warn (running, idle) reads Ready", () => {
  const ctx = runningCtx(
    health("warn", { config: ok, db: ok, index: ok, vector: { status: "warn", message: "rebuilding" } })
  );
  assert.equal(deriveStatusLine(ctx), "Ready");
  assert.equal(deriveStatusHint(ctx), SEMANTIC_HINT);
});

test("deriveStatusLine: db warn still reads Needs attention", () => {
  const ctx = runningCtx(
    health("warn", {
      config: ok,
      db: { status: "warn", message: "db problem" },
      index: ok,
      vector: { status: "warn", message: "rebuilding" },
    })
  );
  assert.equal(deriveStatusLine(ctx), "Needs attention");
  assert.notEqual(deriveStatusHint(ctx), SEMANTIC_HINT);
});

test("deriveStatusLine: index fail still reads Needs attention", () => {
  const ctx = runningCtx(
    health("fail", {
      config: ok,
      db: ok,
      index: { status: "fail", message: "index broken" },
      vector: { status: "warn", message: "rebuilding" },
    })
  );
  assert.equal(deriveStatusLine(ctx), "Needs attention");
});

test("deriveStatusHint (semantic-only Ready) does not leak a raw technical value", () => {
  const ctx = runningCtx(
    health("warn", { config: ok, db: ok, index: ok, vector: { status: "warn", message: "rebuilding" } })
  );
  const hint = deriveStatusHint(ctx);
  assert.doesNotMatch(hint, /https?:\/\//);
  assert.doesNotMatch(hint, /\d{2,}/);
});

// ---------------------------------------------------------------------------
// Property: whenever isSemanticOnlyDegraded holds for a running/idle context,
// the status line is "Ready" (never "Needs attention") and carries the tailored
// hint — i.e. a background semantic rebuild is never a dead-end.
// ---------------------------------------------------------------------------

const arbNonCoreCheck = fc.record({
  status: fc.constantFrom("ok"),
  message: fc.constant("ok"),
});

test("Property: a semantic-only-degraded running context always reads Ready with the semantic hint", () => {
  fc.assert(
    fc.property(
      // vary presence of optional checks; index always ok, vector always warn.
      fc.option(arbNonCoreCheck, { nil: undefined }),
      fc.option(arbNonCoreCheck, { nil: undefined }),
      fc.option(arbNonCoreCheck, { nil: undefined }),
      (config, db, embedder) => {
        const checks: HealthReport["checks"] = {
          index: ok,
          vector: { status: "warn", message: "rebuilding" },
        };
        if (config) checks.config = config;
        if (db) checks.db = db;
        if (embedder) checks.embedder = embedder;
        const ctx = runningCtx(health("warn", checks));
        // Precondition holds by construction.
        assert.equal(isSemanticOnlyDegraded(ctx), true);
        assert.equal(deriveStatusLine(ctx), "Ready");
        assert.equal(deriveStatusHint(ctx), SEMANTIC_HINT);
      }
    ),
    { numRuns: 100 }
  );
});
