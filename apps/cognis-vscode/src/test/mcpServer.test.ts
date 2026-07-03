// Harness first: installs the vscode stub before mcpServer.ts is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as net from "node:net";
import * as path from "node:path";
import test from "node:test";

import {
  STOPPED_STATE,
  buildHttpMcpServerBlock,
  buildMcpUrl,
  buildMcpdHttpFlags,
  derivePort,
  getMcpServerState,
  isHttpServerBlock,
  isMcpServerRunning,
  waitForBind,
} from "../mcpServer";

// ---------------------------------------------------------------------------
// derivePort: deterministic, in the dynamic/private band, collision-resistant.
// ---------------------------------------------------------------------------

test("derivePort returns the same port for the same repo path", () => {
  const repo = path.resolve("/tmp/example-repo");
  assert.equal(derivePort(repo), derivePort(repo));
});

test("derivePort lands inside the IANA dynamic/private band [49152, 65535]", () => {
  for (const repo of [
    "/tmp/a",
    "/tmp/another-repo",
    "C:\\Users\\me\\code\\cognis",
    "/very/deep/nested/path/to/a/workspace",
  ]) {
    const port = derivePort(repo);
    assert.ok(port >= 49152 && port <= 65535, `port ${port} out of band for ${repo}`);
  }
});

test("derivePort separates two distinct repos", () => {
  // Two clearly distinct repos should not collide on the same port. Stable
  // and reproducible — no timing, no env.
  const a = derivePort("/tmp/repo-a");
  const b = derivePort("/tmp/repo-b");
  assert.notEqual(a, b);
});

test("derivePort offset shifts the result deterministically", () => {
  const base = derivePort("/tmp/example-repo");
  const next = derivePort("/tmp/example-repo", 1);
  assert.notEqual(base, next);
  // Same offset always lands on the same port.
  assert.equal(next, derivePort("/tmp/example-repo", 1));
});

test("derivePort is path-normalized for `.` segments and case on Windows", () => {
  // Node's path.normalize preserves trailing separators by design; we don't
  // claim to normalize those. What matters is that the canonical workspace
  // path (what VS Code returns from getWorkspaceFolder().uri.fsPath) is stable
  // — ``foo/./bar`` and ``foo/bar`` must hash the same.
  const a = derivePort(path.join("/tmp", "example-repo", "x"));
  const b = derivePort(path.join("/tmp", "example-repo", ".", "x"));
  assert.equal(a, b);
});

// ---------------------------------------------------------------------------
// URL + spawn args: the wire shape for both mcp.json and the spawned process.
// ---------------------------------------------------------------------------

test("buildMcpUrl produces the http://host:port/mcp shape clients expect", () => {
  assert.equal(buildMcpUrl("127.0.0.1", 50001), "http://127.0.0.1:50001/mcp");
});

test("buildMcpdHttpFlags binds host/port for the mcpd http transport", () => {
  assert.deepEqual(
    buildMcpdHttpFlags("127.0.0.1", 50001),
    ["--transport", "http", "--host", "127.0.0.1", "--port", "50001"]
  );
});

// ---------------------------------------------------------------------------
// Default lifecycle state (no spawn happens here).
// ---------------------------------------------------------------------------

test("getMcpServerState returns the stopped state for an unknown workspace", () => {
  const state = getMcpServerState("/tmp/never-started-here");
  assert.equal(state, STOPPED_STATE);
  assert.equal(state.phase, "stopped");
  assert.equal(state.host, "127.0.0.1");
});

test("isMcpServerRunning is false for an unknown workspace", () => {
  assert.equal(isMcpServerRunning("/tmp/never-started-here"), false);
});


test("buildHttpMcpServerBlock writes the type:http url form editors expect", () => {
  assert.deepEqual(buildHttpMcpServerBlock("http://127.0.0.1:50001/mcp"), {
    type: "http",
    url: "http://127.0.0.1:50001/mcp",
  });
});


test("isHttpServerBlock distinguishes url (http) from command (stdio) blocks", () => {
  assert.equal(isHttpServerBlock({ type: "http", url: "http://127.0.0.1:50001/mcp" }), true);
  assert.equal(isHttpServerBlock({ url: "http://127.0.0.1:50001/mcp" }), true);
  assert.equal(isHttpServerBlock({ command: "cognis", args: ["mcpd"] }), false);
  assert.equal(isHttpServerBlock(undefined), false);
  assert.equal(isHttpServerBlock(null), false);
  assert.equal(isHttpServerBlock("nope"), false);
});


// ---------------------------------------------------------------------------
// Readiness probe: the authoritative "is the server bound?" check that
// replaced trusting a log line.
// ---------------------------------------------------------------------------

test("waitForBind detects a listening port and a closed one", async () => {
  const server = net.createServer();
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", () => resolve()));
  const port = (server.address() as net.AddressInfo).port;

  // A real listener → ready quickly.
  assert.equal(await waitForBind("127.0.0.1", port, 5_000), true);

  await new Promise<void>((resolve) => server.close(() => resolve()));

  // Nothing listening → false within the (short) timeout.
  assert.equal(await waitForBind("127.0.0.1", port, 800), false);
});
