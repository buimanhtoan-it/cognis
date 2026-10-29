import * as fs from "node:fs";
import * as path from "node:path";
import { trace } from "./diagnostics";
import { readLease, type LeaseRecord } from "./lease";
import type {
  Capabilities,
  HealthReport,
  OwnerStatusReport,
  StatusReport,
  WorkspaceStatus,
} from "./types";

export type TransitionPhase =
  | "verify_engine_binary"
  | "verify_model_assets"
  | "open_database"
  | "reconcile_and_index"
  | "start_watcher"
  | "write_mcp_entry"
  | "stop_watcher"
  | "stop_daemons"
  | "remove_mcp_entry";

export interface StatusInput {
  health?: HealthReport;
  owner?: OwnerStatusReport;
  lease?: LeaseRecord;
  transition?: TransitionPhase;
  readErrors: string[];
}

export interface StatusSnapshot {
  input: StatusInput;
  report: StatusReport;
  workspaceStatus: WorkspaceStatus;
}

interface DeriveStatusOptions {
  repoRoot: string;
  health?: HealthReport;
  healthReadFailed?: boolean;
  mcpEnabled: boolean;
  configured: boolean;
  transition?: TransitionPhase;
  /** Test seam: freeze the 5 s unreadability budget clock. */
  nowMs?: number;
}

interface ReadResult<T> {
  value?: T;
  failed: boolean;
}

type StatusSource = "health" | "owner-status.json" | "owner.lease";

const OWNER_STATUS_FILE = "owner-status.json";
const OWNER_LEASE_FILE = "owner.lease";
const STATUS_READ_BUDGET_MS = 5000;
const STATUS_POLL_CEILING_MS = 2000;
const UNREADABLE_ACTION = {
  title: "Show diagnostics",
  command: "cognis.showDiagnostics",
} as const;
const BLOCKING_ACTIONS = {
  missing_required_index: {
    title: "Rebuild Index",
    command: "cognis.clearAndReindex",
  },
  checksum_verification_failed: {
    title: "Retry download",
    command: "cognis.installBackend",
  },
  database_unwritable: {
    title: "Repair setup",
    command: "cognis.repairSetup",
  },
  lexical_retrieval_unavailable: {
    title: "Repair setup",
    command: "cognis.repairSetup",
  },
} as const;
const WORKING_OWNER_PHASES = new Set([
  "starting",
  "provisioning",
  "indexing",
  "embedding",
  "incremental",
  "cold_index",
  "rebuild",
  "sweep",
  "branch_change",
]);
const PHASE_LABELS: Record<string, string> = {
  verify_engine_binary: "verify engine binary",
  verify_model_assets: "verify model assets",
  open_database: "open database",
  reconcile_and_index: "reconcile and index",
  start_watcher: "start watcher",
  write_mcp_entry: "write MCP entry",
  stop_watcher: "stop watcher",
  stop_daemons: "stop daemons",
  remove_mcp_entry: "remove MCP entry",
  starting: "starting",
  provisioning: "provisioning",
  indexing: "indexing",
  embedding: "embedding",
  incremental: "incremental indexing",
  cold_index: "cold index",
  rebuild: "rebuild",
  sweep: "reconciliation sweep",
  branch_change: "branch change",
};

const failureSinceByRepo = new Map<string, Map<StatusSource, number>>();
const lastReportByRepo = new Map<string, StatusReport>();

function repoStatusDir(repoRoot: string): string {
  return path.join(repoRoot, ".cognis");
}

function ownerStatusPath(repoRoot: string): string {
  return path.join(repoStatusDir(repoRoot), OWNER_STATUS_FILE);
}

function ownerLeasePath(repoRoot: string): string {
  return path.join(repoStatusDir(repoRoot), OWNER_LEASE_FILE);
}

function normalizedReadErrors(input: StatusInput): string[] {
  return [...new Set(input.readErrors.filter((value) => value.trim().length > 0))];
}

function lexicalServing(input: StatusInput): boolean {
  const checks = input.health?.checks;
  return (
    checks?.config?.status === "ok" &&
    checks.db?.status === "ok" &&
    checks.index?.status === "ok"
  );
}

function coalesceCapabilities(input: StatusInput): Capabilities | undefined {
  return input.health?.capabilities ?? input.owner?.capabilities;
}

function coalesceSourceGeneration(input: StatusInput): number | undefined {
  return input.health?.source_generation ?? input.owner?.sourceGeneration;
}

function coalesceSemanticGeneration(input: StatusInput): number | undefined {
  return input.health?.semantic_generation ?? input.owner?.semanticGeneration;
}

function coalescePendingVectorCount(input: StatusInput): number | undefined {
  return input.health?.pending_vector_count ?? input.owner?.pendingVectorCount;
}

function coalesceSemanticAvailable(input: StatusInput): boolean | undefined {
  return input.health?.semantic_available;
}

function coalesceFingerprintMatch(input: StatusInput): boolean | undefined {
  return input.health?.fingerprint_match ?? input.owner?.fingerprintMatch;
}

function coalesceObservedAt(input: StatusInput): number {
  if (typeof input.owner?.updatedAt === "number") {
    return input.owner.updatedAt;
  }
  if (typeof input.lease?.heartbeat_at === "number") {
    return input.lease.heartbeat_at;
  }
  return 0;
}

function queueDepths(input: StatusInput): {
  watcherQueueDepth: number;
  embeddingQueueDepth: number;
} {
  return {
    watcherQueueDepth: Math.max(0, input.owner?.watcherQueueDepth ?? 0),
    embeddingQueueDepth: Math.max(0, coalescePendingVectorCount(input) ?? 0),
  };
}

function phaseLabel(raw: string | undefined): string {
  if (!raw) {
    return "working";
  }
  return PHASE_LABELS[raw] ?? raw.replace(/_/g, " ");
}

function buildDerivedFrom(input: StatusInput): StatusReport["derivedFrom"] {
  return {
    sourceGeneration: coalesceSourceGeneration(input),
    semanticGeneration: coalesceSemanticGeneration(input),
    pendingVectorCount: coalescePendingVectorCount(input),
    semanticAvailable: coalesceSemanticAvailable(input),
    fingerprintMatch: coalesceFingerprintMatch(input),
    capabilities: coalesceCapabilities(input),
  };
}

function baseReport(state: StatusReport["state"], detail: string, input: StatusInput): StatusReport {
  return {
    state,
    detail,
    derivedFrom: buildDerivedFrom(input),
    observedAt: coalesceObservedAt(input),
  };
}

function firstMatchingMessage(
  messages: Array<string | undefined>,
  pattern: RegExp
): string | undefined {
  return messages.find((message) => typeof message === "string" && pattern.test(message));
}

function blockingCondition(input: StatusInput): {
  label: string;
  action: { title: string; command: string };
} | undefined {
  const missingIndexes = input.health?.missing_indexes ?? [];
  if (missingIndexes.length > 0) {
    return {
      label: `Missing required index: ${missingIndexes.join(", ")}`,
      action: { ...BLOCKING_ACTIONS.missing_required_index },
    };
  }

  const checksumMessage = firstMatchingMessage(
    [
      input.owner?.message,
      input.owner?.lastError,
      ...Object.values(input.health?.checks ?? {}).map((check) => check.message),
    ],
    /checksum/i
  );
  if (checksumMessage) {
    return {
      label: "Checksum verification failed",
      action: { ...BLOCKING_ACTIONS.checksum_verification_failed },
    };
  }

  const dbMessage = input.health?.checks?.db?.message;
  if (
    input.health?.checks?.db?.status === "fail" &&
    typeof dbMessage === "string" &&
    /(not writable|read-only|readonly)/i.test(dbMessage)
  ) {
    return {
      label: "Workspace database is not writable",
      action: { ...BLOCKING_ACTIONS.database_unwritable },
    };
  }

  return undefined;
}

function workingPhase(input: StatusInput): string | undefined {
  if (input.transition) {
    return input.transition;
  }
  const ownerPhase = input.owner?.phase;
  if (ownerPhase && WORKING_OWNER_PHASES.has(ownerPhase)) {
    return ownerPhase;
  }
  return undefined;
}

function degradedDetail(input: StatusInput): string {
  const capabilities = coalesceCapabilities(input);
  const pendingVectorCount = coalescePendingVectorCount(input) ?? 0;
  const sourceGeneration = coalesceSourceGeneration(input);
  const semanticGeneration = coalesceSemanticGeneration(input);
  const fingerprintMatch = coalesceFingerprintMatch(input);
  const semanticAvailable = coalesceSemanticAvailable(input);

  if (!capabilities?.semantic_backend) {
    return "Lexical search is ready; semantic backend is unavailable.";
  }
  if (pendingVectorCount > 0) {
    return `Lexical search is ready; ${pendingVectorCount} embeddings are still pending.`;
  }
  if (
    sourceGeneration !== undefined &&
    semanticGeneration !== undefined &&
    semanticGeneration < sourceGeneration
  ) {
    return "Lexical search is ready; semantic generation is still catching up.";
  }
  if (fingerprintMatch === false) {
    return "Lexical search is ready; stored vectors were refused because the vector space fingerprint does not match.";
  }
  if (semanticAvailable === false) {
    return "Lexical search is ready; semantic search is temporarily unavailable.";
  }
  if (capabilities.semantic_backend && !capabilities.accelerated_vector_search) {
    return "Lexical search is ready; vector search running in fallback mode.";
  }
  return "Lexical search is ready; semantic search is temporarily degraded.";
}

export function deriveStatusState(input: StatusInput): StatusReport {
  const readErrors = normalizedReadErrors(input);
  if (readErrors.length > 0) {
    const report = baseReport(
      "Needs_Attention",
      `Status data is unreadable: ${readErrors.join(", ")}.`,
      input
    );
    report.blockingCondition = `Unreadable status sources: ${readErrors.join(", ")}`;
    report.nextAction = { ...UNREADABLE_ACTION };
    return report;
  }

  const blocking = blockingCondition(input);
  if (blocking) {
    const report = baseReport(
      "Needs_Attention",
      `Status needs attention: ${blocking.label}.`,
      input
    );
    report.blockingCondition = blocking.label;
    report.nextAction = blocking.action;
    return report;
  }

  const phase = workingPhase(input);
  if (phase) {
    const queues = queueDepths(input);
    const report = baseReport(
      "Working",
      `Working: ${phaseLabel(phase)} (watcher queue ${queues.watcherQueueDepth}, embedding queue ${queues.embeddingQueueDepth}).`,
      input
    );
    report.phase = phaseLabel(phase);
    report.watcherQueueDepth = queues.watcherQueueDepth;
    report.embeddingQueueDepth = queues.embeddingQueueDepth;
    return report;
  }

  const capabilities = coalesceCapabilities(input);
  const semanticAvailable = coalesceSemanticAvailable(input) === true;
  const pendingVectorCount = coalescePendingVectorCount(input) ?? 0;
  const sourceGeneration = coalesceSourceGeneration(input);
  const semanticGeneration = coalesceSemanticGeneration(input);
  const fingerprintMatch = coalesceFingerprintMatch(input) === true;

  if (
    lexicalServing(input) &&
    semanticAvailable &&
    pendingVectorCount === 0 &&
    sourceGeneration !== undefined &&
    semanticGeneration !== undefined &&
    semanticGeneration === sourceGeneration &&
    fingerprintMatch &&
    capabilities?.semantic_backend === true &&
    capabilities?.accelerated_vector_search === true
  ) {
    return baseReport(
      "Ready_Full",
      "Lexical and semantic search are ready.",
      input
    );
  }

  if (lexicalServing(input)) {
    return baseReport("Ready_Lexical", degradedDetail(input), input);
  }

  const report = baseReport(
    "Needs_Attention",
    "Status needs attention: lexical retrieval is not serving the committed generation.",
    input
  );
  report.blockingCondition = "Lexical retrieval unavailable";
  report.nextAction = { ...BLOCKING_ACTIONS.lexical_retrieval_unavailable };
  return report;
}

function failureMapForRepo(repoRoot: string): Map<StatusSource, number> {
  let current = failureSinceByRepo.get(repoRoot);
  if (!current) {
    current = new Map<StatusSource, number>();
    failureSinceByRepo.set(repoRoot, current);
  }
  return current;
}

function trackReadFailure(
  repoRoot: string,
  source: StatusSource,
  failed: boolean,
  now: number
): boolean {
  const failures = failureMapForRepo(repoRoot);
  if (!failed) {
    failures.delete(source);
    return false;
  }
  const since = failures.get(source) ?? now;
  failures.set(source, since);
  return now - since >= STATUS_READ_BUDGET_MS;
}

function readOwnerStatus(repoRoot: string): ReadResult<OwnerStatusReport> {
  const file = ownerStatusPath(repoRoot);
  if (!fs.existsSync(file)) {
    return { failed: false };
  }
  try {
    const raw = fs.readFileSync(file, "utf8").trim();
    if (!raw) {
      return { failed: true };
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (!parsed || typeof parsed !== "object") {
      return { failed: true };
    }
    return {
      failed: false,
      value: {
        pid: typeof parsed.pid === "number" ? parsed.pid : undefined,
        active: Boolean(parsed.active),
        phase: typeof parsed.phase === "string" ? parsed.phase : "idle",
        message: typeof parsed.message === "string" ? parsed.message : "",
        progressPercent:
          typeof parsed.progressPercent === "number"
            ? parsed.progressPercent
            : typeof parsed.progress_percent === "number"
              ? parsed.progress_percent
              : undefined,
        pendingCount:
          typeof parsed.pendingCount === "number"
            ? parsed.pendingCount
            : typeof parsed.pending_count === "number"
              ? parsed.pending_count
              : 0,
        pendingFiles: Array.isArray(parsed.pendingFiles)
          ? parsed.pendingFiles.filter((value): value is string => typeof value === "string")
          : Array.isArray(parsed.pending_files)
            ? parsed.pending_files.filter((value): value is string => typeof value === "string")
            : [],
        inflightCount:
          typeof parsed.inflightCount === "number"
            ? parsed.inflightCount
            : typeof parsed.inflight_count === "number"
              ? parsed.inflight_count
              : 0,
        inflightFiles: Array.isArray(parsed.inflightFiles)
          ? parsed.inflightFiles.filter((value): value is string => typeof value === "string")
          : Array.isArray(parsed.inflight_files)
            ? parsed.inflight_files.filter((value): value is string => typeof value === "string")
            : [],
        recentFiles: Array.isArray(parsed.recentFiles)
          ? parsed.recentFiles.filter((value): value is string => typeof value === "string")
          : Array.isArray(parsed.recent_files)
            ? parsed.recent_files.filter((value): value is string => typeof value === "string")
            : [],
        updatedAt:
          typeof parsed.updatedAt === "number"
            ? parsed.updatedAt
            : typeof parsed.updated_at === "number"
              ? parsed.updated_at
              : 0,
        lastError:
          typeof parsed.lastError === "string"
            ? parsed.lastError
            : typeof parsed.last_error === "string"
              ? parsed.last_error
              : undefined,
        sourceGeneration:
          typeof parsed.sourceGeneration === "number" ? parsed.sourceGeneration : undefined,
        semanticGeneration:
          typeof parsed.semanticGeneration === "number" ? parsed.semanticGeneration : undefined,
        pendingVectorCount:
          typeof parsed.pendingVectorCount === "number" ? parsed.pendingVectorCount : undefined,
        watcherQueueDepth:
          typeof parsed.watcherQueueDepth === "number" ? parsed.watcherQueueDepth : undefined,
        modelResident:
          typeof parsed.modelResident === "boolean" ? parsed.modelResident : undefined,
        fingerprintMatch:
          typeof parsed.fingerprintMatch === "boolean" ? parsed.fingerprintMatch : undefined,
        capabilities:
          typeof parsed.capabilities === "object" && parsed.capabilities !== null
            ? {
                semantic_backend: Boolean(
                  (parsed.capabilities as Record<string, unknown>).semantic_backend
                ),
                accelerated_vector_search: Boolean(
                  (parsed.capabilities as Record<string, unknown>)
                    .accelerated_vector_search
                ),
              }
            : undefined,
        canonicalWorkspace:
          typeof parsed.canonicalWorkspace === "string"
            ? parsed.canonicalWorkspace
            : undefined,
      },
    };
  } catch {
    return { failed: true };
  }
}

function readOwnerLease(repoRoot: string): ReadResult<LeaseRecord> {
  const file = ownerLeasePath(repoRoot);
  if (!fs.existsSync(file)) {
    return { failed: false };
  }
  const lease = readLease(repoRoot, "owner");
  if (!lease) {
    return { failed: true };
  }
  return { failed: false, value: lease };
}

function logStatusTransition(repoRoot: string, report: StatusReport): void {
  const previous = lastReportByRepo.get(repoRoot);
  if (
    previous &&
    previous.state === report.state &&
    previous.detail === report.detail &&
    previous.phase === report.phase &&
    previous.watcherQueueDepth === report.watcherQueueDepth &&
    previous.embeddingQueueDepth === report.embeddingQueueDepth &&
    previous.blockingCondition === report.blockingCondition &&
    previous.nextAction?.title === report.nextAction?.title &&
    previous.nextAction?.command === report.nextAction?.command
  ) {
    return;
  }
  lastReportByRepo.set(repoRoot, report);
  trace.info("status", "Status surface transition", {
    repoRoot,
    state: report.state,
    detail: report.detail,
    phase: report.phase,
    watcherQueueDepth: report.watcherQueueDepth,
    embeddingQueueDepth: report.embeddingQueueDepth,
    blockingCondition: report.blockingCondition,
    nextAction: report.nextAction,
    derivedFrom: report.derivedFrom,
    observedAt: report.observedAt,
  });
}

export function deriveStatusSnapshot(options: DeriveStatusOptions): StatusSnapshot {
  const now = options.nowMs ?? Date.now();
  const owner = readOwnerStatus(options.repoRoot);
  const lease = readOwnerLease(options.repoRoot);
  const readErrors: string[] = [];

  if (trackReadFailure(options.repoRoot, "health", options.healthReadFailed === true, now)) {
    readErrors.push("health");
  }
  if (trackReadFailure(options.repoRoot, "owner-status.json", owner.failed, now)) {
    readErrors.push("owner-status.json");
  }
  if (trackReadFailure(options.repoRoot, "owner.lease", lease.failed, now)) {
    readErrors.push("owner.lease");
  }

  const input: StatusInput = {
    health: options.health,
    owner: owner.value,
    lease: lease.value,
    transition: options.transition,
    readErrors,
  };
  const report = deriveStatusState(input);
  logStatusTransition(options.repoRoot, report);

  let workspaceStatus: WorkspaceStatus;
  if (!options.configured) {
    workspaceStatus = "notInstalled";
  } else if (
    options.health === undefined &&
    report.state === "Needs_Attention" &&
    report.blockingCondition === "Lexical retrieval unavailable" &&
    readErrors.length === 0
  ) {
    workspaceStatus = "unknown";
  } else {
    switch (report.state) {
      case "Working":
        workspaceStatus = "indexing";
        break;
      case "Needs_Attention":
        workspaceStatus = "degraded";
        break;
      case "Ready_Full":
      case "Ready_Lexical":
      default:
        workspaceStatus = options.mcpEnabled ? "mcpEnabled" : "ready";
        break;
    }
  }

  return { input, report, workspaceStatus };
}

export function deriveStatus(
  repoRoot: string,
  health: HealthReport | undefined,
  options: {
    mcpEnabled: boolean;
    configured: boolean;
    transition?: TransitionPhase;
    healthReadFailed?: boolean;
  }
): WorkspaceStatus {
  return deriveStatusSnapshot({
    repoRoot,
    health,
    healthReadFailed: options.healthReadFailed,
    mcpEnabled: options.mcpEnabled,
    configured: options.configured,
    transition: options.transition,
  }).workspaceStatus;
}

function statusSignature(repoRoot: string): string {
  const files = [ownerStatusPath(repoRoot), ownerLeasePath(repoRoot)];
  return files
    .map((file) => {
      try {
        const stat = fs.statSync(file);
        return `${path.basename(file)}:${stat.mtimeMs}:${stat.size}`;
      } catch {
        return `${path.basename(file)}:missing`;
      }
    })
    .join("|");
}

export function startStatusSurfaceRefresh(options: {
  repoRoot: string;
  onDidChange: () => void;
}): { dispose(): void } {
  let disposed = false;
  let signature = statusSignature(options.repoRoot);
  let watcher: fs.FSWatcher | undefined;

  const maybeNotify = (): void => {
    if (disposed) {
      return;
    }
    const next = statusSignature(options.repoRoot);
    if (next === signature) {
      return;
    }
    signature = next;
    options.onDidChange();
  };

  const interval = setInterval(maybeNotify, STATUS_POLL_CEILING_MS);
  const dir = repoStatusDir(options.repoRoot);
  if (fs.existsSync(dir)) {
    try {
      watcher = fs.watch(dir, (_eventType, fileName) => {
        const observed = typeof fileName === "string" ? fileName : "";
        if (
          observed &&
          observed !== OWNER_STATUS_FILE &&
          observed !== OWNER_LEASE_FILE &&
          !observed.startsWith(OWNER_STATUS_FILE) &&
          !observed.startsWith(OWNER_LEASE_FILE)
        ) {
          return;
        }
        maybeNotify();
      });
    } catch (err) {
      trace.warn("status", "Could not watch status files; bounded poll remains active", {
        repoRoot: options.repoRoot,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return {
    dispose(): void {
      disposed = true;
      clearInterval(interval);
      watcher?.close();
      failureSinceByRepo.delete(options.repoRoot);
      lastReportByRepo.delete(options.repoRoot);
    },
  };
}

export const statusReadBudgetMs = STATUS_READ_BUDGET_MS;
export const statusPollCeilingMs = STATUS_POLL_CEILING_MS;
