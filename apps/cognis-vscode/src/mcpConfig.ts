import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import { runCliJson } from "./cli";
import { envMatchesExpected, envMatchesRepo } from "./mcpEnv";
import {
  getGlobalMcpConfigPath as buildGlobalMcpConfigPath,
  getWorkspaceMcpConfigPath,
  resolveMcpConfigPath as buildMcpConfigPath,
  type McpConfigScope,
} from "./mcpConfigPaths";
import { deriveMcpServerName, isCognisMcpServerName } from "./mcpServerName";
import { canonicalRepoIdentity, dedupeCognisOwnersByIdentity } from "./mcpCanonical";
import { buildHttpMcpServerBlock, isHttpServerBlock, rewriteServerBlockToBinary, rewriteServerBlockToThinProxy, type McpStdioMode } from "./mcpServer";
import {
  isLiveSharedHttpAllowed,
  resolveSharingGate,
  type SharingGateDecision,
} from "./mcpSharingGate";
import { isManagedBinaryActive, managedBinaryPath } from "./binary";
import { modelEnv } from "./model";
import type { McpConfigPayload, McpServerBlock } from "./types";

export type { McpConfigScope } from "./mcpConfigPaths";
export { deriveMcpServerName, isCognisMcpServerName } from "./mcpServerName";
export {
  evaluateSharingGate,
  isLiveSharedHttpAllowed,
  isSharedHttpAllowed,
  resolveMcpSharedHttpFlag,
  resolveSharingGate,
  selectSharingTopology,
  type GateCheckId,
  type SharingGateDecision,
  type SharingTopology,
} from "./mcpSharingGate";

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
  // Without a repo root there is nothing to scope to (e.g. global-only
  // cleanup passes), so the global host config is the only target.
  if (!repoRoot) {
    return resolveMcpConfigPath(host, undefined, "global");
  }
  // A repo-scoped enable defaults to workspace scope and only writes global
  // when the user has *explicitly* opted in via `cognis.mcpConfigScope`. This
  // guarantees a repo enable never silently falls back into the shared global
  // host config (which would fan a heavy daemon out across every host × repo).
  const scope = resolveMcpConfigScope();
  return resolveMcpConfigPath(host, repoRoot, scope);
}

export function readJsonFile(filePath: string): Record<string, unknown> {
  if (!fs.existsSync(filePath)) {
    return {};
  }
  const raw = fs.readFileSync(filePath, "utf8").trim();
  if (!raw) {
    return {};
  }
  return JSON.parse(raw) as Record<string, unknown>;
}

/**
 * Per-file in-process lock guarding concurrent {@link writeJsonFile} calls to
 * the same path. Config writes are synchronous, so this only guards against
 * re-entrant writes within one extension host; the temp-file + rename below is
 * what makes the on-disk commit atomic against *other* processes and crashes.
 * Keyed by the resolved absolute path so two aliases of the same file share a
 * lock.
 */
const writeLocks = new Set<string>();

/**
 * Atomically write a JSON config file: serialize to a sibling temp file,
 * `fsync` its contents to durable storage, then `rename` it over the
 * destination (an atomic replace on POSIX and NTFS). This mirrors indexd's
 * `write_status_file` (tmp + rename with a short retry) so an interrupted or
 * crashing write can never leave a truncated/half-written `mcp.json` — the
 * destination either holds the previous complete file or the new complete file.
 *
 * The output format is byte-identical to the previous plain truncating write
 * (2-space indented JSON + a single trailing newline) so existing callers and
 * readers see no change.
 *
 * Guarded by a per-file lock (keyed on the resolved path) so re-entrant writes
 * to the same config within one process serialize rather than racing on the
 * shared temp file.
 */
export function writeJsonFile(
  filePath: string,
  data: Record<string, unknown>
): void {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  const lockKey = path.resolve(filePath);
  if (writeLocks.has(lockKey)) {
    // A re-entrant write to the same file is already in flight on this call
    // stack; fall back to a direct durable write to avoid deadlocking. This is
    // not expected on the synchronous config paths but keeps the lock safe.
    fs.writeFileSync(filePath, `${JSON.stringify(data, null, 2)}\n`, "utf8");
    return;
  }
  writeLocks.add(lockKey);
  try {
    const payload = `${JSON.stringify(data, null, 2)}\n`;
    // Unique temp name per write so concurrent writers to sibling files (or a
    // retried write) never clobber each other's staging file.
    const tmp = path.join(
      dir,
      `.${path.basename(filePath)}.${process.pid}.${Date.now()}.tmp`
    );
    // Write + fsync the temp file so its bytes are durable before we rename.
    const fd = fs.openSync(tmp, "w");
    try {
      fs.writeFileSync(fd, payload, "utf8");
      fs.fsyncSync(fd);
    } finally {
      fs.closeSync(fd);
    }
    try {
      renameWithRetry(tmp, filePath);
    } catch (err) {
      // Rename failed after retries — remove the orphan temp file so we don't
      // litter the config directory, then surface the error.
      try {
        fs.rmSync(tmp, { force: true });
      } catch {
        /* best effort */
      }
      throw err;
    }
  } finally {
    writeLocks.delete(lockKey);
  }
}

/**
 * Rename `tmp` over `dest`, retrying a few times with a short backoff. On
 * Windows a concurrent reader (an MCP host tailing `mcp.json`) can momentarily
 * hold the destination open, producing a transient `EPERM`/`EBUSY` sharing
 * violation rather than a real failure — the same hazard indexd's
 * `write_status_file` guards against.
 */
function renameWithRetry(tmp: string, dest: string): void {
  let lastErr: unknown;
  for (let attempt = 0; attempt < 10; attempt += 1) {
    try {
      fs.renameSync(tmp, dest);
      return;
    } catch (err) {
      lastErr = err;
      const code = (err as NodeJS.ErrnoException).code;
      if (code !== "EPERM" && code !== "EBUSY" && code !== "EACCES") {
        throw err;
      }
      sleepSync(20 * (attempt + 1));
    }
  }
  throw lastErr instanceof Error
    ? lastErr
    : new Error("atomic config rename failed");
}

/** Block the calling thread for `ms` — the config write path is synchronous. */
function sleepSync(ms: number): void {
  const shared = new Int32Array(new SharedArrayBuffer(4));
  Atomics.wait(shared, 0, 0, ms);
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
  // Extension-generated configs always carry an explicit policy. Keep absent
  // env => Eager reserved for legacy/direct engine launches.
  const warmSemantic = config.get<boolean>("mcpWarmSemanticOnStartup", false);
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
 *
 * When the stdio mode is ``"proxy"`` (the default gate-OFF path), the block is
 * rewritten to thin-proxy form so each host×repository connection costs a
 * model-free proxy rather than a heavy process (Requirements 2.8, 2.11;
 * preservation 3.8).
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
  const mode = resolveMcpStdioMode();
  const rewritten =
    mode === "proxy"
      ? rewriteServerBlockToThinProxy(block, binaryPath)
      : rewriteServerBlockToBinary(block, binaryPath);
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

/**
 * Resolve how editor-owned stdio mcpd blocks should launch.
 *
 * Default is ``"proxy"`` (thin stdio proxy → one heavy daemon per repository),
 * which is the gate-OFF topology path (Requirements 2.8, 2.11). Opt out via:
 * * setting ``cognis.mcpStdioMode`` = ``"heavy"``, or
 * * env ``COGNIS_MCP_STDIO_MODE=heavy``
 * to restore the legacy one-heavy-process-per-connection path (preservation
 * 3.8 escape hatch).
 */
export function resolveMcpStdioMode(): McpStdioMode {
  const env = (process.env.COGNIS_MCP_STDIO_MODE ?? "").trim().toLowerCase();
  if (env === "heavy" || env === "stdio" || env === "legacy") {
    return "heavy";
  }
  if (env === "proxy" || env === "thin" || env === "thin-proxy") {
    return "proxy";
  }
  try {
    const configured = vscode.workspace
      .getConfiguration("cognis")
      .get<string>("mcpStdioMode", "proxy");
    if (configured === "heavy") {
      return "heavy";
    }
  } catch {
    // Outside a VS Code host (unit tests) the default is proxy.
  }
  return "proxy";
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

/**
 * Remove every other Cognis-managed entry that resolves to the *same canonical
 * repository* (symlink/case-resolved root + `COGNIS_DB_PATH`) as `repoRoot`,
 * keeping only `keepServerName`. Deduping by canonical identity (rather than a
 * raw `path.resolve` comparison) makes a repeated enable through any alias —
 * `D:\Repo` vs `d:\repo`, or a symlink and its target — collapse onto the
 * single kept entry instead of leaving a duplicate heavy owner behind
 * (Requirements 2.3, 2.11). Distinct repositories keep distinct identities and
 * are never touched (preservation 3.6).
 */
function removeStaleCognisEntriesForRepo(
  servers: Record<string, unknown>,
  repoRoot: string,
  keepServerName: string
): void {
  const identity = canonicalRepoIdentity(repoRoot);
  dedupeCognisOwnersByIdentity(
    servers,
    identity,
    keepServerName,
    isCognisMcpServerName
  );
}

/**
 * Stable Cognis MCP server name for a repository, derived from its *canonical*
 * identity (symlink/case-resolved absolute root). Two aliases of one repo —
 * a symlink and its target, or `D:\Repo` vs `d:\repo` — always produce the
 * same key, so enable through either path never creates a second `cognis-*`
 * entry (Requirements 2.3, 2.11). Distinct repositories still get distinct
 * names (preservation 3.6).
 */
export function mcpServerNameForRepo(repoRoot: string): string {
  return deriveMcpServerName(canonicalRepoIdentity(repoRoot).root);
}

export async function fetchMcpConfig(
  repoRoot: string,
  host?: string
): Promise<McpConfigPayload> {
  const resolvedHost = host ?? resolveMcpHost();
  const serverName = mcpServerNameForRepo(repoRoot);
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
  return match.serverName === mcpServerNameForRepo(repoRoot);
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
  if (!match || match.serverName !== mcpServerNameForRepo(repoRoot)) {
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
 *
 * Gated by the reversible sharing gate (Requirement 2.9 / Property 10):
 * shared HTTP is only written when the flag is ON *and* every required gate
 * check has evidence of pass. A closed or failed gate refuses the write and
 * returns ``written: false`` so the caller can retain the compatible stdio
 * path with no data loss (preservation 3.8). Pass ``force: true`` only for
 * tests that intentionally bypass the gate.
 */
export function writeHttpMcpConfig(
  repoRoot: string,
  url: string,
  options?: { force?: boolean }
): {
  configPath: string;
  serverName: string;
  written: boolean;
  gate: SharingGateDecision;
} {
  const host = resolveMcpHost();
  const serverName = deriveMcpServerName(repoRoot);
  const configPath = getMcpConfigPath(host, repoRoot);
  const gate = resolveSharingGate(repoRoot);
  if (!options?.force && !gate.sharingEnabled) {
    // Fail-closed: do not rewrite mcp.json to a shared-HTTP URL while the
    // gate is closed. Existing stdio (thin-proxy / heavy) config is left
    // untouched — no data loss (preservation 3.8).
    return { configPath, serverName, written: false, gate };
  }
  const existing = readJsonFile(configPath);
  const servers =
    (existing.mcpServers as Record<string, unknown> | undefined) ?? {};
  removeStaleCognisEntriesForRepo(servers, repoRoot, serverName);
  servers[serverName] = buildHttpMcpServerBlock(url);
  existing.mcpServers = servers;
  writeJsonFile(configPath, existing);
  return { configPath, serverName, written: true, gate };
}

/**
 * True when the live sharing gate allows shared HTTP for ``repoRoot``.
 * Thin wrapper kept next to the config writers so call sites that already
 * import from ``mcpConfig`` do not need a second module. Fail-closed.
 */
export function canWriteSharedHttpConfig(repoRoot?: string): boolean {
  return isLiveSharedHttpAllowed(repoRoot);
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
 * This backs the "prepare for uninstall" cleanup: MCP config may live in a
 * shared global host file (when the user explicitly opts into
 * `cognis.mcpConfigScope = "global"`) and/or a workspace-local file. Removing
 * only the current workspace's entry would leave orphaned ``cognis-*`` servers
 * that the MCP host keeps trying to spawn after the extension is gone.
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
