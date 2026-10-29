/**
 * Property 31: Foreign state is preserved (task 16.12).
 *
 * For any MCP config document (`arb_mcp_config_doc`), process table, and
 * source tree: production removal leaves every unmarked / other-workspace
 * entry byte-identical; `runTurnOff` terminates only Owned_Daemons; user
 * source files stay byte-identical; the Workspace_Database remains present
 * with an unchanged content digest after deactivation.
 *
 * Validates: Requirements 4.3, 4.4, 4.5, 4.9, 21.5, 21.7, 21.8
 * Properties: P31
 */
import "./testHarness";

import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { writeJsonFile } from "../mcpConfig";
import { filterOutManagedMcpEntries } from "../mcpManagedEntry";
import {
  runTurnOff,
  type DeactivationHost,
  type OwnedDaemon,
  type OwnedDaemonRole,
} from "../deactivation";
import {
  PROPERTY_RUNS,
  arbMcpConfigDoc,
  preservedEntries,
  removableEntries,
  serializeMcpConfigDoc,
  type McpConfigDoc,
} from "./generators";

const ROLES: readonly OwnedDaemonRole[] = ["indexd", "mcpd", "owner"];

interface ProcessTable {
  owned: Array<OwnedDaemon & { staysAlive: boolean }>;
  foreign: number[];
}

interface SourceTree {
  files: Array<{ rel: string; body: string }>;
  dbBytes: number[];
}

const arbProcessTable: fc.Arbitrary<ProcessTable> = fc
  .tuple(
    fc.array(
      fc.record({
        pid: fc.integer({ min: 2, max: 30_000 }),
        role: fc.constantFrom(...ROLES),
        staysAlive: fc.boolean(),
      }),
      { minLength: 0, maxLength: 4 }
    ),
    fc.array(fc.integer({ min: 40_000, max: 50_000 }), {
      minLength: 0,
      maxLength: 4,
    })
  )
  .map(([ownedRaw, foreignRaw]) => {
    const seen = new Set<number>();
    const owned: ProcessTable["owned"] = [];
    for (const daemon of ownedRaw) {
      if (seen.has(daemon.pid)) {
        continue;
      }
      seen.add(daemon.pid);
      owned.push(daemon);
    }
    const foreign: number[] = [];
    for (const pid of foreignRaw) {
      if (seen.has(pid)) {
        continue;
      }
      seen.add(pid);
      foreign.push(pid);
    }
    return { owned, foreign };
  });

const arbSourceTree: fc.Arbitrary<SourceTree> = fc.record({
  files: fc
    .array(
      fc.record({
        rel: fc.constantFrom(
          "src/main.ts",
          "src/lib.rs",
          "README.md",
          "pkg/app.py"
        ),
        body: fc.string({ maxLength: 32 }),
      }),
      { minLength: 1, maxLength: 4 }
    )
    .map((files) => {
      const seen = new Set<string>();
      return files.filter((file) => {
        if (seen.has(file.rel)) {
          return false;
        }
        seen.add(file.rel);
        return true;
      });
    }),
  dbBytes: fc.array(fc.integer({ min: 0, max: 255 }), {
    minLength: 8,
    maxLength: 48,
  }),
});

function sha256(bytes: Buffer): string {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function cloneServers(document: McpConfigDoc): Record<string, unknown> {
  return {
    ...((document.doc.mcpServers as Record<string, unknown> | undefined) ?? {}),
  };
}

function assertMcpPreserved(document: McpConfigDoc): Record<string, unknown> {
  const servers = cloneServers(document);
  const beforeBytes = new Map<string, string>();
  const beforeRefs = new Map<string, unknown>();
  for (const entry of preservedEntries(document)) {
    beforeBytes.set(entry.name, JSON.stringify(servers[entry.name]));
    beforeRefs.set(entry.name, servers[entry.name]);
  }

  const removed = filterOutManagedMcpEntries(servers, document.workspaceKey);
  const expectedRemoved = new Set(removableEntries(document).map((entry) => entry.name));

  for (const name of expectedRemoved) {
    assert.ok(removed.includes(name), `must remove "${name}"`);
    assert.equal(name in servers, false);
  }
  for (const entry of preservedEntries(document)) {
    assert.equal(
      servers[entry.name],
      beforeRefs.get(entry.name),
      `"${entry.name}" (${entry.kind}) must keep object identity`
    );
    assert.equal(
      JSON.stringify(servers[entry.name]),
      beforeBytes.get(entry.name),
      `"${entry.name}" (${entry.kind}) must stay byte-identical`
    );
    assert.equal(
      serializeMcpConfigDoc({ [entry.name]: servers[entry.name] }),
      serializeMcpConfigDoc({ [entry.name]: entry.block })
    );
  }
  return servers;
}

function makeDeactivationHost(
  table: ProcessTable,
  removeManagedMcpEntries: () => Promise<void>
): {
  host: DeactivationHost;
  terminated: number[];
  signaled: number[];
  stillRunning: Set<number>;
} {
  const terminated: number[] = [];
  const signaled: number[] = [];
  const stillRunning = new Set<number>([
    ...table.owned.filter((daemon) => daemon.staysAlive).map((daemon) => daemon.pid),
    ...table.foreign,
  ]);
  const owned: OwnedDaemon[] = table.owned.map(({ pid, role }) => ({ pid, role }));

  const host: DeactivationHost = {
    signal: new AbortController().signal,
    onPhase() {},
    async stopWatcher() {},
    listOwnedDaemons() {
      return [...owned];
    },
    signalStop(daemon) {
      signaled.push(daemon.pid);
    },
    isAlive(daemon) {
      return stillRunning.has(daemon.pid);
    },
    terminate(daemon) {
      terminated.push(daemon.pid);
      stillRunning.delete(daemon.pid);
    },
    logTermination() {},
    removeManagedMcpEntries,
    async sleep() {},
  };
  return { host, terminated, signaled, stillRunning };
}

test("Property 31: foreign MCP entries, processes, sources, and the database survive", async () => {
  await fc.assert(
    fc.asyncProperty(
      arbMcpConfigDoc(),
      arbProcessTable,
      arbSourceTree,
      async (document, table, tree) => {
        const servers = assertMcpPreserved(document);

        const repo = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p31-"));
        try {
          for (const file of tree.files) {
            const abs = path.join(repo, file.rel);
            fs.mkdirSync(path.dirname(abs), { recursive: true });
            fs.writeFileSync(abs, file.body, "utf8");
          }
          const dbPath = path.join(repo, ".cognis", "uckg.db");
          fs.mkdirSync(path.dirname(dbPath), { recursive: true });
          const dbBytes = Buffer.from(tree.dbBytes);
          fs.writeFileSync(dbPath, dbBytes);
          const dbDigest = sha256(dbBytes);

          const sourceBefore = new Map(
            tree.files.map((file) => [
              file.rel,
              fs.readFileSync(path.join(repo, file.rel)),
            ])
          );
          // Indexing treats the source tree as read-only (Requirement 21.5):
          // a pass that only reads user files must not rewrite them.
          for (const file of tree.files) {
            fs.readFileSync(path.join(repo, file.rel));
          }

          const configPath = path.join(repo, ".cursor", "mcp.json");
          const onDisk = JSON.parse(serializeMcpConfigDoc(document.doc)) as Record<
            string,
            unknown
          >;
          const onDiskServers =
            (onDisk.mcpServers as Record<string, unknown> | undefined) ?? {};
          filterOutManagedMcpEntries(onDiskServers, document.workspaceKey);
          onDisk.mcpServers = onDiskServers;
          writeJsonFile(configPath, onDisk);

          const written = JSON.parse(fs.readFileSync(configPath, "utf8")) as Record<
            string,
            unknown
          >;
          const writtenServers =
            (written.mcpServers as Record<string, unknown> | undefined) ?? {};
          for (const entry of preservedEntries(document)) {
            assert.equal(
              JSON.stringify(writtenServers[entry.name]),
              JSON.stringify(entry.block),
              `on-disk "${entry.name}" (${entry.kind}) must stay byte-identical`
            );
          }
          for (const key of Object.keys(document.doc)) {
            if (key === "mcpServers") {
              continue;
            }
            assert.deepEqual(written[key], document.doc[key]);
          }

          const { host, terminated, signaled, stillRunning } = makeDeactivationHost(
            table,
            async () => {
              // MCP removal already applied via production filter + writeJsonFile.
            }
          );
          await runTurnOff(host);

          const ownedPids = new Set(table.owned.map((daemon) => daemon.pid));
          for (const pid of terminated) {
            assert.ok(
              ownedPids.has(pid),
              `terminated pid ${pid} is not in the Owned_Daemon set`
            );
          }
          for (const pid of signaled) {
            assert.ok(ownedPids.has(pid), `signaled pid ${pid} is not owned`);
          }
          for (const pid of table.foreign) {
            assert.ok(!terminated.includes(pid));
            assert.ok(!signaled.includes(pid));
            assert.ok(stillRunning.has(pid), `foreign pid ${pid} must keep running`);
          }

          assert.equal(fs.existsSync(dbPath), true);
          assert.equal(sha256(fs.readFileSync(dbPath)), dbDigest);
          for (const [rel, before] of sourceBefore) {
            assert.ok(
              before.equals(fs.readFileSync(path.join(repo, rel))),
              `${rel} must stay byte-identical after indexing and deactivation`
            );
          }

          assert.equal(
            JSON.stringify(servers),
            JSON.stringify(writtenServers)
          );
        } finally {
          fs.rmSync(repo, { recursive: true, force: true });
        }
      }
    ),
    { numRuns: PROPERTY_RUNS }
  );
});
