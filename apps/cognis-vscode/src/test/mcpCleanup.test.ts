// Harness first: installs the vscode stub before mcpConfig.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import { filterOutCognisServers } from "../mcpConfig";

test("filterOutCognisServers removes legacy and named cognis entries", () => {
  const servers: Record<string, unknown> = {
    cognis: { command: "python", env: {} },
    "cognis-myrepo": { command: "python", env: {} },
    "cognis-other-repo": { command: "python", env: {} },
    "brave-search": { command: "node", env: {} },
  };
  const removed = filterOutCognisServers(servers);
  assert.deepEqual(removed.sort(), ["cognis", "cognis-myrepo", "cognis-other-repo"]);
  // Non-cognis servers are preserved.
  assert.deepEqual(Object.keys(servers), ["brave-search"]);
});

test("filterOutCognisServers is a no-op when there are no cognis entries", () => {
  const servers: Record<string, unknown> = {
    "brave-search": { command: "node", env: {} },
  };
  const removed = filterOutCognisServers(servers);
  assert.deepEqual(removed, []);
  assert.deepEqual(Object.keys(servers), ["brave-search"]);
});

test("filterOutCognisServers handles an empty map", () => {
  const servers: Record<string, unknown> = {};
  assert.deepEqual(filterOutCognisServers(servers), []);
});
