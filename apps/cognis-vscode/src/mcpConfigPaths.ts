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

export function resolveMcpConfigPath(
  host: string,
  repoRoot: string | undefined,
  scope: McpConfigScope,
  homeDir: string
): string {
  if (repoRoot && scope === "workspace") {
    const workspacePath = getWorkspaceMcpConfigPath(repoRoot, host);
    if (workspacePath) {
      return workspacePath;
    }
  }
  return getGlobalMcpConfigPath(host, homeDir);
}
