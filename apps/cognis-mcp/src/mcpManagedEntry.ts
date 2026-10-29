/**
 * Marker-based `Managed_MCP_Entry` ownership (Requirement 3.10, 4.3, 21.7).
 *
 * Cognis writes two keys into an MCP server block's `env`:
 *
 *   COGNIS_MANAGED_BY        = "cognis-mcp"
 *   COGNIS_MANAGED_WORKSPACE = <RepoIdentity wire_key>
 *
 * Removal matches on the marker for the Canonical_Workspace. Entries written
 * by older versions have no marker, so the legacy `isCognisMcpServerName`
 * heuristic is the fallback. Every block without a Cognis-owned marker — and
 * every marker owned by a *different* workspace — is left untouched.
 *
 * This module is vscode-free so the generator suite and property tests can
 * import the production predicate without a host stub.
 */
import { isCognisMcpServerName } from "./mcpServerName";

/** Design: `"COGNIS_MANAGED_BY": "cognis-mcp"`. */
export const COGNIS_MANAGED_BY = "COGNIS_MANAGED_BY";

/** Design: `"COGNIS_MANAGED_WORKSPACE": "<RepoIdentity wire_key>"`. */
export const COGNIS_MANAGED_WORKSPACE = "COGNIS_MANAGED_WORKSPACE";

/** The value Cognis writes into {@link COGNIS_MANAGED_BY}. */
export const COGNIS_MANAGED_BY_VALUE = "cognis-mcp";

function blockEnv(block: unknown): Record<string, string> | undefined {
  if (!block || typeof block !== "object") {
    return undefined;
  }
  const env = (block as { env?: unknown }).env;
  if (!env || typeof env !== "object") {
    return undefined;
  }
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(env as Record<string, unknown>)) {
    if (typeof value === "string") {
      out[key] = value;
    }
  }
  return out;
}

/** True when the block carries Cognis's ownership marker, for any workspace. */
export function isCognisOwnedMarker(block: unknown): boolean {
  const env = blockEnv(block);
  return env?.[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE;
}

/**
 * True when the block is Cognis-owned *for this Canonical_Workspace*.
 * Independent of the server name — the marker is the source of truth.
 */
export function isManagedForWorkspace(
  block: unknown,
  workspaceKey: string
): boolean {
  const env = blockEnv(block);
  if (!env) {
    return false;
  }
  return (
    env[COGNIS_MANAGED_BY] === COGNIS_MANAGED_BY_VALUE &&
    env[COGNIS_MANAGED_WORKSPACE] === workspaceKey
  );
}

/**
 * Per-workspace removal predicate (Requirement 4.3):
 *
 * - marked for this workspace → remove
 * - marked for another workspace → keep
 * - unmarked + legacy Cognis name heuristic → remove
 * - otherwise → keep
 */
export function isRemovableManagedMcpEntry(
  name: string,
  block: unknown,
  workspaceKey: string
): boolean {
  if (isManagedForWorkspace(block, workspaceKey)) {
    return true;
  }
  if (isCognisOwnedMarker(block)) {
    return false;
  }
  return isCognisMcpServerName(name);
}

/**
 * Uninstall-all predicate: every Cognis-owned marker (any workspace) plus
 * unmarked legacy Cognis names. Foreign unmarked entries stay.
 */
export function isCognisOwnedOrLegacyEntry(
  name: string,
  block: unknown
): boolean {
  if (isCognisOwnedMarker(block)) {
    return true;
  }
  return isCognisMcpServerName(name);
}

/** Stamp the Cognis-owned marker onto an env block without dropping existing keys. */
export function stampManagedMcpEnv(
  env: Record<string, string> | undefined,
  workspaceKey: string
): Record<string, string> {
  return {
    ...(env ?? {}),
    [COGNIS_MANAGED_BY]: COGNIS_MANAGED_BY_VALUE,
    [COGNIS_MANAGED_WORKSPACE]: workspaceKey,
  };
}

/** Stamp the marker onto a server block, creating `env` when the block had none. */
export function stampManagedMcpEntry<T extends object>(
  block: T,
  workspaceKey: string
): T & { env: Record<string, string> } {
  const existing =
    "env" in block &&
    typeof (block as { env?: unknown }).env === "object" &&
    (block as { env?: unknown }).env !== null
      ? ((block as { env: Record<string, string> }).env)
      : undefined;
  return { ...block, env: stampManagedMcpEnv(existing, workspaceKey) };
}

/**
 * Delete removable Cognis entries from `servers` in place.
 *
 * When `workspaceKey` is set, only this workspace's managed entries (plus
 * unmarked legacy names) are removed. When omitted, every Cognis-owned or
 * legacy-named entry is removed (prepare-for-uninstall).
 *
 * Entries that are not selected are not rewritten — their object identity and
 * key order stay so a later `JSON.stringify` keeps them byte-identical.
 */
export function filterOutManagedMcpEntries(
  servers: Record<string, unknown>,
  workspaceKey?: string
): string[] {
  const removed: string[] = [];
  for (const name of Object.keys(servers)) {
    const block = servers[name];
    const drop = workspaceKey
      ? isRemovableManagedMcpEntry(name, block, workspaceKey)
      : isCognisOwnedOrLegacyEntry(name, block);
    if (!drop) {
      continue;
    }
    delete servers[name];
    removed.push(name);
  }
  return removed;
}
