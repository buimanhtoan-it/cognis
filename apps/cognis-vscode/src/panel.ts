import * as vscode from "vscode";
import type {
  HealthReport,
  IndexStatusReport,
  PrerequisiteReport,
  WorkspaceStatus,
} from "./types";

const ACTION_COMMANDS: Record<string, string> = {
  setup: "cognis.setupForAi",
  repair: "cognis.repairSetup",
  clearReindex: "cognis.clearAndReindex",
  health: "cognis.showHealth",
  output: "cognis.showOutput",
  refreshPrerequisites: "cognis.refreshPrerequisites",
  installAllPrerequisites: "cognis.installAllPrerequisites",
  installBackend: "cognis.installBackend",
  remove: "cognis.removeFromWorkspace",
  prepareUninstall: "cognis.prepareUninstall",
};

export interface PanelContext {
  status: WorkspaceStatus;
  health?: HealthReport;
  liveIndexing?: boolean;
  mcpEnabled?: boolean;
  indexStatus?: IndexStatusReport;
  indexingMessage?: string;
  /** When health cannot be fetched but workspace was previously set up. */
  setupHint?: "python";
  /** Installable-prerequisite checklist (from `cognis-cli doctor`). */
  prerequisites?: PrerequisiteReport;
  /** True once the workspace has a `.cognis/config.yaml` (setup has run). */
  configured?: boolean;
  /**
   * Whether the Python backend (cognis CLI) could actually run. Undefined until
   * probed. False means the backend isn't installed/reachable yet — on a fresh
   * machine this is the first thing to fix, before any setup can succeed.
   */
  backendAvailable?: boolean;
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

  if (status === "indexing") {
    return {
      headline: deriveIndexingHeadline(ctx),
      detail: deriveIndexingDetail(ctx),
      statusClass: "status-active",
      primary: { id: "output", label: "View Output" },
    };
  }

  if (!health || status === "notInstalled") {
    if (ctx.setupHint === "python") {
      return {
        headline: "Reconnect the Cognis backend",
        detail:
          "Cognis is installed but couldn't start. This usually fixes itself — reinstall the backend in one click, or run Troubleshoot.",
        statusClass: "status-warn",
        primary: { id: "installBackend", label: "Reinstall backend" },
      };
    }
    // Fresh machine: the Python backend isn't installed yet, so `doctor` can't
    // even produce a checklist. Offer a one-click install instead of asking the
    // user to run pip and pick a Python environment by hand.
    if (ctx.backendAvailable === false && !ctx.prerequisites) {
      return {
        headline: "Install the Cognis backend",
        detail:
          "This extension is the control panel; the search engine is a small Python package. " +
          "Click Install backend and Cognis sets it up for you — no terminal, no setup.",
        statusClass: "status-warn",
        primary: { id: "installBackend", label: "Install backend" },
      };
    }
    // Gate setup on the prerequisite checklist: if a required component is
    // missing, point the user at the checklist instead of letting setup fail
    // partway through (and create a half-provisioned .cognis/).
    if (ctx.prerequisites && !ctx.prerequisites.ready) {
      return {
        headline: "Install prerequisites",
        detail:
          "Some required components are not installed yet. Use the checklist above to install them, then run Set Up for AI.",
        statusClass: "status-warn",
        primary: { id: "setup", label: "Set Up for AI", disabled: true },
      };
    }
    return {
      headline: "Setup required",
      detail:
        "Initialize Cognis for this workspace so your editor can search code semantically.",
      statusClass: "status-muted",
      primary: { id: "setup", label: "Set Up for AI" },
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

  if (mcpEnabled && liveIndexing && health.overall === "ok") {
    return {
      headline: "AI search ready",
      detail:
        "Semantic search and live indexing are active. Reload your MCP host if tools do not appear yet.",
      statusClass: "status-ok",
    };
  }

  if (health.overall === "ok" && !mcpEnabled) {
    return {
      headline: "Index ready",
      detail: "Connect Cognis to your MCP host to enable semantic search in chat.",
      statusClass: "status-ok",
      primary: { id: "setup", label: "Set Up for AI" },
    };
  }

  if (mcpEnabled && !liveIndexing) {
    return {
      headline: "Managed setup needs repair",
      detail:
        "MCP is configured, but live indexing is not active. Run Troubleshoot so Cognis can restore the workspace through the normal managed flow.",
      statusClass: "status-warn",
      primary: { id: "repair", label: "Troubleshoot" },
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
    primary: { id: "setup", label: "Set Up for AI" },
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
  if (ctx.status === "indexing") {
    return "$(sync~spin) Cognis: Indexing";
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
 *   ① Backend   → ② Components → ③ Index synced → ④ AI connected
 *
 * The steps are derived purely from the same context the panel already has, so
 * they never disagree with the status pill or the primary action.
 */
export function deriveSetupSteps(ctx: PanelContext): SetupStep[] {
  const { health, prerequisites, mcpEnabled, liveIndexing, status } = ctx;
  const configured = ctx.configured ?? false;
  const pythonBroken = ctx.setupHint === "python";
  const healthOk = health?.overall === "ok";

  // ① Backend (Python) usable.
  let backend: SetupStepState;
  if (pythonBroken || ctx.backendAvailable === false) {
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

  // ④ MCP tools connected to the editor.
  let connected: SetupStepState;
  if (indexed !== "done") {
    connected = "pending";
  } else if (mcpEnabled && liveIndexing) {
    connected = "done";
  } else if (mcpEnabled) {
    connected = "active";
  } else {
    connected = "active";
  }

  return [
    { id: "backend", label: "Backend", state: backend },
    { id: "components", label: "Components", state: components },
    { id: "indexed", label: "Index synced", state: indexed },
    { id: "connected", label: "AI connected", state: connected },
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
 * model of the path (Backend → Components → Index → AI) and where they are,
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
    : "Install the required components below before running Set Up for AI.";

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
    .step-active .step-marker { background: var(--accent); color: var(--button-fg); border-color: transparent; }
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
  <div class="hero">
    <img src="${logoSrc}" alt="Cognis logo" />
    <div class="hero-copy">
      <div class="title">Cognis</div>
      <div class="subtitle">Semantic index and MCP setup for AI tooling.</div>
    </div>
  </div>

  <div class="surface">
    <div class="status-overview">
      <div class="status-pill ${view.statusClass}">
        <span class="status-dot"></span>
        <span class="headline">${escapeHtml(view.headline)}</span>
      </div>
      <div class="status-copy">
        ${
          view.detail
            ? `<div class="detail">${escapeHtml(view.detail)}</div>`
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

  ${renderStepperSection(context)}

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
        <button data-action="clearReindex" title="Delete the stored index and rebuild from scratch. Keeps your config and MCP wiring.">Rebuild index</button>
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
        "No files are waiting in the debounce queue."
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
      <div class="advanced-label">Remove Cognis</div>
      <div class="link-actions">
        <button class="link link-danger" data-action="remove" title="Stop indexing, disconnect MCP for this repo, and delete the local .cognis index for this workspace.">Remove from this workspace</button>
        <button class="link link-danger" data-action="prepareUninstall" title="Stop indexing, delete this workspace's .cognis index, remove ALL cognis MCP entries from your editor, and uninstall the Cognis backend Cognis installed. Run this before uninstalling the extension.">Remove everything (prepare to uninstall)</button>
      </div>
      <div class="surface-detail">Your source code is never touched. "Remove everything" also uninstalls the backend Cognis installed for you.</div>
    </div>
  </details>

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
  private context: PanelContext = { status: "unknown" };

  constructor(private readonly extensionUri: vscode.Uri) {}

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
    this.context = context;
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
