import type * as vscode from "vscode";
import type { IndexStatusReport, WorkspaceStatus } from "./types";

export interface WorkspaceState {
  liveIndexing: boolean;
  mcpEnabled: boolean;
  autoManaged: boolean;
  lastHealth?: string;
  symbolCount?: number;
  indexStatus?: IndexStatusReport;
}

interface PersistedWorkspaceState {
  liveIndexing?: boolean;
  mcpEnabled?: boolean;
  autoManaged?: boolean;
  lastHealth?: string;
}

const STORAGE_KEY = "cognis.workspaceState.v1";

const states = new Map<string, WorkspaceState>();
let workspaceStorage: vscode.Memento | undefined;

function defaultState(): WorkspaceState {
  return { liveIndexing: false, mcpEnabled: false, autoManaged: false };
}

export function initStateStorage(context: vscode.ExtensionContext): void {
  workspaceStorage = context.workspaceState;
}

export function getWorkspaceKey(folder: string): string {
  return folder;
}

function readPersistedEntries(): Record<string, PersistedWorkspaceState> {
  return workspaceStorage?.get<Record<string, PersistedWorkspaceState>>(STORAGE_KEY) ?? {};
}

export function persistState(folder: string): void {
  if (!workspaceStorage) {
    return;
  }
  const state = getState(folder);
  const entries = readPersistedEntries();
  entries[getWorkspaceKey(folder)] = {
    liveIndexing: state.liveIndexing,
    mcpEnabled: state.mcpEnabled,
    autoManaged: state.autoManaged,
    lastHealth: state.lastHealth,
  };
  void workspaceStorage.update(STORAGE_KEY, entries);
}

export function loadPersistedState(folder: string): void {
  const entry = readPersistedEntries()[getWorkspaceKey(folder)];
  if (!entry) {
    return;
  }
  const state = getState(folder);
  if (entry.liveIndexing !== undefined) {
    state.liveIndexing = entry.liveIndexing;
  }
  if (entry.mcpEnabled !== undefined) {
    state.mcpEnabled = entry.mcpEnabled;
  }
  if (entry.autoManaged !== undefined) {
    state.autoManaged = entry.autoManaged;
  }
  if (entry.lastHealth !== undefined) {
    state.lastHealth = entry.lastHealth;
  }
}

export function getState(folder: string): WorkspaceState {
  const key = getWorkspaceKey(folder);
  if (!states.has(key)) {
    states.set(key, defaultState());
  }
  return states.get(key)!;
}

export function setLiveIndexing(folder: string, active: boolean): void {
  getState(folder).liveIndexing = active;
  persistState(folder);
}

export function setMcpEnabled(folder: string, enabled: boolean): void {
  getState(folder).mcpEnabled = enabled;
  persistState(folder);
}

export function setAutoManaged(folder: string, managed: boolean): void {
  getState(folder).autoManaged = managed;
  persistState(folder);
}

export function setLastHealth(folder: string, overall: string | undefined): void {
  const state = getState(folder);
  state.lastHealth = overall;
  persistState(folder);
}

export function setIndexStatus(
  folder: string,
  status: IndexStatusReport | undefined
): void {
  getState(folder).indexStatus = status;
}

export function isIndexStatusBusy(status: IndexStatusReport | undefined): boolean {
  if (!status?.active) {
    return false;
  }
  if (status.pendingCount > 0 || status.inflightCount > 0) {
    return true;
  }
  return !["watching", "idle", "stopped"].includes(status.phase);
}

export function deriveStatus(
  folder: string,
  healthOverall: string | undefined,
  /** True only while a blocking setup/sync operation is in progress — not live indexd. */
  operationInProgress: boolean
): WorkspaceStatus {
  const state = getState(folder);
  if (operationInProgress || isIndexStatusBusy(state.indexStatus)) {
    return "indexing";
  }
  if (healthOverall === "fail") {
    return "degraded";
  }
  if (state.mcpEnabled && healthOverall === "ok") {
    return "mcpEnabled";
  }
  if (healthOverall === "ok") {
    return "ready";
  }
  if (healthOverall === "warn") {
    return "degraded";
  }
  return "unknown";
}
