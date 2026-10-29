#!/usr/bin/env node
/**
 * Property 37: Harness isolation and cleanup under failure (task 19.5).
 *
 * Feature: cognis-one-toggle-engine, Property 37: For any E2E_Harness run and
 * any injected failure point, every workspace, database, configuration home,
 * and log the run creates is confined to the Isolation_Root; no real user MCP
 * configuration file and no real user `.cognis` directory is read or written;
 * a failed assertion fails the run and records the failing step and observed
 * values; and the Isolation_Root is removed either way.
 *
 * Validates: Requirements 23.8, 23.9, 23.10, 23.11
 * Properties: P37
 *
 * Usage (from repo root):
 *   node --test tests/e2e/full-flow/prop-isolation.mjs
 */

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
  ScenarioError,
  pathIsUnder,
  realUserSensitivePaths,
  runWithHarness,
  sleep,
  waitExit,
} from "./harness.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "..", "..", "..");
const require = createRequire(import.meta.url);
const fc = require(join(REPO_ROOT, "apps", "cognis-mcp", "node_modules", "fast-check"));

const PROPERTY_RUNS = 100;

const arbFailure = fc.constantFrom("success", "assert", "throw", "timeout");
const arbKind = fc.constantFrom("workspace", "db", "config", "log");
const arbName = fc.stringMatching(/^[a-z]{1,8}$/);
const arbArtifacts = fc.array(
  fc.record({ kind: arbKind, name: arbName }),
  { minLength: 0, maxLength: 4 }
);
const arbStep = fc.stringMatching(/^step-[a-z]{1,8}$/);
const arbObserved = fc.record({
  state: fc.constantFrom("Working", "Needs_Attention", "Ready_Lexical"),
  n: fc.integer({ min: 0, max: 99 }),
});
const arbSpawnChild = fc.boolean();

const arbPlan = fc.record({
  failure: arbFailure,
  artifacts: arbArtifacts,
  step: arbStep,
  observed: arbObserved,
  spawnChild: arbSpawnChild,
});

function snapshotSensitive() {
  const paths = [
    ...realUserSensitivePaths().mcp,
    ...realUserSensitivePaths().cognis,
  ];
  /** @type {Map<string, { exists: boolean, mtimeMs?: number, size?: number }>} */
  const snap = new Map();
  for (const p of paths) {
    try {
      if (!existsSync(p)) {
        snap.set(p, { exists: false });
        continue;
      }
      const st = statSync(p);
      snap.set(p, { exists: true, mtimeMs: st.mtimeMs, size: st.size });
    } catch {
      snap.set(p, { exists: false });
    }
  }
  return snap;
}

function snapshotsEqual(before, after) {
  if (before.size !== after.size) return false;
  for (const [key, a] of before) {
    const b = after.get(key);
    if (!b) return false;
    if (a.exists !== b.exists || a.mtimeMs !== b.mtimeMs || a.size !== b.size) {
      return false;
    }
  }
  return true;
}

function artifactPath(h, artifact) {
  switch (artifact.kind) {
    case "workspace":
      return join(h.workspacePath(artifact.name), "src", `${artifact.name}.txt`);
    case "db":
      return join(h.workspacePath(artifact.name), ".cognis", "uckg.db");
    case "config":
      return join(h.iso.home, ".cursor", `${artifact.name}-mcp.json`);
    case "log":
      return join(h.iso.logs, `${artifact.name}.log`);
    default:
      return join(h.iso.root, artifact.name);
  }
}

function walkFiles(root) {
  const found = [];
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries = [];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const ent of entries) {
      const full = join(dir, ent.name);
      if (ent.isDirectory()) stack.push(full);
      else found.push(full);
    }
  }
  return found;
}

test("Property 37: Harness isolation and cleanup under failure", async () => {
  await fc.assert(
    fc.asyncProperty(arbPlan, async (plan) => {
      const before = snapshotSensitive();
      /** @type {string | undefined} */
      let isoRoot;
      /** @type {unknown} */
      let caught;
      try {
        await runWithHarness(
          async (h) => {
            isoRoot = h.iso.root;
            await h.step(plan.step, async () => {
              for (const artifact of plan.artifacts) {
                const target = artifactPath(h, artifact);
                mkdirSync(dirname(target), { recursive: true });
                writeFileSync(target, `${artifact.kind}:${artifact.name}\n`, "utf8");
                h.assertUnderIsolation(`${artifact.kind} path`, target);
              }
              if (plan.spawnChild) {
                const marker = join(h.iso.logs, "child-ok.txt");
                h.assertUnderIsolation("child log path", marker);
                const child = h.spawn(
                  process.execPath,
                  [
                    "-e",
                    "require('fs').writeFileSync(process.env.COGNIS_AUDIT_LOG, 'ok', 'utf8')",
                  ],
                  { COGNIS_AUDIT_LOG: marker }
                );
                await waitExit(child);
                h.assert("child wrote isolation log", existsSync(marker), { marker });
              }
              for (const file of walkFiles(h.iso.root)) {
                h.assertUnderIsolation("created file", file);
              }
              if (plan.failure === "assert") {
                h.assert(plan.step, false, plan.observed);
              } else if (plan.failure === "throw") {
                throw new Error(`injected:${plan.step}`);
              } else if (plan.failure === "timeout") {
                await sleep(80);
              }
            });
          },
          plan.failure === "timeout" ? { timeoutMs: 15 } : {}
        );
      } catch (error) {
        caught = error;
      }

      assert.ok(isoRoot, "run created an Isolation_Root");
      assert.equal(
        existsSync(isoRoot),
        false,
        `Isolation_Root must be removed after ${plan.failure}`
      );
      assert.ok(
        snapshotsEqual(before, snapshotSensitive()),
        "real user MCP/.cognis must be unchanged"
      );

      if (plan.failure === "success") {
        assert.equal(caught, undefined, "success run must not throw");
        return;
      }

      assert.ok(
        caught instanceof ScenarioError,
        `failure ${plan.failure} must fail the run as ScenarioError`
      );
      if (plan.failure === "timeout") {
        assert.equal(caught.step, "timeout");
      } else {
        assert.equal(caught.step, plan.step);
      }
      if (plan.failure === "assert") {
        assert.deepEqual(caught.observed, plan.observed);
      } else if (plan.failure === "throw") {
        assert.equal(
          /** @type {{ message?: string }} */ (caught.observed).message,
          `injected:${plan.step}`
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
