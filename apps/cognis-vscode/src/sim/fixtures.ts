/**
 * Named PanelContext fixtures covering every panel state the simulator drives.
 *
 * Each fixture is a real {@link PanelContext} the production `renderPanelHtml`
 * accepts, so the simulator renders the *actual* webview markup for that state
 * — no hand-written HTML. Add a state here and it is automatically built into a
 * standalone page and exercised by the Playwright spec.
 */
import type { PanelContext } from "../panel";
import type { HealthReport, PrerequisiteReport } from "../types";

const VERSION = "0.4.0";

const healthOk: HealthReport = {
  runtime_version: VERSION,
  overall: "ok",
  checks: {
    config: { status: "ok", message: "ok" },
    db: { status: "ok", message: "ok" },
    index: { status: "ok", message: "ok" },
    vector: { status: "ok", message: "ok" },
    embedder: { status: "ok", message: "ok" },
    version: { status: "ok", message: "ok" },
  },
};

const healthDegraded: HealthReport = {
  runtime_version: VERSION,
  overall: "fail",
  checks: {
    ...healthOk.checks,
    index: { status: "fail", message: "Index is corrupt; rebuild required." },
  },
};

const prereqsMissing: PrerequisiteReport = {
  ready: false,
  combined_install_target: "",
  items: [
    {
      id: "engine",
      label: "Cognis engine",
      description: "The single self-contained cognis binary.",
      status: "missing",
      required: true,
      install_target: "",
      detail: "Not installed.",
    },
    {
      id: "semantic_index",
      label: "Semantic index",
      description: "Symbol embeddings for semantic search.",
      status: "missing",
      required: false,
      install_target: "",
      detail: "Not installed.",
    },
  ],
};

export interface NamedFixture {
  name: string;
  title: string;
  context: PanelContext;
}

export const FIXTURES: NamedFixture[] = [
  {
    name: "fresh-machine",
    title: "Fresh machine — backend not installed",
    context: {
      status: "notInstalled",
      backendAvailable: false,
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "prerequisites-missing",
    title: "Prerequisites missing",
    context: {
      status: "notInstalled",
      backendAvailable: true,
      prerequisites: prereqsMissing,
      version: VERSION,
    },
  },
  {
    name: "setup-required",
    title: "Backend ready — setup required",
    context: { status: "notInstalled", backendAvailable: true, version: VERSION },
  },
  {
    name: "indexing",
    title: "Building the index",
    context: {
      status: "indexing",
      version: VERSION,
      indexStatus: {
        active: true,
        phase: "cold_index",
        message: "Indexing your codebase…",
        progressPercent: 42,
        pendingCount: 3,
        pendingFiles: ["src/a.ts", "src/b.ts", "src/c.ts"],
        inflightCount: 1,
        inflightFiles: ["src/main.ts"],
        recentFiles: ["src/done.ts"],
        updatedAt: Date.now(),
      },
    },
  },
  {
    name: "embedding-backfill",
    title: "Embedding backfill in progress (configured)",
    context: {
      status: "indexing",
      configured: true,
      version: VERSION,
      indexStatus: {
        active: true,
        phase: "embedding",
        message:
          "Generating semantic embeddings… 120/240 symbols (search already works)",
        progressPercent: 85,
        pendingCount: 0,
        pendingFiles: [],
        inflightCount: 0,
        inflightFiles: [],
        recentFiles: ["src/auth.ts"],
        updatedAt: Date.now(),
      },
    },
  },
  {
    name: "transient-health-gap",
    title: "Configured — transient health gap (must not regress to setup)",
    context: { status: "unknown", configured: true, version: VERSION },
  },
  {
    name: "mcp-http-stopped",
    title: "MCP connected; HTTP server stopped (panel can start)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      configured: true,
      mcpEnabled: true,
      liveIndexing: true,
      mcpHost: "vscode",
      mcpServerPhase: "stopped",
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "mcp-http-running",
    title: "MCP HTTP server running (URL visible, can stop)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      configured: true,
      mcpEnabled: true,
      liveIndexing: true,
      mcpHost: "vscode",
      mcpServerPhase: "running",
      mcpServerUrl: "http://127.0.0.1:50001/mcp",
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "ready-not-connected",
    title: "Index ready — connect MCP",
    context: {
      status: "ready",
      health: healthOk,
      liveIndexing: true,
      mcpEnabled: false,
      configured: true,
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "ready-connected",
    title: "Semantic search ready (fully connected)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      mcpEnabled: true,
      configured: true,
      version: VERSION,
    },
  },
  {
    name: "degraded",
    title: "Degraded — needs repair",
    context: {
      status: "degraded",
      health: healthDegraded,
      mcpEnabled: true,
      liveIndexing: false,
      configured: true,
      version: VERSION,
    },
  },
  {
    name: "sync-paused",
    title: "Index sync paused",
    context: {
      status: "ready",
      health: healthOk,
      liveIndexing: false,
      mcpEnabled: true,
      configured: true,
      syncPaused: true,
      version: VERSION,
    },
  },
  {
    name: "mcp-connected-disconnectable",
    title: "MCP connected — can disconnect (Disconnect MCP button)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      mcpEnabled: true,
      configured: true,
      mcpHost: "vscode",
      mcpServerName: "cognis",
      mcpConfigPath: ".vscode/mcp.json",
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "indexing-cancelable",
    title: "Indexing busy — can cancel (Cancel indexing button)",
    context: {
      status: "ready",
      health: healthOk,
      liveIndexing: true,
      mcpEnabled: true,
      configured: true,
      advancedMode: true,
      version: VERSION,
      indexStatus: {
        active: true,
        phase: "rebuild",
        message: "Rebuilding the index…",
        progressPercent: 30,
        pendingCount: 5,
        pendingFiles: ["src/a.ts", "src/b.ts", "src/c.ts", "src/d.ts", "src/e.ts"],
        inflightCount: 2,
        inflightFiles: ["src/main.ts", "src/panel.ts"],
        recentFiles: ["src/done.ts"],
        updatedAt: Date.now(),
      },
    },
  },
  // ---------------------------------------------------------------------------
  // Minimal_Surface / Advanced_Surface coverage across every Cognis_State.
  //
  // Six fixtures below give one advancedMode-OFF and one advancedMode-ON
  // fixture for each Cognis_State derived by `deriveCognisState`:
  //   - off:     not provisioned, not indexing, not paused
  //   - running: configured && mcpEnabled && !syncPaused (provisioned)
  //   - paused:  syncPaused === true
  // Names are stable/greppable so the Playwright spec (task 8.2) can consume
  // them directly: minimal-{off,running,paused} and advanced-{off,running,paused}.
  // ---------------------------------------------------------------------------
  {
    name: "minimal-off",
    title: "Minimal — Cognis off (setup required)",
    context: {
      status: "notInstalled",
      backendAvailable: true,
      configured: false,
      mcpEnabled: false,
      advancedMode: false,
      version: VERSION,
    },
  },
  {
    name: "minimal-running",
    title: "Minimal — Cognis running (provisioned + connected)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      configured: true,
      mcpEnabled: true,
      advancedMode: false,
      version: VERSION,
    },
  },
  {
    name: "minimal-paused",
    title: "Minimal — sync paused",
    context: {
      status: "ready",
      health: healthOk,
      liveIndexing: false,
      configured: true,
      mcpEnabled: true,
      syncPaused: true,
      advancedMode: false,
      version: VERSION,
    },
  },
  {
    name: "advanced-off",
    title: "Advanced — Cognis off (setup required)",
    context: {
      status: "notInstalled",
      backendAvailable: true,
      configured: false,
      mcpEnabled: false,
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "advanced-running",
    title: "Advanced — Cognis running (provisioned + connected)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      configured: true,
      mcpEnabled: true,
      mcpHost: "vscode",
      mcpServerName: "cognis",
      mcpConfigPath: ".vscode/mcp.json",
      advancedMode: true,
      version: VERSION,
    },
  },
  {
    name: "advanced-paused",
    title: "Advanced — sync paused",
    context: {
      status: "ready",
      health: healthOk,
      liveIndexing: false,
      configured: true,
      mcpEnabled: true,
      syncPaused: true,
      advancedMode: true,
      version: VERSION,
    },
  },
];
