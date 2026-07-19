import * as path from "path";

export type McpConfigScope = "workspace" | "global";

export function getGlobalMcpConfigPath(host: string, homeDir: string): string {
  switch (host) {
    case "cursor":
      return path.join(homeDir, ".cursor", "mcp.json");
    case "vscode":
      return path.join(homeDir, ".vscode", "mcp.json");
    case "kiro":
      // Kiro reads user-level MCP from ~/.kiro/settings/mcp.json.
      return path.join(homeDir, ".kiro", "settings", "mcp.json");
    case "claude":
      if (process.platform === "win32") {
        return path.join(
          process.env.APPDATA ?? homeDir,
          "Claude",
          "claude_desktop_config.json"
        );
      }
      if (process.platform === "darwin") {
        return path.join(
          homeDir,
          "Library",
          "Application Support",
          "Claude",
          "claude_desktop_config.json"
        );
      }
      return path.join(homeDir, ".config", "Claude", "claude_desktop_config.json");
    default:
      return path.join(homeDir, ".vscode", "mcp.json");
  }
}

export function getWorkspaceMcpConfigPath(
  repoRoot: string,
  host: string
): string | undefined {
  switch (host) {
    case "cursor":
      return path.join(repoRoot, ".cursor", "mcp.json");
    case "vscode":
      return path.join(repoRoot, ".vscode", "mcp.json");
    case "kiro":
      // Kiro reads workspace MCP from <repo>/.kiro/settings/mcp.json — the
      // right default for Cognis since each repo has its own COGNIS_DB_PATH.
      return path.join(repoRoot, ".kiro", "settings", "mcp.json");
    default:
      return undefined;
  }
}

/**
 * Resolve where Cognis should write/read MCP config.
 *
 * Workspace scope never silently falls back to the global host config: if the
 * host has no repository-local path (e.g. Claude Desktop), callers must pass
 * `scope: "global"` explicitly. Global scope always targets the host's user
 * config under `homeDir`.
 */
export function resolveMcpConfigPath(
  host: string,
  repoRoot: string | undefined,
  scope: McpConfigScope,
  homeDir: string
): string {
  if (scope === "global" || !repoRoot) {
    return getGlobalMcpConfigPath(host, homeDir);
  }
  // scope === "workspace" with a repo root: require a real workspace path.
  // Never fall back to global — that is what produced host × repository
  // fan-out when the setting defaulted to (or silently resolved as) global.
  const workspacePath = getWorkspaceMcpConfigPath(repoRoot, host);
  if (!workspacePath) {
    throw new Error(
      `MCP host "${host}" has no workspace-scoped config path; set cognis.mcpConfigScope to "global" to opt in to the shared user config`
    );
  }
  return workspacePath;
}
