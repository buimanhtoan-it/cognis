import assert from "node:assert/strict";
import test from "node:test";

import {
  DEFAULT_TOGGLE_STATE,
  TOGGLE_STORAGE_KEY,
  ToggleControl,
  pendingToggleManagers,
  toggleWorkspaceKey,
  type ToggleMemento,
  type ToggleState,
  type ToggleTransitionHandlers,
} from "../toggle";
import {
  OTHER_WORKSPACE,
  PRIMARY_WORKSPACE_KEY,
  WORKSPACE_ALIASES,
} from "./generators";

class MemoryMemento implements ToggleMemento {
  private readonly store = new Map<string, unknown>();

  get<T>(key: string): T | undefined {
    return this.store.get(key) as T | undefined;
  }

  update(key: string, value: unknown): Promise<void> {
    this.store.set(key, value);
    return Promise.resolve();
  }
}

function deferred(): {
  promise: Promise<void>;
  resolve: () => void;
} {
  let resolve!: () => void;
  const promise = new Promise<void>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function abortError(): Error {
  const error = new Error("aborted");
  error.name = "AbortError";
  return error;
}

function gatedHandlers(activateGate: Promise<void>): ToggleTransitionHandlers {
  return {
    async activate(_key, signal) {
      if (signal.aborted) {
        throw abortError();
      }
      await Promise.race([
        activateGate,
        new Promise<void>((_, reject) => {
          signal.addEventListener("abort", () => reject(abortError()), {
            once: true,
          });
        }),
      ]);
    },
    async deactivate(_key, signal) {
      if (signal.aborted) {
        throw abortError();
      }
    },
  };
}

function controlWith(
  memento: ToggleMemento,
  extras?: {
    transition?: ToggleTransitionHandlers;
    reports?: Array<{ workspace: string; displayed: ToggleState }>;
  }
): ToggleControl {
  const reports = extras?.reports ?? [];
  return new ToggleControl({
    workspaceState: memento,
    transition: extras?.transition ?? pendingToggleManagers,
    reportStatus: (workspace, displayed) => {
      reports.push({ workspace, displayed });
    },
  });
}

test("alias spellings collapse to one canonical key", () => {
  const keys = new Set(WORKSPACE_ALIASES.map(toggleWorkspaceKey));
  assert.equal(keys.size, 1);
  assert.equal(toggleWorkspaceKey(WORKSPACE_ALIASES[0]), PRIMARY_WORKSPACE_KEY);
});

test("one binary control per workspace: two workspaces stay independent", async () => {
  const control = controlWith(new MemoryMemento());
  await control.set(WORKSPACE_ALIASES[0], "On");
  assert.equal(control.displayed(WORKSPACE_ALIASES[1]), "On");
  assert.equal(control.displayed(OTHER_WORKSPACE), DEFAULT_TOGGLE_STATE);
  await control.set(OTHER_WORKSPACE, "On");
  assert.equal(control.persisted(WORKSPACE_ALIASES[2]), "On");
  assert.equal(control.persisted(OTHER_WORKSPACE), "On");
});

test("setting the current value is a no-op that re-reports status", async () => {
  const reports: Array<{ workspace: string; displayed: ToggleState }> = [];
  let activateCalls = 0;
  const control = controlWith(new MemoryMemento(), {
    reports,
    transition: {
      async activate() {
        activateCalls += 1;
      },
      async deactivate() {},
    },
  });
  await control.set(WORKSPACE_ALIASES[0], "Off");
  assert.equal(activateCalls, 0);
  assert.equal(reports.length, 1);
  assert.equal(reports[0]?.displayed, "Off");
  assert.equal(control.isTransitioning(WORKSPACE_ALIASES[0]), false);
});

test("a change persists only after the transition settles", async () => {
  const gate = deferred();
  const memento = new MemoryMemento();
  const control = controlWith(memento, { transition: gatedHandlers(gate.promise) });
  const pending = control.set(WORKSPACE_ALIASES[0], "On");
  assert.equal(control.displayed(WORKSPACE_ALIASES[1]), "On");
  assert.equal(control.persisted(WORKSPACE_ALIASES[2]), "Off");
  assert.equal(control.isTransitioning(WORKSPACE_ALIASES[0]), true);
  assert.equal(memento.get<Record<string, ToggleState>>(TOGGLE_STORAGE_KEY), undefined);
  gate.resolve();
  await pending;
  assert.equal(control.persisted(WORKSPACE_ALIASES[3]), "On");
  assert.equal(control.isTransitioning(WORKSPACE_ALIASES[0]), false);
  assert.equal(
    memento.get<Record<string, ToggleState>>(TOGGLE_STORAGE_KEY)?.[PRIMARY_WORKSPACE_KEY],
    "On"
  );
});

test("a mid-transition request aborts and converges on the newest value", async () => {
  const gate = deferred();
  const control = controlWith(new MemoryMemento(), {
    transition: gatedHandlers(gate.promise),
  });
  const first = control.set(WORKSPACE_ALIASES[0], "On");
  const second = control.set(WORKSPACE_ALIASES[1], "Off");
  gate.resolve();
  await first;
  await second;
  assert.equal(control.displayed(WORKSPACE_ALIASES[2]), "Off");
  assert.equal(control.persisted(WORKSPACE_ALIASES[3]), "Off");
  assert.equal(control.isTransitioning(WORKSPACE_ALIASES[4]), false);
});

test("reopen discards in-flight work and restores the persisted value", async () => {
  const memento = new MemoryMemento();
  const settled = controlWith(memento);
  await settled.set(WORKSPACE_ALIASES[0], "On");

  const gate = deferred();
  const control = controlWith(memento, { transition: gatedHandlers(gate.promise) });
  const pending = control.set(WORKSPACE_ALIASES[0], "Off");
  assert.equal(control.displayed(WORKSPACE_ALIASES[1]), "Off");
  assert.equal(control.persisted(WORKSPACE_ALIASES[1]), "On");
  control.reopen(WORKSPACE_ALIASES[2]);
  await pending;
  assert.equal(control.displayed(WORKSPACE_ALIASES[3]), "On");
  assert.equal(control.persisted(WORKSPACE_ALIASES[4]), "On");
  assert.equal(control.isTransitioning(WORKSPACE_ALIASES[0]), false);

  const restored = controlWith(memento);
  assert.equal(restored.displayed(WORKSPACE_ALIASES[0]), "On");
  assert.equal(restored.persisted(OTHER_WORKSPACE), "Off");
});

test("activate receives the filesystem path spelling, not the canonical key", async () => {
  const seen: string[] = [];
  const control = controlWith(new MemoryMemento(), {
    transition: {
      async activate(workspace) {
        seen.push(workspace);
      },
      async deactivate() {},
    },
  });
  const spelling = WORKSPACE_ALIASES[4];
  await control.set(spelling, "On");
  assert.deepEqual(seen, [spelling]);
  assert.notEqual(seen[0], PRIMARY_WORKSPACE_KEY);
});
