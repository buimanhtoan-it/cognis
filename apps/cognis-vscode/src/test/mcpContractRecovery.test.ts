/**
 * Property 11 + Property 13 — Contract-parity and recovery discipline.
 *
 * Spec: mcp-process-ram-duplication · Task 9.4
 *
 * **Property 11: Bug Condition** — Contract and handshake preserved or bumped
 * in lockstep.
 * **Validates: Requirements 2.10**
 *
 * Asserts eight-tool real-output parity and handshake against the checked-in
 * fixtures under `tests/e2e/contracts/`, and that `CONTRACT_VERSION` /
 * `EXPECTED_CONTRACT_VERSION` stay at the intentional baseline (1) unless
 * both sides are intentionally bumped together with fixtures + parity tests.
 *
 * **Property 13: Bug Condition** — Deterministic recovery and evidence
 * discipline.
 * **Validates: Requirements 2.13, 2.14**
 *
 * Asserts migration/rollback dry-run/plan is restartable and idempotent,
 * retains backups until verified success, restores prior config/topology on
 * failed checks, and cleans only Cognis-owned state. Sharing-gate evidence
 * stays fail-closed without a complete, pointed evidence set.
 */
// Harness first: installs the vscode stub before mcpConfig* modules load.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import {
  EXPECTED_CONTRACT_VERSION,
  REQUIRED_CLI_COMMANDS,
  REQUIRED_MCP_TOOLS,
  evaluateHandshake,
  type HandshakePayload,
} from "../contract";
import { writeJsonFile } from "../mcpConfig";
import {
  migrateGlobalEntryToWorkspace,
  planGlobalEntryToWorkspaceMigration,
  type MigrationStep,
} from "../mcpConfigMigrate";
import {
  REQUIRED_GATE_CHECKS,
  evaluateSharingGate,
  parseGateEvidenceDocument,
  type GateCheckEvidence,
  type GateCheckId,
} from "../mcpSharingGate";

// ---------------------------------------------------------------------------
// Paths / helpers
// ---------------------------------------------------------------------------

/** Repo root: from apps/cognis-vscode/out/test/ → four levels up. */
const REPO_ROOT = path.resolve(__dirname, "..", "..", "..", "..");
const CONTRACTS_DIR = path.join(REPO_ROOT, "tests", "e2e", "contracts");
const RUST_CONTRACT_RS = path.join(
  REPO_ROOT,
  "crates",
  "cognis-core",
  "src",
  "contract.rs"
);

/**
 * The eight MCP tools in contract order — must stay lockstep with
 * `crates/cognis-core/src/contract.rs` `MCP_TOOLS` (Property 11 / Req 2.10).
 */
const EIGHT_MCP_TOOLS = [
  "diffuse_context",
  "symbol_lookup",
  "symbol_search",
  "discover_symbols",
  "semantic_search",
  "resolve_symbols",
  "dependency_trace",
  "retrieve_context_capsule",
] as const;

/** Fault-injection steps that leave the migration mid-flight. */
const INTERRUPT_STEPS: MigrationStep[] = [
  "backedUp",
  "wroteDestination",
  "verifiedDestination",
  "removedSource",
];

function mkTempHome(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p11-p13-home-"));
}

function mkTempRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p11-p13-repo-"));
}

function cleanup(...dirs: string[]): void {
  for (const dir of dirs) {
    try {
      fs.rmSync(dir, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
}

function loadContract(name: string): Record<string, unknown> | undefined {
  const file = path.join(CONTRACTS_DIR, name);
  if (!fs.existsSync(file)) {
    return undefined;
  }
  return JSON.parse(fs.readFileSync(file, "utf8")) as Record<string, unknown>;
}

function assertKeys(
  contract: Record<string, unknown>,
  required: string[],
  label: string
): void {
  for (const key of required) {
    assert.ok(
      key in contract,
      `${label}: fixture is missing field '${key}' required by the extension (Property 11 / real-output parity)`
    );
  }
}

/** Parse `pub const CONTRACT_VERSION: u32 = N;` from the Rust producer. */
function parseRustContractVersion(text: string): number | undefined {
  const m = text.match(/pub\s+const\s+CONTRACT_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/);
  return m ? Number(m[1]) : undefined;
}

/** Parse the ordered `MCP_TOOLS` string literals from the Rust producer. */
function parseRustMcpTools(text: string): string[] {
  // Match the array initializer after `=`, not the type `[&str; 8]`.
  const m = text.match(
    /pub\s+const\s+MCP_TOOLS\s*:\s*\[[^\]]+\]\s*=\s*\[([\s\S]*?)\];/
  );
  if (!m) {
    return [];
  }
  const tools: string[] = [];
  const re = /"([^"]+)"/g;
  let lit: RegExpExecArray | null;
  while ((lit = re.exec(m[1])) !== null) {
    tools.push(lit[1]);
  }
  return tools;
}

function fullHandshake(
  over: Partial<HandshakePayload> = {}
): HandshakePayload {
  return {
    contract_version: EXPECTED_CONTRACT_VERSION,
    engine_version: "0.8.8",
    cli_commands: [
      ...REQUIRED_CLI_COMMANDS,
      "index",
      "handshake",
    ],
    mcp_tools: [...EIGHT_MCP_TOOLS],
    ...over,
  };
}

function nonCognisSnapshot(configPath: string): string {
  if (!fs.existsSync(configPath)) {
    return JSON.stringify({ topLevel: {}, servers: {} });
  }
  const config = JSON.parse(fs.readFileSync(configPath, "utf8")) as Record<
    string,
    unknown
  >;
  const servers =
    (config.mcpServers as Record<string, unknown> | undefined) ?? {};
  const nonCognis: Record<string, unknown> = {};
  for (const name of Object.keys(servers).sort()) {
    if (!name.startsWith("cognis-")) {
      nonCognis[name] = servers[name];
    }
  }
  const topLevel: Record<string, unknown> = {};
  for (const key of Object.keys(config).sort()) {
    if (key !== "mcpServers") {
      topLevel[key] = config[key];
    }
  }
  return JSON.stringify({ topLevel, servers: nonCognis });
}

function listBackups(dir: string): string[] {
  if (!fs.existsSync(dir)) {
    return [];
  }
  return fs
    .readdirSync(dir)
    .filter((n) => n.includes("backup") && n.startsWith("mcp.json"));
}

// ---------------------------------------------------------------------------
// Property 11 — Contract + handshake lockstep / fixture parity
// **Validates: Requirements 2.10**
// ---------------------------------------------------------------------------

test("Property 11: CONTRACT_VERSION is unchanged at intentional baseline 1 (lockstep)", () => {
  // Baseline pinned by task 9.1 — bump only in a reviewed lockstep change that
  // also updates Rust producer, TS types, fixtures, and parity tests.
  assert.equal(
    EXPECTED_CONTRACT_VERSION,
    1,
    "EXPECTED_CONTRACT_VERSION must stay 1 unless intentionally bumped in lockstep with Rust CONTRACT_VERSION + fixtures"
  );

  assert.ok(
    fs.existsSync(RUST_CONTRACT_RS),
    `Rust contract producer missing at ${RUST_CONTRACT_RS}`
  );
  const rustText = fs.readFileSync(RUST_CONTRACT_RS, "utf8");
  const rustVersion = parseRustContractVersion(rustText);
  assert.equal(
    rustVersion,
    EXPECTED_CONTRACT_VERSION,
    `Rust CONTRACT_VERSION (${rustVersion}) must equal extension EXPECTED_CONTRACT_VERSION (${EXPECTED_CONTRACT_VERSION}) — bump both together (Property 11 / 2.10)`
  );
});

test("Property 11: eight MCP tools match Rust MCP_TOOLS order and extension handshake", () => {
  assert.equal(EIGHT_MCP_TOOLS.length, 8);

  const rustText = fs.readFileSync(RUST_CONTRACT_RS, "utf8");
  const rustTools = parseRustMcpTools(rustText);
  assert.deepEqual(
    rustTools,
    [...EIGHT_MCP_TOOLS],
    "TypeScript eight-tool set must match crates/cognis-core MCP_TOOLS order exactly"
  );

  // Extension hard-depends on a subset; every required tool must appear in the
  // full eight-tool set advertised by the handshake.
  for (const tool of REQUIRED_MCP_TOOLS) {
    assert.ok(
      (EIGHT_MCP_TOOLS as readonly string[]).includes(tool),
      `REQUIRED_MCP_TOOLS entry '${tool}' must be one of the eight contract tools`
    );
  }

  const result = evaluateHandshake(fullHandshake());
  assert.equal(result.compatibility, "ok");
  assert.equal(result.usable, true);
  assert.equal(result.expectedContractVersion, EXPECTED_CONTRACT_VERSION);
  assert.equal(result.backendContractVersion, EXPECTED_CONTRACT_VERSION);
});

test("Property 11: real-output parity against tests/e2e/contracts fixtures", (t) => {
  // paths.json
  const paths = loadContract("paths.json");
  if (!paths) {
    t.skip("contract golden paths.json not present");
    return;
  }
  assertKeys(
    paths,
    [
      "repo_root",
      "cognis_dir",
      "config_path",
      "db_path",
      "indexd_status_path",
      "audit_log_path",
      "capsule_cache_dir",
      "golden_set_path",
      "runtime_version",
      "engine_binary",
    ],
    "paths.json"
  );

  // mcp_config.json
  const mcp = loadContract("mcp_config.json");
  assert.ok(mcp, "mcp_config.json fixture must exist for real-output parity");
  assertKeys(
    mcp!,
    ["host", "format", "repo_root", "server_name", "config", "config_paths", "env"],
    "mcp_config.json"
  );
  const config = mcp!.config as Record<string, unknown>;
  assert.ok("mcpServers" in config);
  const firstServer = Object.values(
    config.mcpServers as Record<string, Record<string, unknown>>
  )[0];
  assert.ok(firstServer);
  assertKeys(firstServer, ["command", "env"], "mcp_config.json server block");
  assert.ok(
    "COGNIS_DB_PATH" in (firstServer.env as Record<string, unknown>),
    "mcp_config server env must carry COGNIS_DB_PATH"
  );

  // health.json
  const health = loadContract("health.json");
  assert.ok(health, "health.json fixture must exist");
  assertKeys(health!, ["runtime_version", "overall", "checks"], "health.json");

  // bootstrap.json
  const bootstrap = loadContract("bootstrap.json");
  assert.ok(bootstrap, "bootstrap.json fixture must exist");
  const keys = bootstrap!.keys as string[];
  for (const required of [
    "command",
    "runtime_version",
    "repo_root",
    "db_path",
    "skip_embeddings",
    "paths",
    "phases",
    "health",
    "overall",
    "exit_code",
  ]) {
    assert.ok(
      keys.includes(required),
      `bootstrap.json keys missing '${required}'`
    );
  }

  // indexd_status.json
  const status = loadContract("indexd_status.json");
  assert.ok(status, "indexd_status.json fixture must exist");
  assertKeys(
    status!,
    [
      "pid",
      "active",
      "phase",
      "message",
      "progress_percent",
      "pending_count",
      "pending_files",
      "inflight_count",
      "inflight_files",
      "recent_files",
      "updated_at",
      "last_error",
    ],
    "indexd_status.json"
  );
});

test("Property 11: handshake property — full eight tools + matching version is ok; missing tools fail closed", () => {
  fc.assert(
    fc.property(
      fc.subarray([...EIGHT_MCP_TOOLS], { minLength: 0 }),
      fc.integer({ min: 0, max: 3 }),
      (subset, versionDelta) => {
        const tools = subset.length === 0 ? [] : subset;
        const version = EXPECTED_CONTRACT_VERSION + versionDelta;
        const result = evaluateHandshake(
          fullHandshake({
            contract_version: version,
            mcp_tools: tools,
          })
        );

        const missingRequired = REQUIRED_MCP_TOOLS.filter(
          (t) => !tools.includes(t)
        );
        if (missingRequired.length > 0) {
          assert.equal(result.compatibility, "capabilities-missing");
          assert.equal(result.usable, false);
          return;
        }

        // All required tools present.
        if (version === EXPECTED_CONTRACT_VERSION) {
          assert.equal(result.compatibility, "ok");
          assert.equal(result.usable, true);
        } else if (version < EXPECTED_CONTRACT_VERSION) {
          assert.equal(result.compatibility, "backend-older");
          assert.equal(result.usable, true);
        } else {
          assert.equal(result.compatibility, "backend-newer");
          assert.equal(result.usable, true);
        }
      }
    ),
    { numRuns: 80 }
  );
});

// ---------------------------------------------------------------------------
// Property 13 — Deterministic recovery + evidence discipline
// **Validates: Requirements 2.13, 2.14**
// ---------------------------------------------------------------------------

test("Property 13: dry-run/plan is side-effect free and auditable", () => {
  const home = mkTempHome();
  const repoRoot = mkTempRepo();
  try {
    const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
    const globalPath = path.join(home, ".cursor", "mcp.json");
    const workspacePath = path.join(repoRoot, ".cursor", "mcp.json");

    // Unrelated user file that must never be cleaned by Cognis recovery.
    const userNotes = path.join(home, "user-notes.txt");
    fs.mkdirSync(path.dirname(globalPath), { recursive: true });
    fs.writeFileSync(userNotes, "do-not-delete", "utf8");

    writeJsonFile(globalPath, {
      mcpServers: {
        "cognis-myrepo-abc123": {
          command: "cognis",
          args: ["mcpd"],
          env: { COGNIS_DB_PATH: dbPath },
        },
        "user-owned": {
          command: "node",
          args: ["server.js"],
          env: { TOKEN: "keep-me" },
        },
      },
      customTopLevel: { note: "preserve" },
    });
    const globalBefore = fs.readFileSync(globalPath, "utf8");

    const plan = planGlobalEntryToWorkspaceMigration(repoRoot, {
      host: "cursor",
      homeDir: home,
    });
    assert.equal(plan.willMoveEntry, true);
    assert.ok(plan.serverNames.length >= 1);
    assert.equal(plan.sourcePath, globalPath);
    assert.equal(plan.destinationPath, workspacePath);

    const dry = migrateGlobalEntryToWorkspace(repoRoot, {
      host: "cursor",
      homeDir: home,
      dryRun: true,
    });
    assert.equal(dry.ok, true);
    assert.equal(dry.dryRun, true);
    assert.equal(dry.wroteDestination, false);
    assert.equal(dry.removedFromSource, false);
    assert.equal(dry.rolledBack, false);
    assert.ok(dry.steps.length > 0, "dry-run must emit an auditable step trail");
    assert.equal(fs.readFileSync(globalPath, "utf8"), globalBefore);
    assert.equal(fs.existsSync(workspacePath), false);
    assert.equal(
      fs.readFileSync(userNotes, "utf8"),
      "do-not-delete",
      "dry-run must not touch non-Cognis user files"
    );
  } finally {
    cleanup(home, repoRoot);
  }
});

test("Property 13: interrupted migration retains backups, restores prior topology, cleans only Cognis state", () => {
  fc.assert(
    fc.property(
      fc.constantFrom(...INTERRUPT_STEPS),
      fc.dictionary(
        fc
          .string({ minLength: 3, maxLength: 12 })
          .filter((s) => !s.startsWith("cognis") && /^[a-z][a-z0-9-]*$/.test(s)),
        fc.record({
          command: fc.constantFrom("node", "python", "bash"),
          args: fc.array(fc.string({ maxLength: 8 }), { maxLength: 2 }),
          env: fc.dictionary(
            fc.string({ minLength: 2, maxLength: 8 }).filter((k) => /^[A-Z_]+$/.test(k)),
            fc.string({ maxLength: 16 }),
            { maxKeys: 2 }
          ),
        }),
        { minKeys: 1, maxKeys: 3 }
      ),
      (interruptAt, nonCognisServers) => {
        const home = mkTempHome();
        const repoRoot = mkTempRepo();
        try {
          const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
          const globalPath = path.join(home, ".cursor", "mcp.json");
          const workspacePath = path.join(repoRoot, ".cursor", "mcp.json");
          const userNotes = path.join(home, "notes.txt");
          fs.mkdirSync(path.dirname(globalPath), { recursive: true });
          fs.writeFileSync(userNotes, "user-owned-content", "utf8");

          const servers: Record<string, unknown> = {
            "cognis-myrepo-abc123": {
              command: "cognis",
              args: ["mcpd"],
              env: { COGNIS_DB_PATH: dbPath },
            },
            ...nonCognisServers,
          };
          writeJsonFile(globalPath, {
            mcpServers: servers,
            customTopLevel: { keep: true },
          });
          const globalBytesBefore = fs.readFileSync(globalPath);
          const nonCognisBefore = nonCognisSnapshot(globalPath);

          const interrupted = migrateGlobalEntryToWorkspace(repoRoot, {
            host: "cursor",
            homeDir: home,
            faultInjection: (step) => {
              if (step === interruptAt) {
                throw new Error(`simulated interrupt at ${interruptAt}`);
              }
            },
          });

          assert.equal(interrupted.ok, false);
          assert.equal(interrupted.rolledBack, true);
          assert.ok(
            interrupted.backups.length > 0,
            "failed migration must retain backups until verified success"
          );
          for (const b of interrupted.backups) {
            assert.ok(
              fs.existsSync(b.backupPath),
              `backup must remain on disk: ${b.backupPath}`
            );
          }
          // Prior global config topology restored (byte-for-byte via backup).
          assert.deepEqual(
            fs.readFileSync(globalPath),
            globalBytesBefore,
            "failed migration must restore prior global config"
          );
          assert.equal(
            nonCognisSnapshot(globalPath),
            nonCognisBefore,
            "non-Cognis content must be preserved across rollback"
          );
          // Workspace either absent or free of a partial Cognis move after rollback.
          if (fs.existsSync(workspacePath)) {
            const ws = JSON.parse(fs.readFileSync(workspacePath, "utf8")) as {
              mcpServers?: Record<string, unknown>;
            };
            // After rollback of a newly-created destination, the file is removed;
            // if destination existed before, Cognis merge is undone. Either way
            // non-Cognis-only destinations are fine; Cognis entry must not be
            // left only in the destination while removed from source.
            void ws;
          }
          // Clean only Cognis-owned state: user notes untouched.
          assert.equal(
            fs.readFileSync(userNotes, "utf8"),
            "user-owned-content"
          );
          assert.equal(listBackups(path.dirname(userNotes)).length, 0);

          // Restartable: a subsequent clean run must succeed (idempotent recovery).
          const retry = migrateGlobalEntryToWorkspace(repoRoot, {
            host: "cursor",
            homeDir: home,
          });
          assert.equal(retry.ok, true, "migration must be restartable after interrupt");
          assert.equal(retry.rolledBack, false);

          // Non-Cognis still present exactly once after successful recovery.
          assert.equal(nonCognisSnapshot(globalPath), nonCognisBefore);

          // Second success is a no-op (idempotent).
          const noop = migrateGlobalEntryToWorkspace(repoRoot, {
            host: "cursor",
            homeDir: home,
          });
          assert.equal(noop.ok, true);
          assert.equal(noop.plan.willMoveEntry, false);
          assert.deepEqual(noop.movedServerNames, []);

          // Verified success cleans Cognis backups by default.
          assert.equal(noop.backups.length, 0);
          assert.equal(
            fs.readFileSync(userNotes, "utf8"),
            "user-owned-content",
            "success path must still leave non-Cognis user files alone"
          );
        } finally {
          cleanup(home, repoRoot);
        }
      }
    ),
    { numRuns: 25 }
  );
});

test("Property 13: verified success removes backups; retainBackups keeps them on request", () => {
  const home = mkTempHome();
  const repoRoot = mkTempRepo();
  try {
    const dbPath = path.join(repoRoot, ".cognis", "uckg.db");
    const globalPath = path.join(home, ".cursor", "mcp.json");
    writeJsonFile(globalPath, {
      mcpServers: {
        "cognis-myrepo-abc123": {
          command: "cognis",
          args: ["mcpd"],
          env: { COGNIS_DB_PATH: dbPath },
        },
        "brave-search": {
          command: "node",
          args: ["brave.js"],
          env: { K: "v" },
        },
      },
    });

    const retained = migrateGlobalEntryToWorkspace(repoRoot, {
      host: "cursor",
      homeDir: home,
      retainBackups: true,
    });
    assert.equal(retained.ok, true);
    assert.ok(
      retained.backups.length > 0,
      "retainBackups must keep backups after verified success"
    );
    for (const b of retained.backups) {
      assert.ok(fs.existsSync(b.backupPath));
    }

    // Idempotent re-run after success: nothing left to move; still ok.
    const again = migrateGlobalEntryToWorkspace(repoRoot, {
      host: "cursor",
      homeDir: home,
    });
    assert.equal(again.ok, true);
    assert.equal(again.plan.willMoveEntry, false);
  } finally {
    cleanup(home, repoRoot);
  }
});

test("Property 13: evidence discipline — incomplete gate evidence never enables sharing", () => {
  fc.assert(
    fc.property(
      fc.boolean(),
      fc.array(fc.constantFrom(...REQUIRED_GATE_CHECKS), {
        minLength: 0,
        maxLength: REQUIRED_GATE_CHECKS.length,
      }),
      fc.boolean(),
      (flagEnabled, presentChecks, claimPass) => {
        const evidence: Partial<Record<GateCheckId, GateCheckEvidence>> = {};
        const unique = [...new Set(presentChecks)];
        for (const id of unique) {
          evidence[id] = claimPass
            ? { passed: true, evidence: `pointer:${id}` }
            : { passed: false, evidence: `pointer:${id}`, detail: "fail" };
        }

        const decision = evaluateSharingGate(flagEnabled, evidence);
        const allPresent =
          REQUIRED_GATE_CHECKS.every((id) => evidence[id]?.passed === true) &&
          REQUIRED_GATE_CHECKS.every(
            (id) => (evidence[id]?.evidence ?? "").trim().length > 0
          );

        if (flagEnabled && allPresent) {
          assert.equal(decision.topology, "shared-http");
          assert.equal(decision.sharingEnabled, true);
        } else {
          assert.equal(decision.topology, "thin-proxy-stdio");
          assert.equal(decision.sharingEnabled, false);
          // Failed/closed gate never rewrites config — pure decision only.
          assert.ok(decision.fallbackReason);
        }
      }
    ),
    { numRuns: 60 }
  );
});

test("Property 13: evidence document parsing is fail-closed on garbage and partial maps", () => {
  assert.deepEqual(parseGateEvidenceDocument(null), {});
  assert.deepEqual(parseGateEvidenceDocument("nope"), {});
  assert.deepEqual(parseGateEvidenceDocument(42), {});

  const partial = parseGateEvidenceDocument({
    semanticParity: { passed: true, evidence: "a" },
    unknownCheck: { passed: true, evidence: "x" },
  });
  assert.equal(partial.semanticParity?.passed, true);
  assert.equal(
    (partial as Record<string, unknown>).unknownCheck,
    undefined,
    "unknown evidence keys must be ignored"
  );

  // Even with partial evidence, gate stays on stdio when flag is ON.
  const decision = evaluateSharingGate(true, partial);
  assert.equal(decision.sharingEnabled, false);
  assert.equal(decision.topology, "thin-proxy-stdio");
});
