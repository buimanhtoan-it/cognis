import * as path from "path";

const SERVER_PREFIX = "cognis";

/**
 * Short, stable hash of the *full* resolved repo path (FNV-1a, 32-bit, 6 hex
 * chars). Disambiguates two repos that share a folder name (e.g. ``work/api``
 * and ``personal/api``) so their global MCP entries never collide.
 *
 * IMPORTANT: this must stay byte-for-byte identical to ``_short_path_hash`` in
 * ``packages/core/cognis/cli/main.py`` so the extension and the CLI derive the
 * same server key for a given repo. We hash UTF-8 bytes of a normalized path
 * (forward slashes, lowercased) so Windows/POSIX and casing differences don't
 * change the result.
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
  const base = path.basename(resolved).toLowerCase();
  const slug = base.replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return `${prefix}-${slug || "repo"}-${shortPathHash(resolved)}`;
}

export function isCognisMcpServerName(name: string): boolean {
  return name === "cognis" || name.startsWith("cognis-");
}
