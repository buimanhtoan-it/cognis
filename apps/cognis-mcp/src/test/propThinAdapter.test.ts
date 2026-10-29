/**
 * Property 22: Thin adapters are free and faithful (task 13.9).
 *
 * The process classifier distinguishes Thin_Adapter processes (thin-proxy
 * marker / `--proxy`) from Workspace_Owner processes (`cognis owner`).
 *
 * Validates: Requirements 17.7
 * Properties: P22
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { isThinProxyProcess, THIN_PROXY_ENV, type McpdProcess } from "../mcpRuntime";
import { PROPERTY_RUNS } from "./generators";

const arbPid = fc.integer({ min: 1, max: 65535 });

const arbThinProcess: fc.Arbitrary<McpdProcess> = fc.oneof(
  arbPid.map((pid) => ({ pid, env: { [THIN_PROXY_ENV]: "1" } })),
  arbPid.map((pid) => ({ pid, commandLine: "cognis mcpd --proxy" })),
  arbPid.map((pid) => ({ pid, commandLine: "cognis mcpd --transport proxy" })),
  arbPid.map((pid) => ({ pid, commandLine: `cognis mcpd ${THIN_PROXY_ENV}=1` })),
  fc.tuple(arbPid, fc.stringMatching(/^[a-z]{0,6}$/)).map(([pid, extra]) => ({
    pid,
    env: { [THIN_PROXY_ENV]: "1" },
    commandLine: extra ? `cognis mcpd --proxy --note-${extra}` : "cognis mcpd --proxy",
  }))
);

const arbOwnerProcess: fc.Arbitrary<McpdProcess> = fc.oneof(
  arbPid.map((pid) => ({ pid, commandLine: "cognis owner" })),
  arbPid.map((pid) => ({
    pid,
    commandLine: "cognis owner --host 127.0.0.1 --port 9",
    env: {},
  })),
  arbPid.map((pid) => ({
    pid,
    commandLine: "cognis-owner --repo-root C:\\ws",
    env: { COGNIS_BECOME_OWNER: "1" },
  }))
);

test("Property 22: process classifier distinguishes thin adapters from owners", () => {
  fc.assert(
    fc.property(arbThinProcess, arbOwnerProcess, (thin, owner) => {
      assert.equal(
        isThinProxyProcess(thin),
        true,
        `thin adapter must be classified thin: ${JSON.stringify(thin)}`
      );
      assert.equal(
        isThinProxyProcess(owner),
        false,
        `workspace owner must not be classified as a thin adapter: ${JSON.stringify(owner)}`
      );
      assert.equal(THIN_PROXY_ENV, "COGNIS_MCP_PROXY");
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
