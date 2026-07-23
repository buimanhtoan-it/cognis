/**
 * Named PanelContext fixtures covering every panel state the simulator drives.
 *
 * Each fixture is a real {@link PanelContext} the production `renderPanelHtml`
 * accepts, so the simulator renders the *actual* webview markup for that state
 * — no hand-written HTML. Add a state here and it is automatically built into a
 * standalone page and exercised by the Playwright spec.
 */
import type { PanelContext } from "../panel";
import {
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  compatibilitySnapshotFromHandshake,
} from "../compatibility";
import type { CompatibilitySnapshot } from "../compatibility";
import {
  evaluateHandshake,
  REQUIRED_CLI_COMMANDS,
  REQUIRED_MCP_TOOLS,
} from "../contract";
import type { ContractCompatibility, HandshakePayload } from "../contract";
import type { HealthReport, PrerequisiteReport } from "../types";

const VERSION = "0.4.0";

// The Extension build these compatibility fixtures pretend to run as. The
// mismatch fixtures drive a real `evaluateHandshake` against this expected
// version so the verdicts are faithful (not hand-forged) — the exact
// `0.8.10 -> 0.8.11` skew from Requirement 8.1.
const EXPECTED_ENGINE_VERSION = "0.8.11";

/** A complete, capability-full handshake payload; overrides tweak one field. */
function completeHandshake(
  overrides: Partial<HandshakePayload> = {}
): HandshakePayload {
  return {
    contract_version: 1,
    engine_version: EXPECTED_ENGINE_VERSION,
    cli_commands: [...REQUIRED_CLI_COMMANDS],
    mcp_tools: [...REQUIRED_MCP_TOOLS],
    ...overrides,
  };
}

/**
 * Build a committed Confirmed_Mismatch snapshot for one Compatibility_Kind by
 * running the real contract evaluator, so the sim renders the actual mismatch
 * verdict the production pipeline would commit.
 */
function confirmedMismatch(
  kind: Exclude<ContractCompatibility, "ok">
): CompatibilitySnapshot {
  let result;
  switch (kind) {
    case "engine-outdated":
      result = evaluateHandshake(
        completeHandshake({ engine_version: "0.8.10" }),
        EXPECTED_ENGINE_VERSION
      );
      break;
    case "engine-newer":
      result = evaluateHandshake(
        completeHandshake({ engine_version: "0.8.12" }),
        EXPECTED_ENGINE_VERSION
      );
      break;
    case "backend-older":
      result = evaluateHandshake(
        completeHandshake({ contract_version: 0 }),
        EXPECTED_ENGINE_VERSION
      );
      break;
    case "backend-newer":
      result = evaluateHandshake(
        completeHandshake({ contract_version: 2 }),
        EXPECTED_ENGINE_VERSION
      );
      break;
    case "capabilities-missing":
      result = evaluateHandshake(
        completeHandshake({ cli_commands: REQUIRED_CLI_COMMANDS.slice(0, -1) }),
        EXPECTED_ENGINE_VERSION
      );
      break;
    case "unreadable":
      result = evaluateHandshake(
        completeHandshake({ contract_version: undefined as unknown as number }),
        EXPECTED_ENGINE_VERSION
      );
      break;
  }
  if (result.compatibility !== kind) {
    throw new Error(
      `confirmedMismatch("${kind}") produced "${result.compatibility}"`
    );
  }
  return compatibilitySnapshotFromHandshake(result, 1, Date.now());
}

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

interface FixtureDefinition {
  name: string;
  title: string;
  context: Omit<PanelContext, "compatibility">;
  /**
   * Optional committed compatibility snapshot. When omitted the fixture is
   * promoted through the deterministic first-probe snapshot (compatible /
   * unprobed). Confirmed_Mismatch fixtures supply an explicit snapshot so the
   * panel derives the Compatibility_Primary_Action.
   */
  compatibility?: CompatibilitySnapshot;
}

const FIXTURE_DEFINITIONS: FixtureDefinition[] = [
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
  // The six baseline fixtures cover every state in both modes. One additional
  // minimal fixture locks the recovery edge where MCP is connected but live
  // sync stopped without a persisted pause:
  //   - off:     not provisioned, not indexing, not paused
  //   - running: configured, connected, and live indexing active/unknown
  //   - paused:  explicitly paused, or configured + connected + live sync off
  // Names are stable/greppable so the Playwright spec (task 8.2) can consume
  // them directly.
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
    name: "minimal-live-sync-off",
    title: "Minimal — live sync stopped without a persisted pause",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: false,
      configured: true,
      mcpEnabled: true,
      syncPaused: false,
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
  // ---------------------------------------------------------------------------
  // Compatibility (version-skew) coverage — Requirement 8.7/8.8, Playwright
  // task 6.1. Each Compatibility_Kind that maps to a distinct
  // Compatibility_Primary_Action gets a Minimal and an Advanced fixture:
  //   - engine-outdated → Update Engine   (installBackend)
  //   - engine-newer    → Update Extension (updateExtension)
  //   - unreadable      → Repair Engine    (reinstallEngine, modal)
  //
  // The base workspace state is healthy/connected/ready so the ONLY reason the
  // panel shows "Needs attention" + a remediation control is the committed
  // Confirmed_Mismatch — proving compatibility overrides a would-be "Ready".
  // Names are stable/greppable so the Playwright spec can consume them directly.
  // ---------------------------------------------------------------------------
  {
    name: "compat-engine-outdated-minimal",
    title: "Minimal — Engine outdated (Update Engine)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      configured: true,
      mcpEnabled: true,
      advancedMode: false,
      version: VERSION,
    },
    compatibility: confirmedMismatch("engine-outdated"),
  },
  {
    name: "compat-engine-outdated-advanced",
    title: "Advanced — Engine outdated (Update Engine)",
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
    compatibility: confirmedMismatch("engine-outdated"),
  },
  {
    name: "compat-engine-newer-minimal",
    title: "Minimal — Engine newer (Update Extension)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      configured: true,
      mcpEnabled: true,
      advancedMode: false,
      version: VERSION,
    },
    compatibility: confirmedMismatch("engine-newer"),
  },
  {
    name: "compat-engine-newer-advanced",
    title: "Advanced — Engine newer (Update Extension)",
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
    compatibility: confirmedMismatch("engine-newer"),
  },
  {
    name: "compat-unreadable-minimal",
    title: "Minimal — Engine unreadable (Repair Engine)",
    context: {
      status: "mcpEnabled",
      health: healthOk,
      liveIndexing: true,
      configured: true,
      mcpEnabled: true,
      advancedMode: false,
      version: VERSION,
    },
    compatibility: confirmedMismatch("unreadable"),
  },
  {
    name: "compat-unreadable-advanced",
    title: "Advanced — Engine unreadable (Repair Engine)",
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
    compatibility: confirmedMismatch("unreadable"),
  },
];

/**
 * Simulator states predate coordinator-backed probing, so every definition is
 * promoted through the explicit deterministic first-probe snapshot unless it
 * supplies its own committed Confirmed_Mismatch snapshot (the compatibility
 * fixtures).
 */
export const FIXTURES: NamedFixture[] = FIXTURE_DEFINITIONS.map((fixture) => ({
  name: fixture.name,
  title: fixture.title,
  context: {
    compatibility: fixture.compatibility ?? FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    ...fixture.context,
  },
}));
