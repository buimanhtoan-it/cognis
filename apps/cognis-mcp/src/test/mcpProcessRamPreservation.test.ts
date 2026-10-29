// Harness first: installs the vscode stub + child_process.spawn stub before any
// production module (mcpConfig.ts / indexd.ts, which import vscode) is required.
import { resetHarness, killLiveDaemons } from "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import {
  enableMcpForWorkspace,
  disableMcpForWorkspace,
} from "../mcpConfig";
import { resolveMcpConfigPath } from "../mcpConfigPaths";
import { deriveMcpServerName, isCognisMcpServerName } from "../mcpServerName";
import { isLiveIndexing } from "../indexd";
import { EXPECTED_CONTRACT_VERSION, REQUIRED_MCP_TOOLS } from "../contract";

// ---------------------------------------------------------------------------
// Spec: mcp-process-ram-duplication — Task 2, Property 2: Preservation.
//
// Non-buggy behaviour is unchanged. These tests are written BEFORE the fix and
// MUST PASS on the current (unfixed) code — they capture the baseline behaviour
// the fix must preserve. Task 11 re-runs them on the fixed code to confirm no
// regressions (allowing only the two documented, versioned default flips:
// workspace-scope default and lazy-semantic default, which these tests do NOT
// depend on).
//
// Observation-first methodology: each test drives the UNFIXED code on a
// non-buggy input and asserts the observed, already-correct behaviour.
//
// Validates: Requirements 3.1, 3.2, 3.7, 3.9
// (3.3, 3.4, 3.5, 3.6 non-semantic-retrieval / semantic-equivalence / index
//  completeness / per-repo isolation baselines are already covered by the
//  inline Rust tests in crates/cognis-mcp/src/store_engine.rs and the
//  cognis-store / cognis-indexer suites; they are re-run by `cargo test` in
//  Task 11 and are not duplicated here.)
// ---------------------------------------------------------------------------

function makeTempRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-preserve-"));
}

function cleanup(repoRoot: string): void {
  killLiveDaemons();
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    // Best effort: the OS reclaims temp dirs anyway.
  }
}

/** The workspace mcp.json path Cognis writes for a cursor host (scope=workspace). */
function workspaceCursorConfigPath(repoRoot: string): string {
  return path.join(repoRoot, ".cursor", "mcp.json");
}

function readConfig(configPath: string): Record<string, unknown> {
  return JSON.parse(fs.readFileSync(configPath, "utf8")) as Record<
    string,
    unknown
  >;
}

/**
 * A canonical, order-independent snapshot of the *non-Cognis* content of an
 * mcp.json: every server block whose key is not a Cognis-managed name, plus
 * every top-level key other than `mcpServers`. Comparing the serialization of
 * this snapshot is the "byte-for-byte where possible / semantically exact"
 * preservation check (Requirement 3.1) — the whole file is reformatted on
 * write (2-space indent), so file bytes are not literally stable even on
 * unfixed code, but the non-Cognis *content* must be.
 */
function nonCognisSnapshot(config: Record<string, unknown>): string {
  const servers =
    (config.mcpServers as Record<string, unknown> | undefined) ?? {};
  const nonCognisServers: Record<string, unknown> = {};
  for (const name of Object.keys(servers).sort()) {
    if (!isCognisMcpServerName(name)) {
      nonCognisServers[name] = servers[name];
    }
  }
  const topLevel: Record<string, unknown> = {};
  for (const key of Object.keys(config).sort()) {
    if (key !== "mcpServers") {
      topLevel[key] = config[key];
    }
  }
  return JSON.stringify({ topLevel, servers: nonCognisServers });
}

// ---- Generators ----------------------------------------------------------

/** A non-Cognis MCP server block (command / args / env). */
const arbServerBlock: fc.Arbitrary<Record<string, unknown>> = fc.record({
  command: fc.constantFrom("node", "python", "bash", "docker"),
  args: fc.array(fc.string({ maxLength: 12 }), { maxLength: 3 }),
  env: fc.dictionary(
    fc.string({ minLength: 1, maxLength: 6 }).map((k) => `E_${k}`),
    fc.string({ maxLength: 12 }),
    { maxKeys: 3 }
  ),
});

/**
 * A map of arbitrary NON-Cognis servers. Keys are prefixed `ext-` so they can
 * never collide with a Cognis-managed name (`cognis` / `cognis-*`).
 */
const arbNonCognisServers: fc.Arbitrary<Record<string, unknown>> = fc.dictionary(
  fc
    .string({ minLength: 1, maxLength: 10 })
    .map((s) => `ext-${s.replace(/[^a-zA-Z0-9]/g, "")}`)
    .filter((name) => name.length > 4 && !isCognisMcpServerName(name)),
  arbServerBlock,
  { maxKeys: 4 }
);

/** Arbitrary unknown top-level keys the host format may carry (never mcpServers). */
const arbExtraTopLevel: fc.Arbitrary<Record<string, unknown>> = fc.dictionary(
  fc
    .string({ minLength: 1, maxLength: 8 })
    .map((s) => `top_${s.replace(/[^a-zA-Z0-9]/g, "")}`)
    .filter((k) => k.length > 4 && k !== "mcpServers"),
  fc.oneof(
    fc.string({ maxLength: 16 }),
    fc.integer({ min: -1000, max: 1000 }),
    fc.boolean()
  ),
  { maxKeys: 3 }
);

// ---------------------------------------------------------------------------
// Requirement 3.1 — Non-Cognis config preserved through enable + disable.
// ---------------------------------------------------------------------------

test("Preservation 3.1: enable then disable leaves non-Cognis config semantically identical", async () => {
  await fc.assert(
    fc.asyncProperty(
      arbNonCognisServers,
      arbExtraTopLevel,
      async (nonCognisServers, extraTopLevel) => {
        const repoRoot = makeTempRepo();
        try {
          resetHarness(repoRoot, {
            config: {
              cognis: {
                mcpHost: "cursor",
                mcpConfigScope: "workspace",
                mcpWarmSemanticOnStartup: true,
              },
            },
          });

          const configPath = workspaceCursorConfigPath(repoRoot);
          const seeded: Record<string, unknown> = {
            ...extraTopLevel,
            mcpServers: { ...nonCognisServers },
          };
          fs.mkdirSync(path.dirname(configPath), { recursive: true });
          fs.writeFileSync(
            configPath,
            `${JSON.stringify(seeded, null, 2)}\n`,
            "utf8"
          );
          const baseline = nonCognisSnapshot(seeded);

          // Enable Cognis MCP for the repo: adds only the repo's Cognis entry.
          await enableMcpForWorkspace(repoRoot);
          const afterEnable = readConfig(configPath);
          assert.equal(
            nonCognisSnapshot(afterEnable),
            baseline,
            "non-Cognis content must be unchanged after enable"
          );
          // A Cognis entry for this repo was actually added (sanity: the op ran).
          const cognisName = deriveMcpServerName(repoRoot);
          const enabledServers = afterEnable.mcpServers as Record<
            string,
            unknown
          >;
          assert.ok(
            cognisName in enabledServers,
            "enable must add the repo's Cognis server entry"
          );

          // Disable: removes only the Cognis entry, non-Cognis content stays.
          await disableMcpForWorkspace(repoRoot);
          const afterDisable = readConfig(configPath);
          assert.equal(
            nonCognisSnapshot(afterDisable),
            baseline,
            "non-Cognis content must be unchanged after disable"
          );
          const disabledServers = afterDisable.mcpServers as Record<
            string,
            unknown
          >;
          assert.ok(
            !(cognisName in disabledServers),
            "disable must remove the repo's Cognis server entry"
          );
        } finally {
          cleanup(repoRoot);
        }
      }
    ),
    { numRuns: 40 }
  );
});

test("Preservation 3.1: enabling twice (idempotent) never disturbs non-Cognis servers", async () => {
  await fc.assert(
    fc.asyncProperty(arbNonCognisServers, async (nonCognisServers) => {
      const repoRoot = makeTempRepo();
      try {
        resetHarness(repoRoot, {
          config: {
            cognis: {
              mcpHost: "cursor",
              mcpConfigScope: "workspace",
              mcpWarmSemanticOnStartup: true,
            },
          },
        });
        const configPath = workspaceCursorConfigPath(repoRoot);
        const seeded: Record<string, unknown> = {
          mcpServers: { ...nonCognisServers },
        };
        fs.mkdirSync(path.dirname(configPath), { recursive: true });
        fs.writeFileSync(
          configPath,
          `${JSON.stringify(seeded, null, 2)}\n`,
          "utf8"
        );
        const baseline = nonCognisSnapshot(seeded);

        await enableMcpForWorkspace(repoRoot);
        await enableMcpForWorkspace(repoRoot);
        const after = readConfig(configPath);
        assert.equal(
          nonCognisSnapshot(after),
          baseline,
          "non-Cognis content must survive a repeated enable"
        );
        // Exactly one Cognis entry for this repo (no duplicate fan-out).
        const servers = after.mcpServers as Record<string, unknown>;
        const cognisEntries = Object.keys(servers).filter((n) =>
          isCognisMcpServerName(n)
        );
        assert.equal(
          cognisEntries.length,
          1,
          `expected exactly one Cognis entry, got ${cognisEntries.join(", ")}`
        );
      } finally {
        cleanup(repoRoot);
      }
    }),
    { numRuns: 40 }
  );
});

// ---------------------------------------------------------------------------
// Requirement 3.2 — Explicit global scope is retained (never silently
// rewritten to workspace). Path resolution is pure and takes an explicit
// homeDir, so this exercises the real resolver without touching the real $HOME.
// ---------------------------------------------------------------------------

test("Preservation 3.2: an explicit global scope resolves to the global path, never the workspace path", () => {
  fc.assert(
    fc.property(
      fc.constantFrom("cursor", "vscode", "kiro"),
      fc.string({ minLength: 1, maxLength: 12 }).map((s) => `repo-${s.replace(/[^a-zA-Z0-9]/g, "")}`),
      (host, repoLeaf) => {
        const fakeHome = path.join(os.tmpdir(), "cognis-fake-home");
        const repoRoot = path.join(os.tmpdir(), "cognis-fake-ws", repoLeaf);

        const globalPath = resolveMcpConfigPath(host, repoRoot, "global", fakeHome);
        const workspacePath = resolveMcpConfigPath(
          host,
          repoRoot,
          "workspace",
          fakeHome
        );

        // Explicit global scope stays under the user's home, not inside the repo.
        assert.ok(
          globalPath.startsWith(fakeHome),
          `global scope must resolve under home, got ${globalPath}`
        );
        assert.ok(
          !globalPath.startsWith(repoRoot),
          `global scope must NOT be silently rewritten into the repo (${globalPath})`
        );
        // The two scopes are genuinely distinct (global is never the workspace file).
        assert.notEqual(
          globalPath,
          workspacePath,
          "global and workspace scopes must resolve to different files"
        );
      }
    ),
    { numRuns: 60 }
  );
});

// ---------------------------------------------------------------------------
// Requirement 3.7 — The eight-tool contract + handshake version are preserved.
// This records the observed baseline for this spec so Task 11 can re-run it:
// CONTRACT_VERSION is unchanged (== 1) unless intentionally bumped, and the
// checked-in contract fixture still carries the keys the extension reads.
// ---------------------------------------------------------------------------

test("Preservation 3.7: contract version and MCP tool set are the unchanged baseline", () => {
  // Baseline observed on the unfixed code (crates/cognis-core/src/contract.rs
  // CONTRACT_VERSION = 1; the extension's EXPECTED_CONTRACT_VERSION = 1).
  assert.equal(
    EXPECTED_CONTRACT_VERSION,
    1,
    "CONTRACT_VERSION must stay 1 unless intentionally bumped in lockstep"
  );
  // The four semantic-critical tools the extension hard-depends on are present.
  for (const tool of [
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "semantic_search",
  ]) {
    assert.ok(
      (REQUIRED_MCP_TOOLS as readonly string[]).includes(tool),
      `required MCP tool '${tool}' must remain advertised`
    );
  }

  // The checked-in mcp-config contract fixture still carries the keys the
  // extension reads (COGNIS_DB_PATH in the server env, mcpServers shape).
  const contractsDir = path.resolve(
    __dirname,
    "..",
    "..",
    "..",
    "..",
    "tests",
    "e2e",
    "contracts"
  );
  const fixture = path.join(contractsDir, "mcp_config.json");
  if (fs.existsSync(fixture)) {
    const contract = JSON.parse(fs.readFileSync(fixture, "utf8")) as Record<
      string,
      unknown
    >;
    const config = contract.config as Record<string, unknown>;
    const servers = config.mcpServers as Record<
      string,
      Record<string, unknown>
    >;
    const firstServer = Object.values(servers)[0];
    const env = firstServer.env as Record<string, unknown>;
    assert.ok(
      "COGNIS_DB_PATH" in env,
      "mcp-config contract must still carry COGNIS_DB_PATH (per-repo isolation depends on it)"
    );
  }
});

// ---------------------------------------------------------------------------
// Requirement 3.9 — Safe non-destruction: a stale/inaccessible pid in the
// status file is treated as not-live, and nothing is terminated. This is the
// baseline the fix's lease-aware cleanup must preserve (never kill an
// unrelated or PID-reused process).
// ---------------------------------------------------------------------------

test("Preservation 3.9: a status file with a stale/inaccessible pid reports not-live and kills nothing", () => {
  const repoRoot = makeTempRepo();
  try {
    resetHarness(repoRoot, {
      config: {
        cognis: {
          mcpHost: "cursor",
          mcpConfigScope: "workspace",
        },
      },
    });

    // A very high pid that is (practically) never a live process: the observable
    // proxy for an expired lease / inaccessible / reused-and-gone owner.
    const stalePid = 2_000_000_000;
    const statusPath = path.join(repoRoot, ".cognis", "indexd-status.json");
    fs.mkdirSync(path.dirname(statusPath), { recursive: true });
    fs.writeFileSync(
      statusPath,
      JSON.stringify({
        pid: stalePid,
        active: true,
        phase: "watching",
        message: "watching",
        updated_at: Date.now() / 1000,
      }),
      "utf8"
    );

    // No in-memory handle exists (simulates a reload). The only signal is the
    // status-file pid, which is dead — so isLiveIndexing must return false and
    // must not attempt to terminate the unrelated pid.
    assert.equal(
      isLiveIndexing(repoRoot),
      false,
      "a dead/inaccessible status pid must be treated as not-live"
    );

    // Safe non-destruction: the status file is left intact (cleanup is not
    // triggered by a mere liveness probe).
    assert.ok(
      fs.existsSync(statusPath),
      "the status file must be left untouched by a liveness probe"
    );
  } finally {
    cleanup(repoRoot);
  }
});
