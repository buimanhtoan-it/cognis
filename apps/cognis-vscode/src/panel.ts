import * as vscode from "vscode";
import type {
  HealthReport,
  IndexStatusReport,
  WorkspaceStatus,
} from "./types";

const ACTION_COMMANDS: Record<string, string> = {
  setup: "cognis.setupForAi",
  repair: "cognis.repairSetup",
  clearReindex: "cognis.clearAndReindex",
  health: "cognis.showHealth",
  output: "cognis.showOutput",
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
        headline: "Fix Python interpreter",
        detail:
          "Cognis is installed but the CLI could not run. Select the Python environment where cognis is installed, or set cognis.pythonPath.",
        statusClass: "status-warn",
        primary: { id: "repair", label: "Repair Setup" },
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
      primary: { id: "repair", label: "Repair Setup" },
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
        "MCP is configured, but live indexing is not active. Run Repair Setup so Cognis can restore the workspace through the normal managed flow.",
      statusClass: "status-warn",
      primary: { id: "repair", label: "Repair Setup" },
    };
  }

  if (health.overall === "warn" || status === "degraded") {
    const warning = firstWarningCheck(health);
    return {
      headline: warning ? issueHeadline(warning.name) : "Needs attention",
      detail: warning?.message ?? "Some checks reported warnings.",
      statusClass: "status-warn",
      primary: { id: "repair", label: "Repair Setup" },
    };
  }

  return {
    headline: "Checking status…",
    detail: "Waiting for workspace health information.",
    statusClass,
    primary: { id: "setup", label: "Set Up for AI" },
  };
}

/** Short label for the status bar — aligned with panel headlines. */
export function outcomeLabelForContext(ctx: PanelContext): string {
  const view = derivePanelView(ctx);
  let icon = "$(question)";
  if (ctx.status === "indexing") {
    icon = "$(sync~spin)";
  } else if (view.statusClass === "status-ok") {
    icon = ctx.mcpEnabled ? "$(plug)" : "$(check)";
  } else if (view.statusClass === "status-warn") {
    icon = "$(warning)";
  } else if (ctx.status === "notInstalled") {
    icon = "$(circle-slash)";
  }
  return `${icon} ${view.headline}`;
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

  <div class="surface">
    <div class="surface-header">
      <div>
        <div class="surface-title">Index Status</div>
        <div class="surface-detail">
          Track what Cognis is indexing now. Setup and repair manage live indexing automatically when the workspace needs it.
        </div>
      </div>
      <div class="surface-actions">
        <button data-action="clearReindex" title="Delete the stored index and rebuild from scratch. Keeps your config and MCP wiring.">Clear &amp; Re-index</button>
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

  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    document.querySelectorAll('[data-action]').forEach((button) => {
      button.addEventListener('click', () => {
        if (button.disabled) {
          return;
        }
        vscode.postMessage({ type: 'action', id: button.getAttribute('data-action') });
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

    webviewView.webview.onDidReceiveMessage((message: { type?: string; id?: string }) => {
      if (message.type !== "action" || !message.id) {
        return;
      }
      const command = ACTION_COMMANDS[message.id];
      if (!command) {
        return;
      }
      if (command === "cognis.showOutput") {
        void vscode.commands.executeCommand("cognis.showOutput");
        return;
      }
      void vscode.commands.executeCommand(command);
    });

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
