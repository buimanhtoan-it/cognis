import * as path from "path";

const SERVER_PREFIX = "cognis";

/** Return a stable MCP server key such as ``cognis-my-app`` from *repoRoot*. */
export function deriveMcpServerName(
  repoRoot: string,
  prefix: string = SERVER_PREFIX
): string {
  const base = path.basename(path.resolve(repoRoot)).toLowerCase();
  const slug = base.replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return `${prefix}-${slug || "repo"}`;
}

export function isCognisMcpServerName(name: string): boolean {
  return name === "cognis" || name.startsWith("cognis-");
}
