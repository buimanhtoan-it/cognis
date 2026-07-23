import * as vscode from "vscode";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "./types";
import type { HandshakeResult } from "./contract";
import type { CompatibilitySnapshot } from "./compatibility";
import {
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  deriveRemediation,
  isConfirmedMismatch,
} from "./compatibility";
import { isIndexStatusBusy } from "./state";

/**
 * Maps webview `data-action` ids to the VS Code commands they invoke. Exported
 * so the panel simulator/Playwright harness can assert that every clickable
 * button posts an action that resolves to a real command (the UI contract).
 */
export const ACTION_COMMANDS: Record<string, string> = {
  startCognis: "cognis.startCognis",
  setup: "cognis.setupWorkspace",
  repair: "cognis.repairSetup",
  clearReindex: "cognis.clearAndReindex",
  connectMcp: "cognis.connectMcp",
  disconnectMcp: "cognis.disconnectMcp",
  startMcp: "cognis.startMcpServer",
  stopMcp: "cognis.stopMcpServer",
  pauseSync: "cognis.pauseSync",
  resumeSync: "cognis.resumeSync",
  cancelIndexing: "cognis.cancelIndexing",
  health: "cognis.showHealth",
  output: "cognis.showOutput",
  refreshPrerequisites: "cognis.refreshPrerequisites",
  installAllPrerequisites: "cognis.installAllPrerequisites",
  installBackend: "cognis.installBackend",
  reinstallEngine: "cognis.reinstallEngine",
  updateExtension: "cognis.updateExtension",
  coldRestart: "cognis.coldRestart",
  remove: "cognis.removeFromWorkspace",
  prepareUninstall: "cognis.prepareUninstall",
  forceCleanup: "cognis.forceCleanup",
};

// Canonical display label for the `cognis.resumeSync` action. Every rendered
// location that resolves to `resumeSync` MUST use this constant so the label is
// byte-for-byte identical across all panel states (R8.1–R8.3).
const RESUME_SYNC_LABEL = "Resume sync";

export interface PanelContext {
  status: WorkspaceStatus;
  /** Compatibility verdict committed for the same workspace snapshot. */
  compatibility: CompatibilitySnapshot;
  health?: HealthReport;
  liveIndexing?: boolean;
  mcpEnabled?: boolean;
  indexStatus?: IndexStatusReport;
  indexingMessage?: string;
  /** Installable-prerequisite checklist (from `cognis-cli doctor`). */
  prerequisites?: PrerequisiteReport;
  /** True once the workspace has a `.cognis/config.yaml` (setup has run). */
  configured?: boolean;
  /** Resolved Cognis MCP server name for this workspace (e.g. `cognis-<slug>-<hash>`). */
  mcpServerName?: string;
  /** Target MCP host: "cursor" | "vscode" | "claude". */
  mcpHost?: string;
  /** Path to the workspace MCP config (mcp.json) Cognis writes for the host. */
  mcpConfigPath?: string;
  /** Live stdio MCP server processes the editor has spawned (``cognis_mcpd``). */
  mcpRuntimeCount?: number;
  /** True when ``mcpRuntimeCount`` is verified to belong to this repo (env-scoped). */
  mcpRuntimeRepoScoped?: boolean;
  /** Phase of the panel-managed standalone HTTP MCP server, when running. */
  mcpServerPhase?: "stopped" | "starting" | "running" | "error";
  /** URL of the panel-managed HTTP MCP server (``http://host:port/mcp``). */
  mcpServerUrl?: string;
  /** Last error message from the panel-managed MCP server, if any. */
  mcpServerError?: string;
  /**
   * Whether the engine binary (cognis CLI) could actually run. Undefined until
   * probed. False means the backend isn't installed/reachable yet — on a fresh
   * machine this is the first thing to fix, before any setup can succeed.
   */
  backendAvailable?: boolean;
  /** True when the user has explicitly paused index sync for this workspace. */
  syncPaused?: boolean;
  /** Extension version string, rendered in the panel header (e.g. "0.4.0"). */
  version?: string;
  /**
   * Advanced/Debug mode. Mirrors the `cognis.advancedMode` setting. When on,
   * the panel reveals the full detailed control surface; when off (the
   * default), it renders the minimal surface. Optional so existing
   * callers/fixtures keep working — a missing value is treated as `false`.
   */
  advancedMode?: boolean;
}

/**
 * Unified user-facing state the single primary control represents. Collapses
 * the separate indexing + MCP concepts into one on/off/paused model:
 *  - `off`     : Cognis is not set up / not running for this workspace.
 *  - `running` : Cognis is on and actively syncing.
 *  - `paused`  : Cognis is on but index sync is paused.
 */
export type CognisState = "off" | "running" | "paused";

/**
 * data-action ids the {@link UnifiedControl} can carry. Two disjoint groups:
 *  - Operational_Primary_Action: `startCognis` | `pauseSync` | `resumeSync`
 *    (the Start/Pause/Resume model from `extension-minimal-panel`).
 *  - Compatibility_Primary_Action: `installBackend` | `updateExtension` |
 *    `reinstallEngine` — the three permitted, non-Cold-Restart remediation
 *    commands a Confirmed_Mismatch temporarily promotes to *the* one control
 *    (Requirement 3.4–3.7). Every id resolves through {@link ACTION_COMMANDS}.
 */
export type UnifiedControlId =
  | "startCognis"
  | "pauseSync"
  | "resumeSync"
  | "installBackend"
  | "updateExtension"
  | "reinstallEngine";

/**
 * The unified primary control. Its `data-action` id always maps through
 * {@link ACTION_COMMANDS} to a registered command. When there is no actionable
 * Confirmed_Mismatch the id is an Operational_Primary_Action
 * (`startCognis` | `pauseSync` | `resumeSync`); when a mismatch needs the user
 * it becomes the Compatibility_Primary_Action (`installBackend` |
 * `updateExtension` | `reinstallEngine`). It is never a Cold Restart /
 * rebuild / remove action.
 */
export interface UnifiedControl {
  /** data-action id; always resolves through ACTION_COMMANDS. */
  id: UnifiedControlId;
  /** e.g. "Start Cognis" | "Pause" | "Resume" | "Update Engine". */
  label: string;
}

/**
 * Render the single unified primary control as exactly one `<button>` carrying
 * the stable `data-unified="true"` marker (the invariant that identifies *the*
 * one primary control, R1.1) plus a `data-action` that resolves through
 * {@link ACTION_COMMANDS} to a Non_Destructive command (R1.2–R1.4). The label
 * is HTML-escaped like every other rendered label; the button reuses the shared
 * `primary` button style. Exactly one element in the returned string carries
 * `data-unified`.
 */
export function renderUnifiedControl(unified: UnifiedControl): string {
  return `<button class="primary" data-unified="true" data-action="${escapeHtml(
    unified.id
  )}">${escapeHtml(unified.label)}</button>`;
}

/**
 * Closed vocabulary for the minimal-surface status line. Never contains raw
 * technical values.
 */
export type StatusLineText =
  | "Ready"
  | "Working"
  | "Paused"
  | "Off"
  | "Needs attention";

/**
 * Collapse the separate indexing + MCP concepts into the single on/off/paused
 * model the unified primary control represents. The three states are mutually
 * exclusive and checked in order:
 *  1. `paused`  — the user explicitly paused sync, or a configured and
 *     connected workspace reports that live indexing is stopped.
 *  2. `running` — indexing work is in flight, or Cognis is configured,
 *     connected, and live indexing is not known to be stopped.
 *  3. `off`     — anything else (fresh machine, not set up, MCP not connected).
 */
export function deriveCognisState(ctx: PanelContext): CognisState {
  if (ctx.syncPaused === true) {
    return "paused";
  }
  const activelyIndexing =
    ctx.status === "indexing" || isIndexStatusBusy(ctx.indexStatus);
  if (activelyIndexing) {
    return "running";
  }
  const configuredAndConnected = Boolean(ctx.configured && ctx.mcpEnabled);
  if (configuredAndConnected && ctx.liveIndexing === false) {
    return "paused";
  }
  if (configuredAndConnected) {
    return "running";
  }
  return "off";
}

/**
 * Derive the single unified primary control from the current context.
 *
 * An actionable Confirmed_Mismatch takes priority: when the committed
 * compatibility snapshot is a confirmed non-`ok` verdict *and* it maps to a
 * remediation (via {@link deriveRemediation}), the control becomes the
 * Compatibility_Primary_Action — exactly one control whose `data-action`
 * resolves to the remediation command (`installBackend` | `updateExtension` |
 * `reinstallEngine`), replacing Start/Pause/Resume until the mismatch is
 * resolved (Requirement 3.3–3.7). It is never a Cold Restart / rebuild /
 * remove action.
 *
 * Otherwise the mapping is the Operational_Primary_Action fixed on
 * {@link deriveCognisState}:
 *  - `off`     → `{ id: "startCognis", label: "Start Cognis" }`
 *  - `running` → `{ id: "pauseSync",   label: "Pause" }`
 *  - `paused`  → `{ id: "resumeSync",  label: "Resume" }`
 *
 * so the id stays within the Non_Destructive Operational set and never
 * resolves to a Destructive_Action.
 */
export function deriveUnifiedControl(ctx: PanelContext): UnifiedControl {
  // Compatibility_Primary_Action overrides Start/Pause/Resume whenever a
  // confirmed mismatch has an actionable remediation (Requirement 3.3).
  if (isConfirmedMismatch(ctx.compatibility)) {
    const remediation = deriveRemediation(ctx.compatibility.result);
    if (remediation) {
      return { id: remediation.actionId, label: remediation.label };
    }
  }
  switch (deriveCognisState(ctx)) {
    case "running":
      return { id: "pauseSync", label: "Pause" };
    case "paused":
      return { id: "resumeSync", label: "Resume" };
    case "off":
    default:
      return { id: "startCognis", label: "Start Cognis" };
  }
}

/**
 * True when the workspace's ONLY health problem is a degraded (rebuilding)
 * semantic/vector layer — the core lexical + structural index is healthy and
 * search works right now, so this is not a dead-end the user must act on.
 *
 * Precisely: health is present, `overall !== "ok"`, the core checks `config`,
 * `db` and `index` are all `ok`, and the `vector` check is the *only* non-ok
 * check and is specifically `warn` (rebuilding), not `fail`.
 *
 * Tolerant of absent checks — a missing `config`/`db`/`vector` (or any other
 * optional check) is treated as `ok` — but `index` MUST be present and `ok`,
 * since it is the core the tool depends on. Any other non-ok check (e.g.
 * `db`/`version` fail) makes this false, so genuine problems still surface as
 * "Needs attention".
 */
export function isSemanticOnlyDegraded(ctx: PanelContext): boolean {
  const health = ctx.health;
  if (!health || health.overall === "ok") {
    return false;
  }
  const checks = health.checks ?? {};

  // The core index MUST be present and healthy (absent index is NOT tolerated).
  const index = checks.index;
  if (!index || index.status !== "ok") {
    return false;
  }
  // The vector layer must be the degraded one, and only *warn* (rebuilding) —
  // a hard `fail` is a genuine problem, not a background rebuild.
  const vector = checks.vector;
  if (!vector || vector.status !== "warn") {
    return false;
  }
  // Every other present check must be ok — vector is the ONLY non-ok check.
  // Absent checks are tolerated (treated as ok) by simply not appearing here.
  for (const [name, check] of Object.entries(checks)) {
    if (name === "vector") {
      continue;
    }
    if (check.status !== "ok") {
      return false;
    }
  }
  return true;
}

/**
 * Derive the single minimal-surface status line. Reuses the same internal
 * verdict logic as {@link derivePanelView} (via its `statusClass`) but collapses
 * it to the closed vocabulary {@link StatusLineText}, checked in order:
 *  - `Working`         — indexing work is in flight (or setup is finishing).
 *  - `Paused`          — the user has explicitly paused index sync.
 *  - `Needs attention` — the view reports a warning (`status-warn`), or a
 *    setup-required / unknown state that needs the user to act.
 *  - `Ready`           — the index is healthy and connected (`status-ok`).
 *
 * The returned value is always one of the four closed-vocabulary strings and
 * never embeds a raw technical value from the context.
 */
export function deriveStatusLine(ctx: PanelContext): StatusLineText {
  const view = derivePanelView(ctx);

  // Active indexing work (and the transient "finishing setup" phase, which
  // derivePanelView also reports as `status-active`) reads as "Working" — this
  // takes priority over the sync/health verdicts below, mirroring
  // derivePanelView which returns `status-active` while work is in flight.
  if (
    ctx.status === "indexing" ||
    isIndexStatusBusy(ctx.indexStatus) ||
    view.statusClass === "status-active"
  ) {
    return "Working";
  }
  // A Confirmed_Mismatch the user must act on takes priority over every idle
  // verdict below: it overrides both "Ready" and the semantic-only-degraded
  // case, so a version-skewed workspace never reads as fully ready (R1.4,
  // R1.5). Busy indexing above still shows "Working" first.
  if (isConfirmedMismatch(ctx.compatibility)) {
    return "Needs attention";
  }
  // The user explicitly paused index sync.
  if (ctx.syncPaused === true) {
    return "Paused";
  }
  // Not started / not set up for this workspace yet. This is a normal resting
  // state (the Unified_Control shows "Start Cognis"), NOT a problem — so it
  // reads as "Off" rather than the alarming "Needs attention".
  if (deriveCognisState(ctx) === "off") {
    return "Off";
  }
  // Healthy index, connected, idle.
  if (view.statusClass === "status-ok") {
    return "Ready";
  }
  // The tool IS usable — only the semantic/vector layer is rebuilding (lexical
  // + structural search work now). Don't dead-end the user with "Needs
  // attention"; read as "Ready" (the tailored hint explains semantic is still
  // building). Only genuine config/db/index problems fall through below.
  if (isSemanticOnlyDegraded(ctx)) {
    return "Ready";
  }
  // A workspace that IS provisioned but has a health/prerequisite/version
  // problem — the only case that genuinely needs the user to look.
  return "Needs attention";
}

/**
 * A short, plain-language caption that tells the user what the current status
 * means and what to do next. Paired with the {@link StatusLineText} word so the
 * Minimal_Surface is self-explanatory (a single word like "Off" or "Needs
 * attention" is not actionable on its own). Constant per state — never embeds a
 * raw technical value.
 */
export function deriveStatusHint(ctx: PanelContext): string {
  const line = deriveStatusLine(ctx);
  // The status word is "Ready" but the semantic layer is still building — give
  // a tailored caption instead of the generic "up to date" one, so the user
  // knows lexical + structural search already work.
  if (line === "Ready" && isSemanticOnlyDegraded(ctx)) {
    return "Ready — semantic search is still building in the background; lexical and structural search work now.";
  }
  // A Confirmed_Mismatch reads as "Needs attention" (R1.4). Instead of the
  // generic "turn on Advanced mode" caption, name whether the Engine or the
  // Extension needs updating and how to proceed, matched 1:1 to the
  // Compatibility_Primary_Action (R6.4). The caption is drawn from a closed,
  // plain-language vocabulary and never leaks a raw version/URL/id/error — the
  // raw versions live only in the labeled Advanced_Surface detail (R6.2, R6.3,
  // Correctness Property 3).
  if (line === "Needs attention" && isConfirmedMismatch(ctx.compatibility)) {
    const hint = deriveCompatibilityHint(ctx.compatibility.result);
    if (hint) {
      return hint;
    }
  }
  switch (line) {
    case "Off":
      return "Not running yet. Click Start Cognis to set up and index this workspace.";
    case "Working":
      return "Indexing your workspace — this runs in the background, no need to wait.";
    case "Paused":
      return "Index sync is paused. Click Resume to keep the index up to date.";
    case "Ready":
      return "Running and up to date. Your editor’s AI can search this workspace.";
    case "Needs attention":
      return "Something needs a look. Turn on Advanced mode (setting: cognis.advancedMode) to see details.";
  }
}

/**
 * The plain-language "Needs attention" caption for a confirmed compatibility
 * mismatch, chosen 1:1 by the remediation the mismatch maps to (R6.4). Each
 * caption names whether the Engine or the Extension needs updating and how to
 * proceed, using only the user vocabulary "Engine"/"Extension".
 *
 * The captions are a closed vocabulary and deliberately carry NO raw technical
 * value — no raw version numbers, URLs, server ids, or verbatim error strings
 * (R6.2, Correctness Property 3). The raw Engine/Extension versions appear only
 * in the labeled Advanced_Surface detail rendered by
 * {@link renderCompatibilityDetail} (R6.3). Returns `undefined` for a result
 * with no remediation (e.g. `ok`), so the caller falls back to the generic
 * caption.
 */
export function deriveCompatibilityHint(
  result: HandshakeResult
): string | undefined {
  const remediation = deriveRemediation(result);
  if (!remediation) {
    return undefined;
  }
  switch (remediation.actionId) {
    case "installBackend":
      return "The Engine needs updating to match the Extension. Click Update Engine to continue.";
    case "updateExtension":
      return "The Extension needs updating to match the Engine. Click Update Extension to continue.";
    case "reinstallEngine":
      return "The Engine could not be read and needs repair. Click Repair Engine to continue.";
  }
}

/**
 * Render the single minimal-surface status line as one `<div>` carrying the
 * stable `data-status-line="true"` marker (the invariant that identifies *the*
 * one status line, R2.7). The element contains exactly the fixed-vocabulary
 * {@link StatusLineText} string (HTML-escaped like every other rendered label),
 * and never embeds a raw technical value (R2.2). Exactly one element in the
 * returned string carries `data-status-line`.
 */
export function renderStatusLine(text: StatusLineText, hint?: string): string {
  // Tone drives a small colored dot so the status reads at a glance:
  // ok=green, working=busy blue, attention=amber, off/paused=muted.
  const tone =
    text === "Ready"
      ? "status-ok"
      : text === "Working"
        ? "status-active"
        : text === "Needs attention"
          ? "status-warn"
          : "status-muted";
  const hintHtml = hint
    ? `<div class="status-hint">${escapeHtml(hint)}</div>`
    : "";
  return `<div class="status-line ${tone}" data-status-line="true">
    <div class="status-word"><span class="status-dot"></span>${escapeHtml(text)}</div>
    ${hintHtml}
  </div>`;
}

export interface PanelPrimaryAction {
  id: string;
  label: string;
  disabled?: boolean;
}

export interface PanelView {
  headline: string;
  detail?: string;
  statusClass: string;
  primary?: PanelPrimaryAction;
}

const STATUS_CLASSES: Record<WorkspaceStatus, string> = {
  notInstalled: "status-muted",
  indexing: "status-active",
  ready: "status-ok",
  mcpEnabled: "status-ok",
  degraded: "status-warn",
  unknown: "status-muted",
};

function firstFailingCheck(
  health: HealthReport
): { name: string; message: string } | undefined {
  for (const [name, check] of Object.entries(health.checks)) {
    if (check.status === "fail") {
      return { name, message: check.message };
    }
  }
  return undefined;
}

function firstWarningCheck(
  health: HealthReport
): { name: string; message: string } | undefined {
  for (const [name, check] of Object.entries(health.checks)) {
    if (check.status === "warn") {
      return { name, message: check.message };
    }
  }
  return undefined;
}

function issueHeadline(checkName: string): string {
  switch (checkName) {
    case "config":
      return "Refresh workspace config";
    case "db":
    case "index":
      return "Repair semantic index";
    case "embedder":
      return "Embedding setup incomplete";
    case "vector":
      return "Vector search unavailable";
    case "version":
      return "Runtime version mismatch";
    default:
      return "Needs attention";
  }
}

function deriveIndexingHeadline(ctx: PanelContext): string {
  const indexStatus = ctx.indexStatus;
  switch (indexStatus?.phase) {
    case "cold_index":
      return "Building initial index";
    case "rebuild":
      return "Rebuilding semantic index";
    case "embedding":
      return "Generating embeddings (search already works)";
    case "sweep":
      return "Syncing workspace index";
    case "branch_change":
      return "Refreshing after branch change";
    case "incremental":
      if (indexStatus.inflightCount > 1) {
        return `Indexing ${indexStatus.inflightCount} files`;
      }
      return "Indexing changed file";
    case "watching":
      if (indexStatus.pendingCount > 0) {
        return `Queued ${indexStatus.pendingCount} files for indexing`;
      }
      return "Watching for file changes";
    case "error":
      return "Live indexing needs attention";
    default:
      break;
  }
  if (ctx.indexingMessage) {
    return "Preparing semantic index";
  }
  return "Updating semantic index";
}

function deriveIndexingDetail(ctx: PanelContext): string {
  return (
    ctx.indexStatus?.message ??
    ctx.indexingMessage ??
    "Indexing your codebase. This may take a few minutes."
  );
}

interface IndexSectionView {
  title: string;
  summary: string;
  progressPercent?: number;
  progressLabel: string;
  busy: boolean;
  pendingFiles: string[];
  inflightFiles: string[];
  recentFiles: string[];
}

function deriveIndexSectionView(ctx: PanelContext): IndexSectionView {
  const status = ctx.indexStatus;
  // An explicit pause wins over any stale daemon status: show a clear paused
  // state so the user knows updates are intentionally stopped.
  if (ctx.syncPaused && !status?.active) {
    return {
      title: "Index sync paused",
      summary:
        "Automatic indexing is paused. Cognis still answers from the last-synced index; resume to track new changes.",
      progressPercent: 0,
      progressLabel: "Paused",
      busy: false,
      pendingFiles: [],
      inflightFiles: [],
      recentFiles: status?.recentFiles ?? [],
    };
  }
  if (status) {
    return {
      title: deriveIndexingHeadline(ctx),
      summary: deriveIndexingDetail(ctx),
      progressPercent: status.progressPercent,
      progressLabel: status.active
        ? status.pendingCount > 0 || status.inflightCount > 0
          ? `${status.pendingCount + status.inflightCount} file${
              status.pendingCount + status.inflightCount === 1 ? "" : "s"
            }`
          : "Live"
        : "Stopped",
      busy:
        status.active &&
        (status.pendingCount > 0 ||
          status.inflightCount > 0 ||
          !["watching", "idle", "stopped"].includes(status.phase)),
      pendingFiles: status.pendingFiles,
      inflightFiles: status.inflightFiles,
      recentFiles: status.recentFiles,
    };
  }
  if (ctx.status === "indexing") {
    return {
      title: deriveIndexingHeadline(ctx),
      summary: deriveIndexingDetail(ctx),
      progressPercent: undefined,
      progressLabel: "Working",
      busy: true,
      pendingFiles: [],
      inflightFiles: [],
      recentFiles: [],
    };
  }
  if (ctx.liveIndexing) {
    return {
      title: "Watching for file changes",
      summary:
        "Saved files are automatically queued for incremental indexing so semantic search stays fresh.",
      progressPercent: 100,
      progressLabel: "Live",
      busy: false,
      pendingFiles: [],
      inflightFiles: [],
      recentFiles: [],
    };
  }
  return {
    title: "Live indexing is paused",
    summary:
      "Start live indexing to automatically re-index saved file changes in this workspace.",
    progressPercent: 0,
    progressLabel: "Paused",
    busy: false,
    pendingFiles: [],
    inflightFiles: [],
    recentFiles: [],
  };
}

export function derivePanelView(ctx: PanelContext): PanelView {
  const { status, health, liveIndexing, mcpEnabled } = ctx;
  const statusClass = STATUS_CLASSES[status] ?? STATUS_CLASSES.unknown;

  // While the daemon reports *active indexing work* (the cold index or the
  // embedding backfill), health is transiently inconsistent: the DB is being
  // written and the vector table is not complete yet, so a poll can momentarily
  // read a failing "vector"/"index" check or fail to open the WAL-locked DB.
  // Never surface a setup/repair verdict in this window — that caused a
  // first-run "Generating…" → "Set Up for AI" regression and a repeated
  // "Troubleshoot" loop. Show progress and let the next poll settle once
  // embeddings finish.
  //
  // Gate this on genuine in-flight work (isIndexStatusBusy) rather than the
  // broad `indexStatus.active`: in the steady-state `watching`/`idle` phase the
  // DB is settled and health is trustworthy, so a real failing check (e.g. a
  // stale `index_version` after an upgrade) must surface here instead of being
  // masked by "Watching for file changes" — otherwise the headline and the
  // onboarding stepper disagree (stepper reads health.overall directly).
  if (status === "indexing" || isIndexStatusBusy(ctx.indexStatus)) {
    return {
      headline: deriveIndexingHeadline(ctx),
      detail: deriveIndexingDetail(ctx),
      statusClass: "status-active",
      primary: { id: "output", label: "View Output" },
    };
  }

  if (!health || status === "notInstalled") {
    // A workspace that has already been set up must not regress to the
    // first-run "Set Up for AI"/"Install backend" actions just because health
    // is momentarily unavailable (e.g. the poll landed while indexd was still
    // releasing the DB right after embedding). Show a non-destructive checking
    // state; the next poll recovers without the user touching anything.
    if (ctx.configured) {
      return {
        headline: "Finishing setup…",
        detail:
          "Cognis is verifying this workspace. This clears on its own once indexing settles — no action needed.",
        statusClass: "status-active",
        primary: { id: "output", label: "View Output" },
      };
    }
    // Fresh machine: the engine binary isn't installed yet, so `doctor` can't
    // even produce a checklist. Offer a one-click install instead of any manual
    // setup.
    if (ctx.backendAvailable === false && !ctx.prerequisites) {
      return {
        headline: "Install the Cognis engine",
        detail:
          "This extension is the control panel; the search engine is a small, self-contained binary. " +
          "Click Install engine and Cognis downloads it for you — no terminal, no setup.",
        statusClass: "status-warn",
        primary: { id: "installBackend", label: "Install engine" },
      };
    }
    // Gate setup on the prerequisite checklist: if a required component is
    // missing, point the user at the checklist instead of letting setup fail
    // partway through (and create a half-provisioned .cognis/).
    if (ctx.prerequisites && !ctx.prerequisites.ready) {
      return {
        headline: "Install prerequisites",
        detail:
          "Some required components are not installed yet. Use the checklist above to install them, then run Set Up Workspace.",
        statusClass: "status-warn",
        primary: { id: "setup", label: "Set Up Workspace", disabled: true },
      };
    }
    return {
      headline: "Setup required",
      detail:
        "Set up Cognis for this workspace: it indexes your code and connects an MCP server to your editor so it can search your code.",
      statusClass: "status-muted",
      primary: { id: "setup", label: "Set Up Workspace" },
    };
  }

  const failing = firstFailingCheck(health);
  if (failing || status === "degraded" && health.overall === "fail") {
    const headline = failing ? issueHeadline(failing.name) : "Needs attention";
    const detail =
      failing?.message ??
      "Run repair setup to restore semantic search for this workspace.";
    return {
      headline,
      detail,
      statusClass: "status-warn",
      primary: { id: "repair", label: "Troubleshoot" },
    };
  }

  // All healthy-index states live in this single block so the top-of-panel
  // verdict, the onboarding stepper, and the Index Status section always agree.
  // Critically, a healthy index with live sync *off* — whether the user paused
  // it or the daemon simply isn't running — is NOT a broken state: it only
  // means new file changes aren't being tracked. Offer to turn sync back on
  // instead of mislabeling an intentional pause as "needs repair" (which
  // previously contradicted the "Index sync paused" panel just below it).
  if (health.overall === "ok") {
    if (!mcpEnabled) {
      return {
        headline: "Connect Cognis to your editor (MCP)",
        detail:
          "Your index is built. One step left: add Cognis as an MCP server so your editor can search your code in chat. This writes a workspace mcp.json.",
        statusClass: "status-ok",
        primary: { id: "connectMcp", label: "Connect MCP (mcp.json)" },
      };
    }
    if (ctx.syncPaused) {
      return {
        headline: "Connected — index sync paused",
        detail:
          "Cognis is answering from the last-synced index. Resume sync so saved file changes are re-indexed automatically.",
        statusClass: "status-ok",
        primary: { id: "resumeSync", label: RESUME_SYNC_LABEL },
      };
    }
    if (!liveIndexing) {
      return {
        headline: "Connected — live sync is off",
        detail:
          "Your index is healthy and MCP is connected. Turn live indexing back on so saved changes are re-indexed automatically.",
        statusClass: "status-ok",
        primary: { id: "resumeSync", label: RESUME_SYNC_LABEL },
      };
    }
    return {
      headline: "Cognis MCP server connected",
      detail:
        "Semantic search and live indexing are active. If the Cognis tools don't appear in your editor's chat yet, reload the editor window.",
      statusClass: "status-ok",
    };
  }

  if (health.overall === "warn" || status === "degraded") {
    const warning = firstWarningCheck(health);
    return {
      headline: warning ? issueHeadline(warning.name) : "Needs attention",
      detail: warning?.message ?? "Some checks reported warnings.",
      statusClass: "status-warn",
      primary: { id: "repair", label: "Troubleshoot" },
    };
  }

  return {
    headline: "Checking status…",
    detail: "Waiting for workspace health information.",
    statusClass,
    primary: { id: "setup", label: "Set Up Workspace" },
  };
}

/**
 * Short, stable status-bar label.
 *
 * The panel headline is intentionally descriptive (and changes a lot); the
 * status bar should stay glanceable, so we collapse everything to a small fixed
 * vocabulary — Indexing / Ready / Action needed / Not set up — paired with the
 * existing icon. Tooltip (set by the caller) carries the detail.
 */
export function outcomeLabelForContext(ctx: PanelContext): string {
  const view = derivePanelView(ctx);
  if (ctx.status === "indexing" || isIndexStatusBusy(ctx.indexStatus)) {
    return "$(sync~spin) Cognis: Indexing";
  }
  // Mirror deriveStatusLine: a Confirmed_Mismatch the user must act on reads as
  // "Action needed", overriding the "Ready"/warn verdicts below regardless of
  // health being ok or semantic/vector warnings (R1.4). Busy indexing above
  // still shows "Indexing" first.
  if (isConfirmedMismatch(ctx.compatibility)) {
    return "$(warning) Cognis: Action needed";
  }
  if (view.statusClass === "status-ok") {
    return ctx.mcpEnabled
      ? "$(plug) Cognis: Ready"
      : "$(check) Cognis: Index ready";
  }
  if (ctx.status === "notInstalled") {
    return "$(circle-slash) Cognis: Not set up";
  }
  if (view.statusClass === "status-warn") {
    return "$(warning) Cognis: Action needed";
  }
  return "$(question) Cognis";
}

export type SetupStepState = "done" | "active" | "pending" | "error";

export interface SetupStep {
  id: string;
  label: string;
  state: SetupStepState;
}

/**
 * Collapse the many internal states into a fixed 4-step onboarding path so a
 * first-time user always sees *where they are* and the single next action,
 * instead of decoding headlines like "Managed setup needs repair".
 *
 *   ① Backend   → ② Components → ③ Index synced → ④ MCP connected
 *
 * The steps are derived purely from the same context the panel already has, so
 * they never disagree with the status pill or the primary action.
 */
export function deriveSetupSteps(ctx: PanelContext): SetupStep[] {
  const { health, prerequisites, mcpEnabled, liveIndexing, status } = ctx;
  const configured = ctx.configured ?? false;
  const healthOk = health?.overall === "ok";

  // ① Engine binary usable.
  let backend: SetupStepState;
  if (ctx.backendAvailable === false) {
    backend = "error";
  } else if (prerequisites || health || configured) {
    backend = "done";
  } else {
    backend = "active";
  }

  // ② Required components installed.
  let components: SetupStepState;
  if (backend !== "done") {
    components = "pending";
  } else if (!prerequisites) {
    components = configured ? "done" : "active";
  } else if (prerequisites.ready) {
    components = "done";
  } else {
    components = "error";
  }

  // ③ Index built / synced.
  let indexed: SetupStepState;
  if (status === "indexing") {
    indexed = "active";
  } else if (components !== "done") {
    indexed = "pending";
  } else if (healthOk) {
    indexed = "done";
  } else if (configured) {
    indexed = health ? "error" : "active";
  } else {
    indexed = "pending";
  }

  // ④ MCP tools connected to the editor. Reaches "done" once mcp.json is
  // written and the index is healthy — the completion criterion the user
  // actually controls, and the one that matches the "connected" panel headline.
  // A live runtime process is a stronger signal but isn't always observable
  // (e.g. Windows can't scope the process to this repo), so requiring it would
  // leave the stepper stuck "active" forever on the happy path — making setup
  // feel like it never finishes.
  let connected: SetupStepState;
  const mcpRuntimeActive = (ctx.mcpRuntimeCount ?? 0) > 0;
  if (indexed !== "done") {
    connected = "pending";
  } else if (mcpEnabled && (mcpRuntimeActive || healthOk)) {
    connected = "done";
  } else {
    connected = "active";
  }

  return [
    { id: "backend", label: "Engine", state: backend },
    { id: "components", label: "Components", state: components },
    { id: "indexed", label: "Index synced", state: indexed },
    { id: "connected", label: "MCP connected", state: connected },
  ];
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderFileGroup(
  label: string,
  files: string[],
  emptyText: string
): string {
  const body = files.length
    ? `<ul class="file-list">${files
        .map(
          (file) =>
            `<li class="file-item"><span class="file-dot"></span><code>${escapeHtml(
              file
            )}</code></li>`
        )
        .join("")}</ul>`
    : `<div class="empty-files">${escapeHtml(emptyText)}</div>`;
  return `<div class="file-group">
    <div class="file-group-label">${escapeHtml(label)}</div>
    ${body}
  </div>`;
}

function stepMarker(state: SetupStepState): { glyph: string; cls: string } {
  switch (state) {
    case "done":
      return { glyph: "✓", cls: "step-done" };
    case "active":
      return { glyph: "●", cls: "step-active" };
    case "error":
      return { glyph: "!", cls: "step-error" };
    default:
      return { glyph: "○", cls: "step-pending" };
  }
}

/**
 * Render the 4-step onboarding stepper. Gives a first-time user a fixed mental
 * model of the path (Backend → Components → Index → MCP) and where they are,
 * instead of decoding free-form headlines.
 */
export function renderStepperSection(context: PanelContext): string {
  const steps = deriveSetupSteps(context);
  // Once everything is connected the stepper is just noise — hide it so the
  // panel stays focused on the live index status.
  if (steps.every((s) => s.state === "done")) {
    return "";
  }
  const items = steps
    .map((step, idx) => {
      const { glyph, cls } = stepMarker(step.state);
      return `<li class="step-item ${cls}">
        <span class="step-marker">${glyph}</span>
        <span class="step-label">${escapeHtml(step.label)}</span>
        ${idx < steps.length - 1 ? `<span class="step-bar" aria-hidden="true"></span>` : ""}
      </li>`;
    })
    .join("");
  return `<div class="surface">
    <div class="surface-title">Getting started</div>
    <ol class="step-list">${items}</ol>
  </div>`;
}

/** Human label for an MCP host id. */
function hostLabel(host: string | undefined): string {
  switch (host) {
    case "cursor":
      return "Cursor";
    case "vscode":
      return "VS Code";
    case "kiro":
      return "Kiro";
    case "claude":
      return "Claude";
    default:
      return "your editor";
  }
}

/**
 * Render the MCP server status surface.
 *
 * Cognis *is* an MCP server, so the panel states this explicitly: whether the
 * server is wired into the editor (its `mcp.json` is written), what the server
 * is called, where the config lives, and the one action that connects it. This
 * replaces vague "AI" wording with the concrete thing the user needs to know —
 * the MCP server and how to connect it. Hidden until the workspace is set up
 * (before that the onboarding stepper/primary action guides the user).
 */
export function renderMcpSection(context: PanelContext): string {
  if (!context.configured && !context.mcpEnabled) {
    return "";
  }
  const configured = Boolean(context.mcpEnabled);
  const runtimeCount = context.mcpRuntimeCount ?? 0;
  const runtimeActive = runtimeCount > 0;
  const repoScoped = context.mcpRuntimeRepoScoped ?? false;
  // Cursor-style: wired in mcp.json *and* the editor has spawned cognis_mcpd.
  const connected = configured && runtimeActive;
  const host = hostLabel(context.mcpHost);
  let statusText: string;
  if (connected && repoScoped) {
    statusText = `Connected to ${escapeHtml(host)}. The editor is running the Cognis MCP server for this repo (${runtimeCount} process${runtimeCount === 1 ? "" : "es"}).`;
  } else if (connected) {
    // Machine-wide best effort (e.g. Windows can't read per-process env): the
    // server is up, but we can't prove it's bound to *this* repo's database.
    statusText = `Connected to ${escapeHtml(host)}. A Cognis MCP server is running (detected machine-wide; this OS can't confirm it's bound to this repo).`;
  } else if (configured) {
    statusText = `Configured in mcp.json but no live MCP process yet. Open MCP tools in ${escapeHtml(host)} or reload the window.`;
  } else {
    statusText = `Not connected yet. Add Cognis as an MCP server in ${escapeHtml(host)} — this writes a workspace mcp.json the editor reads to launch the server.`;
  }
  // When connected, offer a non-destructive Disconnect alongside the re-write
  // action. This lives in the MCP card (not the Danger-zone) because it only
  // removes this repo's entry from mcp.json — it keeps the index and data.
  const action = configured
    ? `<button data-action="connectMcp" title="Rewrite this workspace's MCP config (mcp.json) for the current editor.">Re-write mcp.json</button>`
      + `<button data-action="disconnectMcp" title="Remove this repo's Cognis entry from mcp.json. Non-destructive; keeps your index.">Disconnect MCP</button>`
    : `<button data-action="connectMcp" title="Write this workspace's MCP config (mcp.json) so your editor can use Cognis.">Connect MCP (mcp.json)</button>`;
  const serverRow = context.mcpServerName
    ? `<div class="surface-detail">Server: <code>${escapeHtml(context.mcpServerName)}</code></div>`
    : "";
  const pathRow = context.mcpConfigPath
    ? `<div class="surface-detail">Config: <code>${escapeHtml(context.mcpConfigPath)}</code></div>`
    : "";
  // Only warn about duplicates when the count is repo-scoped: a machine-wide
  // count is legitimately > 1 when several workspaces are open at once.
  const duplicateRow =
    repoScoped && runtimeCount > 1
      ? `<div class="surface-detail surface-warn">Warning: ${runtimeCount} Cognis MCP processes are running for this repo. Cursor may have spawned duplicates — reload the window to clean up.</div>`
      : "";
  const mark = connected ? "✓" : configured ? "•" : "•";
  const markCls = connected ? "prereq-ok" : configured ? "prereq-required" : "prereq-required";
  const headline = connected
    ? "connected"
    : configured
      ? "configured (not running)"
      : "not connected";
  // Collapsed once connected (status is enough); expanded when action is needed.
  const openAttr = connected ? "" : " open";
  return `<div class="surface">
    <details class="prereq-details"${openAttr}>
      <summary class="prereq-summary">
        <span class="prereq-summary-mark ${markCls}">${mark}</span>
        <span class="prereq-summary-text">
          <span class="surface-title">MCP server — ${headline}</span>
          <span class="surface-detail">${statusText}</span>
        </span>
        <span class="prereq-chevron" aria-hidden="true">▸</span>
      </summary>
      <div class="prereq-body">
        <div class="surface-actions">${action}</div>
        ${serverRow}
        ${pathRow}
        ${duplicateRow}
        <div class="surface-detail">Your editor starts and stops the MCP server automatically from this file.</div>
        ${renderMcpHttpSubsection(context)}
      </div>
    </details>
  </div>`;
}

/**
 * Optional standalone HTTP MCP server (panel-managed, per workspace).
 *
 * Hidden until the user explicitly starts it. Once started this surface is the
 * single source of truth for the URL the editor (or any HTTP client) connects
 * to: a Start/Stop control and the live ``http://127.0.0.1:<port>/mcp`` URL.
 * When stopped it offers an explicit Start button so it never auto-spawns.
 */
function renderMcpHttpSubsection(context: PanelContext): string {
  const phase = context.mcpServerPhase ?? "stopped";
  const url = context.mcpServerUrl;
  const err = context.mcpServerError;
  const stoppedDetail =
    "Optional: run a standalone HTTP server with a stable address (panel-managed, per workspace). Most users do not need this — the config above is enough for Cursor / VS Code.";
  let label: string;
  let detail: string;
  let button: string;
  // Raw technical values (address URL, error string) never appear in the main
  // status text above — they are relocated to labeled detail rows in the body
  // so they stay one glance away without leaking jargon into the headline (R9).
  const detailRows: string[] = [];
  switch (phase) {
    case "starting":
      label = "Starting…";
      detail = "Starting the server…";
      if (url) {
        detailRows.push(
          `<div class="surface-detail">Address: <code>${escapeHtml(url)}</code></div>`
        );
      }
      button = `<button data-action="stopMcp" title="Stop the panel-managed HTTP MCP server.">Stop</button>`;
      break;
    case "running":
      label = "Running";
      detail = "The server is running.";
      if (url) {
        detailRows.push(
          `<div class="surface-detail">Address: <code>${escapeHtml(url)}</code></div>`
        );
      }
      button = `<button data-action="stopMcp" title="Stop the panel-managed HTTP MCP server.">Stop</button>`;
      break;
    case "error":
      label = "Error";
      detail = "The MCP server stopped unexpectedly.";
      if (err) {
        detailRows.push(
          `<div class="surface-detail">Details: <code>${escapeHtml(err)}</code></div>`
        );
      }
      button = `<button data-action="startMcp" title="Start a standalone HTTP MCP server for this workspace.">Start</button>`;
      break;
    default:
      label = "Stopped";
      detail = stoppedDetail;
      button = `<button data-action="startMcp" title="Start a standalone HTTP MCP server for this workspace.">Start</button>`;
  }
  const extraRows = detailRows.join("\n      ");
  return `<details class="prereq-details">
    <summary class="prereq-summary">
      <span class="prereq-summary-mark prereq-optional">•</span>
      <span class="prereq-summary-text">
        <span class="surface-title">Standalone HTTP MCP server — ${escapeHtml(label)}</span>
        <span class="surface-detail">${detail}</span>
      </span>
      <span class="prereq-chevron" aria-hidden="true">▸</span>
    </summary>
    <div class="prereq-body">
      <div class="surface-actions">${button}</div>
      ${extraRows}
    </div>
  </details>`;
}

/**
 * Render the prerequisite checklist surface.
 *
 * When everything required is installed the checklist is **collapsed** by
 * default into a one-line "ready" summary (it's just noise once you're set up);
 * the user can expand it to re-check or install optional extras. When a required
 * component is missing it stays **expanded** so the action is obvious. Returns
 * "" when there is no report yet so the panel stays clean.
 */
export function renderPrerequisitesSection(context: PanelContext): string {
  const report = context.prerequisites;
  if (!report) {
    return "";
  }
  const rows = report.items
    .map((item) => {
      const ok = item.status === "ok";
      const marker = ok ? "✓" : "•";
      const markerClass = ok
        ? "prereq-mark prereq-ok"
        : item.required
          ? "prereq-mark prereq-required"
          : "prereq-mark prereq-optional";
      const badge = item.required
        ? `<span class="prereq-badge">required</span>`
        : `<span class="prereq-badge prereq-badge-optional">optional</span>`;
      const action = ok
        ? `<span class="prereq-state">Installed</span>`
        : `<button class="prereq-install" data-action="installPrerequisite" data-item="${escapeHtml(
            item.id
          )}">Install</button>`;
      return `<li class="prereq-item">
        <span class="${markerClass}">${marker}</span>
        <div class="prereq-copy">
          <div class="prereq-label">${escapeHtml(item.label)} ${badge}</div>
          <div class="prereq-desc">${escapeHtml(item.description)}</div>
        </div>
        <div class="prereq-action">${action}</div>
      </li>`;
    })
    .join("");

  const total = report.items.length;
  const installedCount = report.items.filter(
    (item) => item.status === "ok"
  ).length;
  const optionalMissing = report.items.filter(
    (item) => !item.required && item.status === "missing"
  ).length;

  // Collapsed (ready) summary vs expanded (action-needed) summary.
  const summary = report.ready
    ? optionalMissing > 0
      ? `Ready — ${installedCount}/${total} components installed (${optionalMissing} optional available).`
      : `Ready — all ${total} components installed.`
    : "Install the required components below before running Set Up Workspace.";

  const installAll =
    !report.ready && report.combined_install_target
      ? `<button data-action="installAllPrerequisites" title="Install every missing component in one step.">Install all</button>`
      : "";

  // <details> drives collapse/expand natively. Open when action is needed,
  // collapsed when the workspace is ready for work.
  const openAttr = report.ready ? "" : " open";

  return `<div class="surface">
    <details class="prereq-details"${openAttr}>
      <summary class="prereq-summary">
        <span class="prereq-summary-mark ${report.ready ? "prereq-ok" : "prereq-required"}">${
          report.ready ? "✓" : "!"
        }</span>
        <span class="prereq-summary-text">
          <span class="surface-title">Prerequisites</span>
          <span class="surface-detail">${escapeHtml(summary)}</span>
        </span>
        <span class="prereq-chevron" aria-hidden="true">▸</span>
      </summary>
      <div class="prereq-body">
        <div class="surface-actions prereq-body-actions">
          ${installAll}
          <button data-action="refreshPrerequisites" title="Re-check installed components.">Re-check</button>
        </div>
        <ul class="prereq-list">${rows}</ul>
      </div>
    </details>
  </div>`;
}

/**
 * Render the labeled Advanced_Surface detail area that carries the RAW Engine
 * and Extension version strings for a confirmed mismatch (R6.3).
 *
 * This is the ONLY place the raw versions are allowed to appear: a detail
 * surface with explicit labels ("Engine version" / "Extension version"),
 * separate from the main status text (the Status_Line + caption never embed a
 * raw version — see {@link deriveCompatibilityHint}, Correctness Property 3).
 * Rendered only on the Advanced_Surface; the Minimal_Surface never calls it, so
 * the raw versions can never leak onto the minimal surface.
 *
 * Returns "" when there is no confirmed mismatch (nothing to show) so the
 * advanced body stays clean in the healthy/operational case.
 */
export function renderCompatibilityDetail(context: PanelContext): string {
  const snapshot = context.compatibility;
  if (!isConfirmedMismatch(snapshot)) {
    return "";
  }
  const result = snapshot.result;
  const remediation = deriveRemediation(result);
  // Only surface versions we actually have; a labeled row is omitted when the
  // corresponding version is unknown (e.g. an unreadable payload).
  const rows: string[] = [];
  if (result.engineVersion) {
    rows.push(
      `<div class="surface-detail">Engine version: <code>${escapeHtml(
        result.engineVersion
      )}</code></div>`
    );
  }
  if (result.expectedEngineVersion) {
    rows.push(
      `<div class="surface-detail">Extension expects Engine version: <code>${escapeHtml(
        result.expectedEngineVersion
      )}</code></div>`
    );
  }
  if (context.version) {
    rows.push(
      `<div class="surface-detail">Extension version: <code>${escapeHtml(
        context.version
      )}</code></div>`
    );
  }
  const caption = remediation
    ? deriveCompatibilityHint(result) ?? ""
    : "";
  const captionRow = caption
    ? `<div class="surface-detail">${escapeHtml(caption)}</div>`
    : "";
  return `<div class="surface">
    <div class="surface-title">Compatibility</div>
    ${captionRow}
    ${rows.join("\n    ")}
  </div>`;
}

function panelHtml(
  logoSrc: vscode.Uri,
  cspSource: string,
  view: PanelView,
  context: PanelContext,
  nonce: string
): string {
  const indexView = deriveIndexSectionView(context);
  const progressWidth =
    indexView.progressPercent === undefined
      ? 72
      : Math.max(0, Math.min(100, indexView.progressPercent));
  const progressLabel =
    indexView.progressPercent === undefined
      ? indexView.progressLabel
      : `${indexView.progressPercent.toFixed(0)}%`;
  const primaryBlock = view.primary
    ? `<button class="primary" data-action="${escapeHtml(view.primary.id)}"${
        view.primary.disabled ? " disabled" : ""
      }>${escapeHtml(view.primary.label)}</button>`
    : "";

  // The unified primary control + single status line — the two elements the
  // Minimal_Surface consists of. Task 3.2 will also prepend these to the
  // Advanced_Surface and enforce the single-unified-control invariant.
  const unified = deriveUnifiedControl(context);
  const statusLine = deriveStatusLine(context);
  const unifiedBlock = renderUnifiedControl(unified);
  const statusBlock = renderStatusLine(statusLine, deriveStatusHint(context));

  // Shared hero header, reused by both the minimal and advanced bodies.
  const hero = `<div class="hero">
    <img src="${logoSrc}" alt="Cognis logo" />
    <div class="hero-copy">
      <div class="title">Cognis${
        context.version
          ? ` <span class="version-badge">v${escapeHtml(context.version)}</span>`
          : ""
      }</div>
      <div class="subtitle">Semantic index and MCP setup for your editor.</div>
    </div>
  </div>`;

  // Minimal_Surface: hero + exactly one surface wrapping ONLY the unified
  // control and the status line. No stepper, MCP, prerequisites, Index Status
  // file lists, footer log links, or danger zone are rendered here — those
  // simply aren't emitted, so R2.1/R2.4–R2.7/R5.1/R6.1 hold by construction.
  if (!context.advancedMode) {
    const minimalBody = `${hero}

  <div class="surface">
    ${unifiedBlock}
    ${statusBlock}
  </div>`;
    return htmlDocument(cspSource, nonce, minimalBody);
  }

  // Advanced_Surface is a strict SUPERSET of the Minimal_Surface: the unified
  // control (data-unified) and the single status line (data-status-line) are
  // prepended at the top, then the entire existing detail surface follows
  // (stepper, MCP, prerequisites, Index Status, danger zone, log links).
  //
  // Invariants preserved here:
  //  - Exactly one [data-unified] in the whole document: only `unifiedBlock`
  //    carries it. The Index Status pause/resume buttons below use
  //    data-action="pauseSync"/"resumeSync" WITHOUT data-unified.
  //  - Exactly one [data-status-line]: only `statusBlock` carries it. The
  //    status-pill (view.headline) is a separate, unlabelled marker.
  //  - Raw technical values stay inside the labelled detail surfaces below
  //    (MCP/prerequisites/Index Status), never in the unified control or
  //    status line.
  const advancedBody = `${hero}

  <div class="surface">
    <div class="primary-action">${unifiedBlock}</div>
    ${statusBlock}
    <div class="status-overview">
      <div class="status-pill ${view.statusClass}">
        <span class="status-dot"></span>
        <span class="headline">${escapeHtml(view.headline)}</span>
      </div>
      <div class="status-copy">
        ${
          view.detail
            ? `<div class="detail detail-${view.statusClass}">${escapeHtml(view.detail)}</div>`
            : ""
        }
      </div>
    </div>
    ${
      primaryBlock
        ? `<div class="primary-action">${primaryBlock}</div>`
        : ""
    }
  </div>

  ${renderCompatibilityDetail(context)}

  ${renderStepperSection(context)}

  ${renderMcpSection(context)}

  ${renderPrerequisitesSection(context)}

  <div class="surface">
    <div class="surface-header">
      <div>
        <div class="surface-title">Index Status</div>
        <div class="surface-detail">
          Track what Cognis is indexing now. Setup and repair manage live indexing automatically when the workspace needs it.
        </div>
      </div>
      <div class="surface-actions">
        ${
          context.syncPaused
            ? `<button data-action="resumeSync" title="Resume automatic indexing of file changes in this workspace.">${RESUME_SYNC_LABEL}</button>`
            : `<button data-action="pauseSync" title="Pause automatic indexing. Cognis keeps answering from the current index but stops tracking new changes.">Pause sync</button>`
        }
        <button data-action="clearReindex" title="Delete the stored index and rebuild from scratch. Keeps your config and MCP wiring.">Rebuild index</button>
        ${
          indexView.busy
            ? `<button data-action="cancelIndexing" title="Stop the running index build. Keeps the partial index; you can rebuild later.">Cancel indexing</button>`
            : ""
        }
      </div>
    </div>
    <div class="progress-summary">
      <span>${escapeHtml(indexView.title)}</span>
      <span class="progress-value">${escapeHtml(progressLabel)}</span>
    </div>
    <div class="progress-track">
      <div
        class="progress-fill ${indexView.busy ? "busy" : "idle"}"
        style="width: ${progressWidth}%"
      ></div>
    </div>
    <div class="index-message">${escapeHtml(indexView.summary)}</div>
    <div class="file-sections">
      ${renderFileGroup(
        "Queued files",
        indexView.pendingFiles,
        "No files are waiting to be indexed."
      )}
      ${renderFileGroup(
        "Indexing now",
        indexView.inflightFiles,
        "No files are being indexed right now."
      )}
      ${renderFileGroup(
        "Recently indexed",
        indexView.recentFiles,
        "Recent indexing activity will appear here."
      )}
    </div>
  </div>

  <div class="footer-links">
    <button class="link" data-action="health">Health report</button>
    <button class="link" data-action="output">Output log</button>
  </div>

  <details class="advanced">
    <summary>Danger zone</summary>
    <div class="advanced-group">
      <div class="advanced-label">Reset &amp; recover</div>
      <div class="link-actions">
        <button class="link link-danger" data-action="reinstallEngine" title="Delete the downloaded engine binary + semantic model and fetch fresh, checksum-verified copies. Keeps your index and MCP wiring.">Reinstall engine</button>
        <button class="link link-danger" data-action="coldRestart" title="Full clean slate: wipe this workspace's .cognis index, remove ALL cognis MCP entries, uninstall the engine + model, then re-download everything and re-index from scratch. Your source code is untouched.">Cold restart (wipe &amp; rebuild)</button>
      </div>
      <div class="surface-detail">Cold restart fixes a corrupted or stale state — a legacy vector index, a locked binary, or a half-finished setup.</div>
    </div>
    <div class="advanced-group">
      <div class="advanced-label">Remove Cognis</div>
      <div class="link-actions">
        <button class="link link-danger" data-action="remove" title="Stop indexing, disconnect MCP for this repo, and delete the local .cognis index for this workspace.">Remove from this workspace</button>
        <button class="link link-danger" data-action="prepareUninstall" title="Stop indexing, delete this workspace's .cognis index, remove ALL cognis MCP entries from your editor, and uninstall the Cognis engine Cognis installed. Run this before uninstalling the extension.">Remove everything (prepare to uninstall)</button>
        <button class="link link-danger" data-action="forceCleanup" title="Force-stop any running Cognis processes (indexd/mcpd) that are holding the local database open, then delete .cognis. Use this when a normal Remove failed because the database was locked (Windows &quot;the process cannot access the file&quot; / &quot;directory not empty&quot;).">Force cleanup (kill processes &amp; delete .cognis)</button>
      </div>
      <div class="surface-detail">Your source code is never touched. "Remove everything" also uninstalls the engine Cognis installed for you. Use "Force cleanup" only if a normal Remove failed because a process was locking the database.</div>
    </div>
  </details>`;
  return htmlDocument(cspSource, nonce, advancedBody);
}

/**
 * Shared HTML document scaffold. Wraps a body fragment in the exact same
 * `<head>`/`<style>`/CSP/nonce and action-dispatch `<script>` used by both the
 * minimal and advanced surfaces, so the large style block is written once.
 */
function htmlDocument(
  cspSource: string,
  nonce: string,
  bodyInner: string
): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${cspSource} https:; style-src ${cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <style>
    :root {
      color-scheme: light dark;
      --accent: #1f8fff;
      --accent-soft: rgba(31, 143, 255, 0.14);
      --accent-soft-strong: rgba(31, 143, 255, 0.22);
      --warm: #ff9a2e;
      --ok: #3ecf8e;
      --text: var(--vscode-foreground);
      --muted: var(--vscode-descriptionForeground);
      --panel: var(--vscode-editor-background);
      --border: var(--vscode-panel-border);
      --surface: var(--vscode-sideBar-background, var(--panel));
      --button-bg: var(--vscode-button-background);
      --button-fg: var(--vscode-button-foreground);
      --button-hover: var(--vscode-button-hoverBackground);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      padding: 16px 14px 20px;
      font-family: var(--vscode-font-family);
      color: var(--text);
      background: var(--panel);
    }
    .hero {
      display: flex;
      align-items: center;
      gap: 12px;
      margin-bottom: 14px;
    }
    .hero img {
      width: 44px;
      height: 44px;
      object-fit: contain;
    }
    .hero-copy {
      display: flex;
      flex-direction: column;
      gap: 4px;
    }
    .title {
      font-size: 16px;
      font-weight: 700;
      letter-spacing: 0.02em;
    }
    .version-badge {
      font-size: 10px;
      font-weight: 600;
      letter-spacing: 0.04em;
      color: var(--muted);
      border: 1px solid var(--border);
      border-radius: 999px;
      padding: 1px 7px;
      margin-left: 6px;
      vertical-align: middle;
    }
    .subtitle {
      font-size: 12px;
      color: var(--muted);
      line-height: 1.4;
    }
    .surface {
      border: 1px solid var(--border);
      border-radius: 14px;
      background: var(--surface);
      padding: 14px;
      margin-bottom: 14px;
    }
    .status-overview {
      display: flex;
      flex-direction: column;
      align-items: flex-start;
      gap: 10px;
    }
    .status-copy {
      display: flex;
      flex-direction: column;
      gap: 6px;
    }
    .headline {
      font-size: 15px;
      font-weight: 600;
      line-height: 1.35;
    }
    .detail {
      font-size: 12px;
      color: var(--muted);
      line-height: 1.45;
    }
    /* Tint the detail text to match the status so warnings (e.g. a version
       mismatch that needs a re-index) stand out in amber instead of muted gray. */
    .detail-status-warn { color: var(--warm); }
    .detail-status-ok { color: var(--ok); }
    .status-pill {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 5px 10px;
      border-radius: 999px;
      font-size: 11px;
      border: 1px solid var(--border);
      background: var(--accent-soft);
      margin-top: 2px;
    }
    .status-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: var(--accent);
      flex-shrink: 0;
    }
    .status-ok .status-dot { background: #3ecf8e; }
    .status-warn .status-dot { background: var(--warm); }
    .status-active .status-dot { background: var(--accent); animation: pulse 1.2s ease-in-out infinite; }
    .status-muted .status-dot { background: var(--muted); }
    @keyframes pulse {
      0%, 100% { opacity: 1; transform: scale(1); }
      50% { opacity: 0.55; transform: scale(0.92); }
    }
    .primary-action {
      margin-top: 16px;
    }
    .primary-action button {
      width: 100%;
    }
    .status-line {
      margin-top: 12px;
    }
    .status-word {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 14px;
      font-weight: 600;
    }
    .status-hint {
      margin-top: 6px;
      font-size: 12px;
      color: var(--muted);
      line-height: 1.45;
    }
    .surface-header {
      display: flex;
      justify-content: space-between;
      align-items: flex-start;
      gap: 12px;
      margin-bottom: 12px;
    }
    .surface-title {
      font-size: 13px;
      font-weight: 700;
      letter-spacing: 0.02em;
    }
    .surface-detail {
      font-size: 12px;
      color: var(--muted);
      line-height: 1.45;
      margin-top: 4px;
    }
    .surface-detail.surface-warn {
      color: var(--warm);
    }
    .surface-actions {
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
      justify-content: flex-end;
    }
    .surface-actions button {
      padding: 7px 10px;
      font-size: 11px;
      white-space: nowrap;
    }
    .progress-summary {
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 8px;
      font-size: 12px;
      margin-bottom: 8px;
    }
    .progress-value {
      font-weight: 700;
      color: var(--muted);
    }
    .progress-track {
      position: relative;
      height: 8px;
      border-radius: 999px;
      background: var(--accent-soft);
      overflow: hidden;
      border: 1px solid rgba(255, 255, 255, 0.06);
    }
    .progress-fill {
      height: 100%;
      border-radius: inherit;
      transition: width 0.2s ease;
      background: linear-gradient(90deg, var(--accent), #54c4ff);
    }
    .progress-fill.busy {
      background:
        linear-gradient(
          90deg,
          var(--accent) 0%,
          #54c4ff 48%,
          var(--accent) 100%
        );
      background-size: 200% 100%;
      animation: progress-slide 1.5s linear infinite;
    }
    .progress-fill.idle {
      background: linear-gradient(90deg, var(--ok), #7ed8b4);
    }
    @keyframes progress-slide {
      0% { background-position: 0% 50%; }
      100% { background-position: 200% 50%; }
    }
    .index-message {
      margin-top: 10px;
      font-size: 12px;
      color: var(--muted);
      line-height: 1.5;
    }
    .file-sections {
      display: grid;
      gap: 10px;
      margin-top: 12px;
    }
    .file-group {
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 10px;
      background: rgba(255, 255, 255, 0.02);
    }
    .file-group-label {
      font-size: 10px;
      font-weight: 700;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
      margin-bottom: 8px;
    }
    .file-list {
      list-style: none;
      margin: 0;
      padding: 0;
      display: grid;
      gap: 6px;
    }
    .file-item {
      display: flex;
      align-items: flex-start;
      gap: 8px;
      font-size: 12px;
      line-height: 1.4;
    }
    .file-item code {
      font-family: var(--vscode-editor-font-family, var(--vscode-font-family));
      white-space: pre-wrap;
      word-break: break-word;
    }
    .file-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      margin-top: 5px;
      background: var(--accent);
      flex-shrink: 0;
    }
    .empty-files {
      font-size: 12px;
      color: var(--muted);
      line-height: 1.4;
    }
    .prereq-list {
      list-style: none;
      margin: 0;
      padding: 0;
      display: grid;
      gap: 10px;
    }
    .step-list {
      list-style: none;
      margin: 12px 0 0;
      padding: 0;
      display: flex;
      justify-content: space-between;
      gap: 4px;
    }
    .step-item {
      position: relative;
      flex: 1;
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 6px;
      text-align: center;
      min-width: 0;
    }
    .step-marker {
      width: 22px;
      height: 22px;
      border-radius: 50%;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      font-size: 12px;
      font-weight: 700;
      border: 1px solid var(--border);
      background: var(--surface);
      z-index: 1;
    }
    .step-label {
      font-size: 10px;
      line-height: 1.3;
      color: var(--muted);
      word-break: break-word;
    }
    .step-bar {
      position: absolute;
      top: 11px;
      left: 50%;
      width: 100%;
      height: 2px;
      background: var(--border);
      z-index: 0;
    }
    .step-done .step-marker { background: var(--accent-soft); color: var(--ok); border-color: transparent; }
    .step-done .step-label { color: var(--text); }
    .step-done .step-bar { background: var(--ok); }
    .step-active .step-marker { background: var(--accent); color: var(--button-fg); border-color: transparent; animation: pulse 1.2s ease-in-out infinite; }
    .step-active .step-label { color: var(--text); font-weight: 600; }
    .step-error .step-marker { background: rgba(255, 154, 46, 0.16); color: var(--warm); border-color: var(--warm); }
    .step-error .step-label { color: var(--warm); }
    .step-pending .step-marker { color: var(--muted); }
    .prereq-details summary {
      list-style: none;
      cursor: pointer;
      user-select: none;
    }
    .prereq-details summary::-webkit-details-marker { display: none; }
    .prereq-summary {
      display: flex;
      align-items: center;
      gap: 10px;
    }
    .prereq-summary-mark {
      width: 20px;
      height: 20px;
      border-radius: 50%;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      font-size: 12px;
      font-weight: 700;
      flex-shrink: 0;
    }
    .prereq-summary-mark.prereq-ok { background: var(--accent-soft); color: var(--ok); }
    .prereq-summary-mark.prereq-required { background: rgba(255, 154, 46, 0.16); color: var(--warm); }
    .prereq-summary-text {
      display: flex;
      flex-direction: column;
      gap: 2px;
      flex: 1;
      min-width: 0;
    }
    .prereq-chevron {
      color: var(--muted);
      font-size: 12px;
      transition: transform 0.15s ease;
      flex-shrink: 0;
    }
    .prereq-details[open] .prereq-chevron {
      transform: rotate(90deg);
    }
    .prereq-body {
      margin-top: 12px;
    }
    .prereq-body-actions {
      margin-bottom: 12px;
    }
    .prereq-item {
      display: flex;
      align-items: flex-start;
      gap: 10px;
    }
    .prereq-mark {
      width: 18px;
      height: 18px;
      border-radius: 50%;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      font-size: 11px;
      font-weight: 700;
      flex-shrink: 0;
      margin-top: 1px;
    }
    .prereq-ok { background: var(--accent-soft); color: var(--ok); }
    .prereq-required { background: rgba(255, 154, 46, 0.16); color: var(--warm); }
    .prereq-optional { background: var(--accent-soft); color: var(--muted); }
    .prereq-copy { flex: 1; min-width: 0; }
    .prereq-label {
      font-size: 12px;
      font-weight: 600;
      display: flex;
      align-items: center;
      gap: 8px;
      flex-wrap: wrap;
    }
    .prereq-desc {
      font-size: 11px;
      color: var(--muted);
      line-height: 1.45;
      margin-top: 2px;
    }
    .prereq-badge {
      font-size: 9px;
      font-weight: 700;
      letter-spacing: 0.06em;
      text-transform: uppercase;
      padding: 2px 6px;
      border-radius: 999px;
      background: rgba(255, 154, 46, 0.16);
      color: var(--warm);
    }
    .prereq-badge-optional {
      background: var(--accent-soft);
      color: var(--muted);
    }
    .prereq-action { flex-shrink: 0; }
    .prereq-state {
      font-size: 11px;
      color: var(--ok);
      white-space: nowrap;
    }
    button.prereq-install {
      padding: 6px 10px;
      font-size: 11px;
      white-space: nowrap;
    }
    button {
      appearance: none;
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 10px 12px;
      background: transparent;
      color: var(--text);
      font: inherit;
      text-align: center;
      cursor: pointer;
    }
    button.primary {
      background: var(--button-bg);
      color: var(--button-fg);
      border-color: transparent;
      font-weight: 600;
    }
    button:disabled {
      opacity: 0.55;
      cursor: default;
    }
    button:not(:disabled):hover {
      background: var(--button-hover, var(--accent-soft));
    }
    button.primary:not(:disabled):hover {
      filter: brightness(1.05);
    }
    details.advanced {
      margin-top: 8px;
    }
    details.advanced summary {
      font-size: 11px;
      font-weight: 600;
      letter-spacing: 0.06em;
      text-transform: uppercase;
      color: var(--muted);
      cursor: pointer;
      list-style: none;
      user-select: none;
    }
    details.advanced summary::-webkit-details-marker { display: none; }
    details.advanced summary::before {
      content: "▸ ";
      display: inline-block;
      transition: transform 0.15s ease;
    }
    details.advanced[open] summary::before {
      transform: rotate(90deg);
    }
    .advanced-group {
      margin-top: 10px;
    }
    .advanced-label {
      font-size: 10px;
      font-weight: 600;
      letter-spacing: 0.07em;
      text-transform: uppercase;
      color: var(--muted);
      margin-bottom: 6px;
    }
    .link-actions {
      display: flex;
      flex-direction: column;
      gap: 2px;
    }
    button.link {
      border: none;
      background: transparent;
      padding: 6px 0;
      text-align: left;
      font-size: 12px;
      color: var(--vscode-textLink-foreground, var(--accent));
    }
    button.link:hover {
      text-decoration: underline;
      background: transparent;
    }
    button.link-danger {
      color: var(--vscode-errorForeground, var(--warm));
      margin-left: auto;
    }
    .footer-links {
      display: flex;
      gap: 12px;
      margin-top: 16px;
      padding-top: 12px;
      border-top: 1px solid var(--border);
    }
  </style>
</head>
<body>
  ${bodyInner}

  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    document.querySelectorAll('[data-action]').forEach((button) => {
      button.addEventListener('click', () => {
        if (button.disabled) {
          return;
        }
        const message = { type: 'action', id: button.getAttribute('data-action') };
        const itemId = button.getAttribute('data-item');
        if (itemId) {
          message.itemId = itemId;
        }
        vscode.postMessage(message);
      });
    });
  </script>
</body>
</html>`;
}

/**
 * Render the exact production webview HTML for a given context, off the VS Code
 * host. Used by the panel simulator (tests/Playwright) so the real markup +
 * button wiring can be exercised in a plain browser. `logoSrc`/`cspSource`/
 * `nonce` are only string-interpolated by {@link panelHtml}, so stub values are
 * sufficient and faithful for everything except the live VS Code resource URIs.
 */
export function renderPanelHtml(context: PanelContext): string {
  const view = derivePanelView(context);
  return panelHtml(
    "media/logo.png" as unknown as vscode.Uri,
    "vscode-resource:",
    view,
    context,
    "simnonce",
  );
}

function getNonce(): string {
  const chars =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let nonce = "";
  for (let i = 0; i < 32; i += 1) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}

export class CognisPanelProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = "cognis.controlPanel";

  private view?: vscode.WebviewView;
  private context: PanelContext = {
    status: "unknown",
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  };

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly version?: string
  ) {
    this.context = {
      status: "unknown",
      compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
      version,
    };
  }

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;
    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, "media")],
    };

    webviewView.webview.onDidReceiveMessage(
      (message: { type?: string; id?: string; itemId?: string }) => {
        if (message.type !== "action" || !message.id) {
          return;
        }
        // Per-item prerequisite install carries the item id as a payload.
        if (message.id === "installPrerequisite" && message.itemId) {
          void vscode.commands.executeCommand(
            "cognis.installPrerequisite",
            message.itemId
          );
          return;
        }
        const command = ACTION_COMMANDS[message.id];
        if (!command) {
          return;
        }
        void vscode.commands.executeCommand(command);
      }
    );

    this.render();
  }

  updateContext(context: PanelContext): void {
    this.context = { ...context, version: context.version ?? this.version };
    this.render();
  }

  /** @deprecated Prefer updateContext — kept for callers passing status only. */
  updateStatus(status: WorkspaceStatus): void {
    this.updateContext({ ...this.context, status });
  }

  reveal(): void {
    void vscode.commands.executeCommand("cognis.controlPanel.focus");
  }

  private render(): void {
    if (!this.view) {
      return;
    }
    const nonce = getNonce();
    const logoSrc = this.view.webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, "media", "logo.png")
    );
    const view = derivePanelView(this.context);
    this.view.webview.html = panelHtml(
      logoSrc,
      this.view.webview.cspSource,
      view,
      this.context,
      nonce
    );
  }
}
