import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import { runCliJson } from "./cli";
import { envMatchesExpected, envMatchesRepo, expectedDbPathForRepo } from "./mcpEnv";
import {
  getGlobalMcpConfigPath as buildGlobalMcpConfigPath,
  getWorkspaceMcpConfigPath,
  resolveMcpConfigPath as buildMcpConfigPath,
  type McpConfigScope,
} from "./mcpConfigPaths";
import { deriveMcpServerName, isCognisMcpServerName } from "./mcpServerName";
import { buildHttpMcpServerBlock, isHttpServerBlock, rewriteServerBlockToBinary } from "./mcpServer";
import { isManagedBinaryActive, managedBinaryPath } from "./binary";
import { modelEnv } from "./model";
import type { McpConfigPayload, McpServerBlock } from "./types";

export type { McpConfigScope } from "./mcpConfigPaths";
export { deriveMcpServerName, isCognisMcpServerName } from "./mcpServerName";

type ConfiguredMcpHost = "auto" | "cursor" | "vscode" | "kiro" | "claude";
type McpTimeoutSetting =
  | "mcpSoftTimeoutSeconds"
  | "mcpHardTimeoutSeconds"
  | "mcpDiscoverSemanticTimeoutSeconds"
  | "mcpSemanticCooldownSeconds";

const MCP_TIMEOUT_ENV_MAP: Array<{
  setting: McpTimeoutSetting;
  env: string;
}> = [
  {
    setting: "mcpSoftTimeoutSeconds",
    env: "COGNIS_MCP_SOFT_TIMEOUT_S",
  },
  {
    setting: "mcpHardTimeoutSeconds",
    env: "COGNIS_MCP_HARD_TIMEOUT_S",
  },
  {
    setting: "mcpDiscoverSemanticTimeoutSeconds",
    env: "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S",
  },
  {
    setting: "mcpSemanticCooldownSeconds",
    env: "COGNIS_MCP_SEMANTIC_COOLDOWN_S",
  },
];

type RepoMcpMatch = {
  configPath: string;
  serverName: string;
  block: McpServerBlock;
};

export function resolveMcpHost(): "cursor" | "vscode" | "kiro" | "claude" {
  const configured = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("mcpHost") as ConfiguredMcpHost | undefined;
  if (!configured || configured === "auto") {
    return detectDefaultHost();
  }
  return configured;
}

export function resolveMcpConfigScope(): McpConfigScope {
  return vscode.workspace
    .getConfiguration("cognis")
    .get<McpConfigScope>("mcpConfigScope", "workspace");
}

function detectDefaultHost(): "cursor" | "vscode" | "kiro" | "claude" {
  const appName = vscode.env.appName.toLowerCase();
  // Kiro is a VS Code fork (its appName does NOT contain "code"), but it reads
  // MCP from .kiro/settings/mcp.json — detect it before the generic "code" check
  // so we never mis-write a Kiro workspace as if it were VS Code.
  if (appName.includes("kiro")) {
    return "kiro";
  }
  if (appName.includes("cursor")) {
    return "cursor";
  }
  if (appName.includes("code")) {
    return "vscode";
  }
  return "vscode";
}

export function getGlobalMcpConfigPath(host: string): string {
  return buildGlobalMcpConfigPath(host, os.homedir());
}

export { getWorkspaceMcpConfigPath };

export function resolveMcpConfigPath(
  host: string,
  repoRoot: string | undefined,
  scope: McpConfigScope
): string {
  return buildMcpConfigPath(host, repoRoot, scope, os.homedir());
}

export function getMcpConfigPath(host: string, repoRoot?: string): string {
  const scope = repoRoot ? resolveMcpConfigScope() : "global";
  return resolveMcpConfigPath(host, repoRoot, scope);
}

function readJsonFile(filePath: string): Record<string, unknown> {
  if (!fs.existsSync(filePath)) {
    return {};
  }
  const raw = fs.readFileSync(filePath, "utf8").trim();
  if (!raw) {
    return {};
  }
  return JSON.parse(raw) as Record<string, unknown>;
}

function writeJsonFile(filePath: string, data: Record<string, unknown>): void {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(data, null, 2)}\n`, "utf8");
}

function resolveMcpEnvOverrides(): Record<string, string> {
  const config = vscode.workspace.getConfiguration("cognis");
  const overrides: Record<string, string> = {};
  for (const entry of MCP_TIMEOUT_ENV_MAP) {
    const configured = config.get<number>(entry.setting, 0);
    if (configured > 0) {
      overrides[entry.env] = String(configured);
    }
  }
  const warmSemantic = config.get<boolean>("mcpWarmSemanticOnStartup");
  if (warmSemantic === true) {
    overrides.COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP = "1";
  } else if (warmSemantic === false) {
    overrides.COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP = "0";
  }
  return overrides;
}

function applyWorkspaceEnvOverrides(
  repoRoot: string,
  payload: McpConfigPayload
): McpConfigPayload {
  const server = payload.config.mcpServers[payload.server_name];
  const mergedEnv = {
    ...server.env,
    ...resolveMcpEnvOverrides(),
    // Point the editor-spawned server at the managed semantic model (if
    // installed) so `diffuse_context` / `discover_symbols` get real embeddings.
    ...modelEnv(),
  };
  return {
    ...payload,
    env: mergedEnv,
    config: {
      ...payload.config,
      mcpServers: {
        ...payload.config.mcpServers,
        [payload.server_name]: {
          ...server,
          env: mergedEnv,
        },
      },
    },
  };
}

/**
 * Point a generated mcp.json payload at the managed single ``cognis`` binary
 * when it is the active backend: rewrite the stdio server block's
 * ``command``/``args`` to ``<binary> mcpd`` (Requirement 1.1 — the editor
 * launches the binary directly, no Python entry point), preserving the env and
 * server name. A no-op when the binary backend is not active, or for an HTTP
 * (url) block. The CLI round-trip still computes the env (COGNIS_DB_PATH,
 * timeouts) — only the launch command is normalized to the binary.
 */
export function applyBinaryBackend(payload: McpConfigPayload): McpConfigPayload {
  if (!isManagedBinaryActive()) {
    return payload;
  }
  const binaryPath = managedBinaryPath();
  if (!binaryPath) {
    return payload;
  }
  const serverName = payload.server_name;
  const block = payload.config.mcpServers[serverName];
  if (!block || isHttpServerBlock(block)) {
    return payload;
  }
  const rewritten = rewriteServerBlockToBinary(block, binaryPath);
  return {
    ...payload,
    config: {
      ...payload.config,
      mcpServers: {
        ...payload.config.mcpServers,
        [serverName]: rewritten,
      },
    },
  };
}

function listMcpConfigCandidates(repoRoot: string, host: string): string[] {
  const paths: string[] = [getGlobalMcpConfigPath(host)];
  const workspacePath = getWorkspaceMcpConfigPath(repoRoot, host);
  if (workspacePath) {
    paths.unshift(workspacePath);
  }
  return [...new Set(paths)];
}

function listCognisServerEntries(
  configPath: string
): Array<{ serverName: string; block: McpServerBlock }> {
  if (!fs.existsSync(configPath)) {
    return [];
  }
  const existing = readJsonFile(configPath);
  const servers =
    (existing.mcpServers as Record<string, McpServerBlock> | undefined) ?? {};
  return Object.entries(servers)
    .filter(([name]) => isCognisMcpServerName(name))
    .map(([serverName, block]) => ({ serverName, block }));
}

function findConfiguredServerBlockForRepo(
  repoRoot: string,
  host: string
): RepoMcpMatch | undefined {
  for (const configPath of listMcpConfigCandidates(repoRoot, host)) {
    for (const { serverName, block } of listCognisServerEntries(configPath)) {
      if (envMatchesRepo(repoRoot, block.env ?? {})) {
        return { configPath, serverName, block };
      }
    }
  }
  return undefined;
}

/** Where this repo's Cognis MCP entry lives (workspace or global mcp.json). */
export function getMcpConfigMatchForRepo(
  repoRoot: string
): RepoMcpMatch | undefined {
  return findConfiguredServerBlockForRepo(repoRoot, resolveMcpHost());
}

function removeStaleCognisEntriesForRepo(
  servers: Record<string, unknown>,
  repoRoot: string,
  keepServerName: string
): void {
  const expectedDb = expectedDbPathForRepo(repoRoot);
  for (const [name, value] of Object.entries(servers)) {
    if (name === keepServerName || !isCognisMcpServerName(name)) {
      continue;
    }
    const env = (value as McpServerBlock).env ?? {};
    const dbPath = env.COGNIS_DB_PATH;
    if (dbPath && path.resolve(dbPath) === expectedDb) {
      delete servers[name];
    }
  }
}

export async function fetchMcpConfig(
  repoRoot: string,
  host?: string
): Promise<McpConfigPayload> {
  const resolvedHost = host ?? resolveMcpHost();
  const serverName = deriveMcpServerName(repoRoot);
  const payload = await runCliJson<McpConfigPayload>(repoRoot, [
    "mcp-config",
    "--host",
    resolvedHost,
    "--server-name",
    serverName,
    "--minimal-env",
  ]);
  return applyBinaryBackend(applyWorkspaceEnvOverrides(repoRoot, payload));
}

export function isCognisMcpConfiguredForRepo(repoRoot: string): boolean {
  const host = resolveMcpHost();
  const match = findConfiguredServerBlockForRepo(repoRoot, host);
  if (!match) {
    return false;
  }
  return match.serverName === deriveMcpServerName(repoRoot);
}

/**
 * True when this repo's cognis mcp.json entry is the HTTP (url) form — i.e. it
 * points at the panel-managed standalone server rather than the editor-managed
 * stdio command. Used on activation to detect a *dangling* http config (server
 * not running) so we can fall back to stdio and keep AI tools working.
 */
export function isHttpMcpConfiguredForRepo(repoRoot: string): boolean {
  const host = resolveMcpHost();
  const match = findConfiguredServerBlockForRepo(repoRoot, host);
  if (!match || match.serverName !== deriveMcpServerName(repoRoot)) {
    return false;
  }
  return isHttpServerBlock(match.block);
}

export async function hasExpectedMcpConfigForRepo(
  repoRoot: string
): Promise<boolean> {
  const host = resolveMcpHost();
  const match = findConfiguredServerBlockForRepo(repoRoot, host);
  if (!match) {
    return false;
  }
  if (match.serverName !== deriveMcpServerName(repoRoot)) {
    return false;
  }
  try {
    const payload = await fetchMcpConfig(repoRoot);
    const expectedBlock = payload.config.mcpServers[payload.server_name];
    if (!expectedBlock) {
      return false;
    }
    return envMatchesExpected(match.block.env ?? {}, expectedBlock.env);
  } catch {
    return false;
  }
}

export async function isMcpConfigured(repoRoot: string): Promise<boolean> {
  return hasExpectedMcpConfigForRepo(repoRoot);
}

export async function enableMcpForWorkspace(
  repoRoot: string
): Promise<{ configPath: string; payload: McpConfigPayload; serverName: string }> {
  const payload = await fetchMcpConfig(repoRoot);
  const serverName = payload.server_name;
  const configPath = getMcpConfigPath(payload.host, repoRoot);
  const existing = readJsonFile(configPath);
  const servers =
    (existing.mcpServers as Record<string, unknown> | undefined) ?? {};
  removeStaleCognisEntriesForRepo(servers, repoRoot, serverName);
  servers[serverName] = payload.config.mcpServers[serverName];
  existing.mcpServers = servers;
  writeJsonFile(configPath, existing);
  return { configPath, payload, serverName };
}

/**
 * Write the workspace mcp.json so the editor connects to a *running* HTTP MCP
 * server at *url* (the panel-managed standalone server). Mirrors
 * ``enableMcpForWorkspace``'s merge — same config path, same stale-entry
 * cleanup — but swaps the stdio block for the ``{type:"http", url}`` form. No
 * CLI round-trip needed (the URL is owned by the extension), so it is sync.
 */
export function writeHttpMcpConfig(
  repoRoot: string,
  url: string
): { configPath: string; serverName: string } {
  const host = resolveMcpHost();
  const serverName = deriveMcpServerName(repoRoot);
  const configPath = getMcpConfigPath(host, repoRoot);
  const existing = readJsonFile(configPath);
  const servers =
    (existing.mcpServers as Record<string, unknown> | undefined) ?? {};
  removeStaleCognisEntriesForRepo(servers, repoRoot, serverName);
  servers[serverName] = buildHttpMcpServerBlock(url);
  existing.mcpServers = servers;
  writeJsonFile(configPath, existing);
  return { configPath, serverName };
}

export async function disableMcpForWorkspace(
  repoRoot: string
): Promise<{ configPath: string; removed: boolean; serverName?: string }> {
  const host = resolveMcpHost();
  const match = findConfiguredServerBlockForRepo(repoRoot, host);
  if (!match) {
    return { configPath: getMcpConfigPath(host, repoRoot), removed: false };
  }
  const existing = readJsonFile(match.configPath);
  const servers =
    (existing.mcpServers as Record<string, unknown> | undefined) ?? {};
  const removed = match.serverName in servers;
  if (removed) {
    delete servers[match.serverName];
    existing.mcpServers = servers;
    writeJsonFile(match.configPath, existing);
  }
  return {
    configPath: match.configPath,
    removed,
    serverName: match.serverName,
  };
}

/**
 * Delete every Cognis-managed server entry (``cognis`` / ``cognis-<slug>``)
 * from a server map in place, returning the names removed. Pure (no fs/vscode)
 * so the matching rule stays unit-testable.
 */
export function filterOutCognisServers(
  servers: Record<string, unknown>
): string[] {
  const removed: string[] = [];
  for (const name of Object.keys(servers)) {
    if (isCognisMcpServerName(name)) {
      delete servers[name];
      removed.push(name);
    }
  }
  return removed;
}

/**
 * Clear ALL Cognis MCP wiring across the global host config and (optionally)
 * the current workspace config.
 *
 * This backs the "prepare for uninstall" cleanup: because MCP config is written
 * globally by default (one ``~/.cursor/mcp.json`` shared by every indexed repo),
 * removing just the current workspace's entry would leave orphaned ``cognis-*``
 * servers that the MCP host keeps trying to spawn after the extension is gone.
 * Scanning the global file removes every repo's entry in one pass. Returns the
 * files touched and the server names removed from each.
 */
export async function removeAllCognisMcpEntries(
  repoRoot?: string
): Promise<Array<{ configPath: string; serverNames: string[] }>> {
  const host = resolveMcpHost();
  const candidates = new Set<string>([getGlobalMcpConfigPath(host)]);
  if (repoRoot) {
    const workspacePath = getWorkspaceMcpConfigPath(repoRoot, host);
    if (workspacePath) {
      candidates.add(workspacePath);
    }
  }
  const touched: Array<{ configPath: string; serverNames: string[] }> = [];
  for (const configPath of candidates) {
    if (!fs.existsSync(configPath)) {
      continue;
    }
    const existing = readJsonFile(configPath);
    const servers =
      (existing.mcpServers as Record<string, unknown> | undefined) ?? {};
    const removed = filterOutCognisServers(servers);
    if (removed.length === 0) {
      continue;
    }
    existing.mcpServers = servers;
    writeJsonFile(configPath, existing);
    touched.push({ configPath, serverNames: removed });
  }
  return touched;
}

export async function showMcpConfigPreview(repoRoot: string): Promise<void> {
  const payload = await fetchMcpConfig(repoRoot);
  const doc = await vscode.workspace.openTextDocument({
    content: JSON.stringify(payload.config, null, 2),
    language: "json",
  });
  await vscode.window.showTextDocument(doc, { preview: true });
}
