import * as path from "path";

const SERVER_PREFIX = "cognis";

/**
 * Short, stable hash of the *full* resolved repo path (FNV-1a, 32-bit, 6 hex
 * chars). Disambiguates two repos that share a folder name (e.g. ``work/api``
 * and ``personal/api``) so their global MCP entries never collide.
 *
 * The extension owns this derivation and passes the resulting server name to
 * the engine via ``cognis-cli mcp-config --server-name``, so the key stays
 * stable for a given repo. We hash UTF-8 bytes of a normalized path (forward
 * slashes, lowercased) so Windows/POSIX and casing differences don't change the
 * result.
 */
export function shortPathHash(resolvedPath: string): string {
  const norm = resolvedPath.replace(/\\/g, "/").toLowerCase();
  const bytes = Buffer.from(norm, "utf8");
  let hash = 0x811c9dc5;
  for (const b of bytes) {
    hash ^= b;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0").slice(0, 6);
}

/**
 * Return a stable, collision-resistant MCP server key such as
 * ``cognis-my-app-3f9a2c`` from *repoRoot*. The slug stays human-readable; the
 * trailing hash guarantees uniqueness across repos with the same folder name so
 * multiple repos can be wired into one global MCP config at once.
 */
export function deriveMcpServerName(
  repoRoot: string,
  prefix: string = SERVER_PREFIX
): string {
  const resolved = path.resolve(repoRoot);
  // Extract the final path segment in a separator-agnostic way so the slug is
  // identical whether the path was written with / or \ , and on any OS. Plain
  // path.basename is platform-specific (it won't split a Windows-style path on
  // POSIX), which made the key differ between platforms. This mirrors the
  // normalization shortPathHash already does.
  const normalized = resolved.replace(/\\/g, "/").replace(/\/+$/, "");
  const base = (normalized.split("/").pop() ?? "").toLowerCase();
  const slug = base.replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return `${prefix}-${slug || "repo"}-${shortPathHash(resolved)}`;
}

export function isCognisMcpServerName(name: string): boolean {
  return name === "cognis" || name.startsWith("cognis-");
}
