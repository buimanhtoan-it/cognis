/** JSON from `cognis-cli health --json`. */
export interface HealthReport {
  runtime_version: string;
  checks: Record<string, { status: string; message: string }>;
  overall: "ok" | "warn" | "fail";
}

/** JSON from `cognis-cli paths`. */
export interface WorkspacePaths {
  repo_root: string;
  cognis_dir: string;
  config_path: string;
  db_path: string;
  indexd_status_path: string;
  audit_log_path: string;
  capsule_cache_dir: string;
  golden_set_path: string;
  runtime_version: string;
  commands: {
    python: string;
    cognis_cli: string | null;
    cognis_mcpd: string | null;
    cognis_indexd: string | null;
    cognis_cli_module: string;
    cognis_mcpd_module: string;
    cognis_indexd_module: string;
  };
}

/** JSON from `cognis-cli mcp-config --json`. */
export interface McpConfigPayload {
  host: string;
  format: string;
  repo_root: string;
  server_name: string;
  config: { mcpServers: Record<string, McpServerBlock> };
  config_paths: Record<string, string>;
  env: Record<string, string>;
}

export interface McpServerBlock {
  command: string;
  args?: string[];
  env: Record<string, string>;
}

/** JSON from `cognis-cli bootstrap --json`. */
export interface BootstrapPayload {
  command: string;
  runtime_version: string;
  repo_root: string;
  index_path: string;
  db_path: string;
  skip_embeddings: boolean;
  paths: WorkspacePaths;
  phases: Array<{ name: string; status: string }>;
  health: HealthReport;
  overall: string;
  exit_code: number;
}

export interface IndexStatusReport {
  pid?: number;
  active: boolean;
  phase: string;
  message: string;
  progressPercent?: number;
  pendingCount: number;
  pendingFiles: string[];
  inflightCount: number;
  inflightFiles: string[];
  recentFiles: string[];
  updatedAt: number;
  lastError?: string;
}

export type WorkspaceStatus =
  | "notInstalled"
  | "indexing"
  | "ready"
  | "mcpEnabled"
  | "degraded"
  | "unknown";

/** Planned repair steps for a degraded workspace. */
export interface RepairPlan {
  needsBootstrap: boolean;
  needsReindex: boolean;
  needsMcp: boolean;
  needsLiveIndexing: boolean;
  health?: HealthReport;
}

/** Outcome of the end-to-end Set Up for AI flow. */
export interface SetupResult {
  bootstrap: BootstrapPayload;
  mcpConfigPath?: string;
  mcpError?: string;
  liveIndexingStarted: boolean;
  liveIndexingError?: string;
  health: HealthReport;
  indexingInBackground?: boolean;
}
