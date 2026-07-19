import assert from "node:assert/strict";
import test from "node:test";

import { isThinProxyProcess, parseEnviron, scopeRuntime, THIN_PROXY_ENV } from "../mcpRuntime";

test("parseEnviron parses a NUL-separated environ blob", () => {
  const env = parseEnviron("COGNIS_DB_PATH=/repo/.cognis/uckg.db\0FOO=bar\0");
  assert.equal(env.COGNIS_DB_PATH, "/repo/.cognis/uckg.db");
  assert.equal(env.FOO, "bar");
});

test("parseEnviron ignores blanks and malformed entries", () => {
  const env = parseEnviron("\0=novalue\0KEY=val\0");
  assert.deepEqual(env, { KEY: "val" });
});

test("scopeRuntime returns machine-wide count when no repo requested", () => {
  const runtime = scopeRuntime([{ pid: 1 }, { pid: 2 }]);
  assert.equal(runtime.count, 2);
  assert.equal(runtime.repoScoped, false);
});

test("scopeRuntime filters to the requested repo when every process exposes env", () => {
  const runtime = scopeRuntime(
    [
      { pid: 10, env: { COGNIS_DB_PATH: "/repo-a/.cognis/uckg.db" } },
      { pid: 11, env: { COGNIS_DB_PATH: "/repo-b/.cognis/uckg.db" } },
    ],
    "/repo-a"
  );
  assert.equal(runtime.repoScoped, true);
  assert.deepEqual(runtime.pids, [10]);
  assert.equal(runtime.count, 1);
});

test("scopeRuntime falls back to machine-wide when any process hides its env", () => {
  // A single unreadable process (e.g. Windows) means we can't prove repo binding.
  const runtime = scopeRuntime(
    [
      { pid: 10, env: { COGNIS_DB_PATH: "/repo-a/.cognis/uckg.db" } },
      { pid: 11 },
    ],
    "/repo-a"
  );
  assert.equal(runtime.repoScoped, false);
  assert.equal(runtime.count, 2);
});

test("scopeRuntime reports zero scoped matches honestly (still scoped)", () => {
  const runtime = scopeRuntime(
    [{ pid: 10, env: { COGNIS_DB_PATH: "/repo-b/.cognis/uckg.db" } }],
    "/repo-a"
  );
  assert.equal(runtime.repoScoped, true);
  assert.equal(runtime.count, 0);
});

// ---------------------------------------------------------------------------
// Thin-proxy vs heavy classification (Task 7.1 / Requirement 2.11).
// ---------------------------------------------------------------------------

test("isThinProxyProcess detects env and command-line markers", () => {
  assert.equal(
    isThinProxyProcess({ pid: 1, env: { [THIN_PROXY_ENV]: "1" } }),
    true
  );
  assert.equal(
    isThinProxyProcess({
      pid: 2,
      commandLine: "cognis mcpd --proxy",
    }),
    true
  );
  assert.equal(
    isThinProxyProcess({
      pid: 3,
      commandLine: "cognis mcpd --transport proxy",
    }),
    true
  );
  assert.equal(
    isThinProxyProcess({ pid: 4, commandLine: "cognis mcpd" }),
    false
  );
});

test("scopeRuntime splits thin proxies from heavy daemons", () => {
  const runtime = scopeRuntime(
    [
      {
        pid: 10,
        env: { COGNIS_DB_PATH: "/repo-a/.cognis/uckg.db", [THIN_PROXY_ENV]: "1" },
      },
      {
        pid: 11,
        env: { COGNIS_DB_PATH: "/repo-a/.cognis/uckg.db" },
      },
      {
        pid: 12,
        env: { COGNIS_DB_PATH: "/repo-b/.cognis/uckg.db" },
        commandLine: "cognis mcpd --proxy",
      },
    ],
    "/repo-a"
  );
  assert.equal(runtime.repoScoped, true);
  assert.equal(runtime.count, 2);
  assert.deepEqual(runtime.thinProxyPids, [10]);
  assert.deepEqual(runtime.heavyPids, [11]);
  assert.equal(runtime.thinProxyCount, 1);
  assert.equal(runtime.heavyCount, 1);
});
