/**
 * `Toggle_Control` — one binary On/Off control per Canonical_Workspace.
 *
 * Spec: `.kiro/specs/cognis-one-toggle-engine` (Requirements 1.1, 1.2, 1.3,
 * 1.4, 1.5, 1.7, 1.8).
 *
 * `Toggle_State` is persisted in `context.workspaceState` under
 * {@link TOGGLE_STORAGE_KEY}, alongside the existing `cognis.workspaceState.v1`
 * record, keyed by the canonical workspace key so alias spellings collapse to
 * one entry. A single-slot runner per workspace holds an `AbortController`:
 * setting the current value is a no-op that re-reports status; a request that
 * arrives during an in-flight transition aborts it and converges on the newest
 * value.
 */

import * as fs from "node:fs";

import { canonicalizePath } from "./mcpCanonical";

/** Design: `export type ToggleState = "On" | "Off"`. */
export type ToggleState = "On" | "Off";

/** The default value of a workspace never toggled before. */
export const DEFAULT_TOGGLE_STATE: ToggleState = "Off";

/**
 * Sibling of `cognis.workspaceState.v1` on `context.workspaceState`.
 * Value is `{ [canonicalWorkspaceKey]: ToggleState }`.
 */
export const TOGGLE_STORAGE_KEY = "cognis.toggleState.v1";

/** How a toggle value arrived. Requirement 1.5 names all three. */
export type ToggleOrigin = "user" | "settingsSync" | "reopen";

/** The `workspaceState` subset Toggle_Control needs. */
export interface ToggleMemento {
  get<T>(key: string): T | undefined;
  update(key: string, value: unknown): Thenable<void> | Promise<void>;
}

/**
 * Turn-on / turn-off work driven by the control. `workspace` is the path
 * spelling passed to {@link ToggleControl.set} (the filesystem root), not the
 * canonical storage key — Activation_Manager needs the real repo root.
 */
export interface ToggleTransitionHandlers {
  activate(workspace: string, signal: AbortSignal): Promise<void>;
  deactivate(workspace: string, signal: AbortSignal): Promise<void>;
}

export interface ToggleControlOptions {
  workspaceState: ToggleMemento;
  transition: ToggleTransitionHandlers;
  /** Re-report the current `Status_State` (Requirement 1.7). */
  reportStatus: (workspace: string, displayed: ToggleState) => void;
}

interface ToggleSlot {
  persisted: ToggleState;
  inFlight?: ToggleState;
  controller?: AbortController;
  generation: number;
}

function isToggleState(value: unknown): value is ToggleState {
  return value === "On" || value === "Off";
}

function abortError(): Error {
  const error = new Error("Toggle transition aborted");
  error.name = "AbortError";
  return error;
}

/**
 * Canonical workspace key used as the persistence map key.
 *
 * Synthetic / not-yet-created paths (property tests, first enable) collapse by
 * spelling: long-path prefix strip, slash normalization, trailing-separator
 * removal, case folding. Real existing roots also go through
 * {@link canonicalizePath} so symlink aliases of a live workspace share one
 * entry (Requirement 1.4).
 */
export function toggleWorkspaceKey(spelling: string): string {
  const stripped = spelling
    .replace(/^\\\\\?\\UNC\\/i, "\\\\")
    .replace(/^\\\\\?\\/, "");
  try {
    if (fs.existsSync(stripped)) {
      return canonicalizePath(stripped);
    }
  } catch {
    // Fall through to spelling-only collapse.
  }
  return stripped
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();
}

/**
 * One Toggle_Control for the extension host. Holds the per-workspace runner
 * slots and the persisted map.
 */
export class ToggleControl {
  private readonly slots = new Map<string, ToggleSlot>();
  private readonly options: ToggleControlOptions;

  constructor(options: ToggleControlOptions) {
    this.options = options;
    this.hydrateFromStorage();
  }

  /** The value the control displays (in-flight target, else persisted). */
  displayed(workspace: string): ToggleState {
    const slot = this.slot(workspace);
    return slot.inFlight ?? slot.persisted;
  }

  /** The value written to `workspaceState`. */
  persisted(workspace: string): ToggleState {
    return this.slot(workspace).persisted;
  }

  /** True while a transition is executing for this workspace. */
  isTransitioning(workspace: string): boolean {
    return this.slot(workspace).inFlight !== undefined;
  }

  /**
   * Set the control to `value`.
   *
   * - Quiescent + already that value → no-op, re-report status (Req 1.7).
   * - Otherwise start (or redirect) the single-slot runner (Req 1.2, 1.3, 1.8).
   */
  async set(
    workspace: string,
    value: ToggleState,
    _origin: ToggleOrigin = "user"
  ): Promise<void> {
    const slot = this.slot(workspace);
    if (slot.inFlight === undefined && slot.persisted === value) {
      this.options.reportStatus(workspace, value);
      return;
    }
    if (slot.inFlight === value) {
      return;
    }

    slot.controller?.abort();
    const controller = new AbortController();
    const generation = slot.generation + 1;
    slot.generation = generation;
    slot.controller = controller;
    slot.inFlight = value;

    const key = toggleWorkspaceKey(workspace);
    const run =
      value === "On"
        ? this.options.transition.activate(workspace, controller.signal)
        : this.options.transition.deactivate(workspace, controller.signal);

    try {
      if (controller.signal.aborted) {
        throw abortError();
      }
      await run;
      if (slot.generation !== generation) {
        return;
      }
      slot.persisted = value;
      slot.inFlight = undefined;
      slot.controller = undefined;
      await this.writePersisted(key, value);
    } catch (error) {
      if (slot.generation !== generation || controller.signal.aborted) {
        return;
      }
      throw error;
    }
  }

  /**
   * Workspace reopen: the host is gone, so an un-settled transition does not
   * survive. Displayed value becomes the persisted value (Requirement 1.4).
   */
  reopen(workspace: string): void {
    const slot = this.slot(workspace);
    slot.controller?.abort();
    slot.controller = undefined;
    slot.inFlight = undefined;
    slot.generation += 1;
    this.options.reportStatus(workspace, slot.persisted);
  }

  /**
   * Drop in-memory slots and re-read `workspaceState`. Used after a simulated
   * host restart (new ToggleControl is preferred; this is for tests that reuse
   * the instance).
   */
  reloadFromStorage(): void {
    for (const slot of this.slots.values()) {
      slot.controller?.abort();
    }
    this.slots.clear();
    this.hydrateFromStorage();
  }

  private slot(workspace: string): ToggleSlot {
    const key = toggleWorkspaceKey(workspace);
    let slot = this.slots.get(key);
    if (!slot) {
      slot = {
        persisted: this.readPersisted(key),
        generation: 0,
      };
      this.slots.set(key, slot);
    }
    return slot;
  }

  private hydrateFromStorage(): void {
    const stored = this.options.workspaceState.get<Record<string, unknown>>(
      TOGGLE_STORAGE_KEY
    );
    if (!stored || typeof stored !== "object") {
      return;
    }
    for (const [key, value] of Object.entries(stored)) {
      if (!isToggleState(value)) {
        continue;
      }
      this.slots.set(key, { persisted: value, generation: 0 });
    }
  }

  private readPersisted(key: string): ToggleState {
    const stored = this.options.workspaceState.get<Record<string, unknown>>(
      TOGGLE_STORAGE_KEY
    );
    const value = stored?.[key];
    return isToggleState(value) ? value : DEFAULT_TOGGLE_STATE;
  }

  private async writePersisted(key: string, value: ToggleState): Promise<void> {
    const stored = {
      ...(this.options.workspaceState.get<Record<string, ToggleState>>(
        TOGGLE_STORAGE_KEY
      ) ?? {}),
    };
    stored[key] = value;
    await this.options.workspaceState.update(TOGGLE_STORAGE_KEY, stored);
  }
}

/** Instant-settling handlers for tests that do not drive the managers. */
export const pendingToggleManagers: ToggleTransitionHandlers = {
  async activate(_workspace, signal) {
    if (signal.aborted) {
      throw abortError();
    }
  },
  async deactivate(_workspace, signal) {
    if (signal.aborted) {
      throw abortError();
    }
  },
};
