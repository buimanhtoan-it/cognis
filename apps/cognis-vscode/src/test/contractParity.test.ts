/**
 * Cross-language contract parity tests.
 *
 * The Python apps (cognis-cli, cognis-indexd) emit JSON that this extension
 * parses into the interfaces in `types.ts` and normalizes in `indexd.ts` /
 * `mcpConfig.ts`. Those JSON shapes are a contract across two languages; if the
 * Python side drops or renames a field, the extension breaks silently.
 *
 * The Python E2E suite captures the *real* CLI/daemon output into golden
 * skeleton files under `tests/e2e/contracts/` (key names + value types, with
 * environment-specific values stripped). These tests load those same goldens
 * and assert every field the extension actually reads is present in the
 * contract — so a drift on the Python side fails here, and a drift on the
 * TypeScript side (reading a field the contract doesn't promise) fails too.
 *
 * If a contract change is intentional: regenerate the goldens with
 * `COGNIS_UPDATE_CONTRACTS=1 pytest -m e2e -k contract_snapshots`, then update
 * the matching interface in `types.ts` and the expected key list below.
 */
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import test from "node:test";

// Resolve tests/e2e/contracts relative to this compiled test file. From
// apps/cognis-vscode/out/test/ the repo root is four levels up.
const CONTRACTS_DIR = path.resolve(
  __dirname,
  "..",
  "..",
  "..",
  "..",
  "tests",
  "e2e",
  "contracts"
);

function loadContract(name: string): Record<string, unknown> {
  const file = path.join(CONTRACTS_DIR, name);
  assert.ok(
    fs.existsSync(file),
    `missing contract golden ${file}. Generate it with ` +
      `COGNIS_UPDATE_CONTRACTS=1 pytest -m e2e -k contract_snapshots`
  );
  return JSON.parse(fs.readFileSync(file, "utf8")) as Record<string, unknown>;
}

/** Assert every key in `required` is present in `contract`. */
function assertKeys(
  contract: Record<string, unknown>,
  required: string[],
  label: string
): void {
  for (const key of required) {
    assert.ok(
      key in contract,
      `${label}: contract is missing field '${key}' the extension reads. ` +
        `The Python output shape drifted from types.ts.`
    );
  }
}

test("WorkspacePaths interface matches the real `cognis-cli paths` contract", () => {
  const contract = loadContract("paths.json");
  // Fields read in workspace.ts / indexd.ts via the WorkspacePaths type.
  assertKeys(
    contract,
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
      "commands",
    ],
    "WorkspacePaths"
  );
  const commands = contract.commands as Record<string, unknown>;
  assertKeys(
    commands,
    [
      "python",
      "cognis_cli",
      "cognis_mcpd",
      "cognis_indexd",
      "cognis_cli_module",
      "cognis_mcpd_module",
      "cognis_indexd_module",
    ],
    "WorkspacePaths.commands"
  );
});

test("McpConfigPayload interface matches the real `cognis-cli mcp-config` contract", () => {
  const contract = loadContract("mcp_config.json");
  // Fields read in mcpConfig.ts via the McpConfigPayload type.
  assertKeys(
    contract,
    ["host", "format", "repo_root", "server_name", "config", "config_paths", "env"],
    "McpConfigPayload"
  );
  const config = contract.config as Record<string, unknown>;
  assert.ok("mcpServers" in config, "McpConfigPayload.config must carry mcpServers");
  const servers = config.mcpServers as Record<string, Record<string, unknown>>;
  const firstServer = Object.values(servers)[0];
  assert.ok(firstServer, "expected at least one server block in the contract");
  // McpServerBlock: command + env are required; args is optional.
  assertKeys(firstServer, ["command", "env"], "McpServerBlock");
  const env = firstServer.env as Record<string, unknown>;
  assert.ok(
    "COGNIS_DB_PATH" in env,
    "MCP server env must carry COGNIS_DB_PATH (envMatchesRepo depends on it)"
  );
});

test("HealthReport interface matches the real `cognis-cli health` contract", () => {
  const contract = loadContract("health.json");
  assertKeys(contract, ["runtime_version", "overall", "checks"], "HealthReport");
  const checks = contract.checks as Record<string, Record<string, unknown>>;
  const sample = Object.values(checks)[0];
  assert.ok(sample, "expected a sample health check in the contract");
  assertKeys(sample, ["status", "message"], "HealthReport.checks[*]");
});

test("BootstrapPayload interface matches the real `cognis-cli bootstrap` contract", () => {
  const contract = loadContract("bootstrap.json");
  const keys = contract.keys as string[];
  // Fields read in workspace.ts / extension.ts via the BootstrapPayload type.
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
      `BootstrapPayload: contract missing top-level field '${required}'`
    );
  }
});

test("IndexStatusReport normalizer matches the real indexd status-file contract", () => {
  const contract = loadContract("indexd_status.json");
  // snake_case keys read by normalizeIndexStatus() in indexd.ts.
  assertKeys(
    contract,
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
    "IndexStatusReport (status file)"
  );
});
