/**
 * Marker-based Managed_MCP_Entry ownership (task 16.4).
 *
 * Validates: Requirements 3.10, 4.3, 21.7
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  COGNIS_MANAGED_BY,
  COGNIS_MANAGED_BY_VALUE,
  COGNIS_MANAGED_WORKSPACE,
  filterOutManagedMcpEntries,
  isCognisOwnedMarker,
  isManagedForWorkspace,
  isRemovableManagedMcpEntry,
  stampManagedMcpEntry,
  stampManagedMcpEnv,
} from "../mcpManagedEntry";
import {
  PROPERTY_RUNS,
  arbMcpConfigDoc,
  isMarkedForWorkspace,
  preservedEntries,
  removableEntries,
  serializeMcpConfigDoc,
  type McpConfigDoc,
} from "./generators";

test("stampManagedMcpEnv writes both marker keys without dropping existing env", () => {
  const env = stampManagedMcpEnv(
    { COGNIS_DB_PATH: "/repo/.cognis/uckg.db", KEEP: "1" },
    "wire-key-abc"
  );
  assert.equal(env.COGNIS_DB_PATH, "/repo/.cognis/uckg.db");
  assert.equal(env.KEEP, "1");
  assert.equal(env[COGNIS_MANAGED_BY], COGNIS_MANAGED_BY_VALUE);
  assert.equal(env[COGNIS_MANAGED_WORKSPACE], "wire-key-abc");
});

test("stampManagedMcpEntry creates env on a block that had none", () => {
  const stamped = stampManagedMcpEntry(
    { type: "http", url: "http://127.0.0.1:1/mcp" } as {
      type: string;
      url: string;
      env?: Record<string, string>;
    },
    "wk"
  );
  assert.equal(stamped.env[COGNIS_MANAGED_BY], COGNIS_MANAGED_BY_VALUE);
  assert.equal(stamped.env[COGNIS_MANAGED_WORKSPACE], "wk");
  assert.equal(stamped.url, "http://127.0.0.1:1/mcp");
});

test("isManagedForWorkspace matches this workspace and not another", () => {
  const block = stampManagedMcpEntry({ command: "cognis", env: {} }, "this-key");
  assert.equal(isManagedForWorkspace(block, "this-key"), true);
  assert.equal(isManagedForWorkspace(block, "other-key"), false);
  assert.equal(isCognisOwnedMarker(block), true);
  assert.equal(isCognisOwnedMarker({ command: "npx", env: { FOO: "1" } }), false);
});

test("legacy unmarked cognis-* names are removable; marked other-workspace is not", () => {
  const legacy = { command: "cognis", args: ["mcpd"], env: { COGNIS_DB_PATH: "/db" } };
  const other = stampManagedMcpEntry({ command: "cognis", env: {} }, "other");
  const lookalike = { command: "node", args: ["cognisearch"] };
  assert.equal(isRemovableManagedMcpEntry("cognis-app-abc123", legacy, "this"), true);
  assert.equal(isRemovableManagedMcpEntry("cognis-elsewhere-00ab1c", other, "this"), false);
  assert.equal(isRemovableManagedMcpEntry("cognisearch", lookalike, "this"), false);
  assert.equal(isRemovableManagedMcpEntry("brave-search", lookalike, "this"), false);
});

test("filterOutManagedMcpEntries leaves unmarked foreign entries byte-identical", () => {
  const foreign = {
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-github"],
    env: { GITHUB_TOKEN: "redacted", Z_LAST: "1", A_FIRST: "0" },
  };
  const servers: Record<string, unknown> = {
    github: foreign,
    cognis: { command: "cognis", args: ["mcpd"] },
  };
  const before = JSON.stringify(foreign);
  const removed = filterOutManagedMcpEntries(servers, "wk");
  assert.deepEqual(removed, ["cognis"]);
  assert.equal(JSON.stringify(servers.github), before);
  assert.equal(servers.github, foreign);
});

test("P31 subset: per-workspace removal deletes only this workspace's Cognis entries", () => {
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document: McpConfigDoc) => {
      const servers = {
        ...((document.doc.mcpServers as Record<string, unknown> | undefined) ?? {}),
      };
      const removed = filterOutManagedMcpEntries(servers, document.workspaceKey);
      const expectedRemoved = new Set(removableEntries(document).map((e) => e.name));
      const expectedKept = preservedEntries(document);

      for (const name of expectedRemoved) {
        assert.ok(
          removed.includes(name),
          `expected to remove "${name}" for workspace ${document.workspaceKey}`
        );
        assert.equal(name in servers, false);
      }
      for (const entry of expectedKept) {
        assert.ok(
          entry.name in servers,
          `preserved entry "${entry.name}" (${entry.kind}) must survive`
        );
        assert.deepEqual(servers[entry.name], entry.block);
        assert.equal(
          serializeMcpConfigDoc({ [entry.name]: servers[entry.name] }),
          serializeMcpConfigDoc({ [entry.name]: entry.block })
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("production isManagedForWorkspace agrees with the generator model", () => {
  fc.assert(
    fc.property(arbMcpConfigDoc(), (document) => {
      for (const entry of document.entries) {
        assert.equal(
          isManagedForWorkspace(entry.block, document.workspaceKey),
          isMarkedForWorkspace(entry.block, document.workspaceKey)
        );
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
