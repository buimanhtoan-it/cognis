/**
 * Property 28: Toggle convergence and persistence (task 16.9).
 *
 * For any sequence from `arb_toggle_event_sequence`, `ToggleControl` matches
 * the independent {@link applyToggleEvents} reference model:
 *
 * - applying the current value while quiescent is a no-op (Req 1.7);
 * - a mid-transition request cancels and converges on the newest value (1.8);
 * - displayed equals persisted at every quiescent point (1.5);
 * - reopen restores the persisted value, and alias spellings share one entry (1.4).
 *
 * Validates: Requirements 1.4, 1.5, 1.7, 1.8
 * Properties: P28
 */
import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  TOGGLE_STORAGE_KEY,
  ToggleControl,
  toggleWorkspaceKey,
  type ToggleMemento,
  type ToggleOrigin,
  type ToggleState,
} from "../toggle";
import {
  PRIMARY_WORKSPACE_KEY,
  PROPERTY_RUNS,
  WORKSPACE_ALIASES,
  applyToggleEvents,
  arbInterruptedToggleSequence,
  arbToggleEventSequence,
  displayedValue,
  quiescentWorkspaces,
  type ToggleEvent,
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

function abortError(): Error {
  const error = new Error("Toggle transition aborted");
  error.name = "AbortError";
  return error;
}

class WorkspaceGate {
  private resolve: () => void = () => undefined;
  private pending: Promise<void>;

  constructor() {
    this.pending = this.newPending();
  }

  private newPending(): Promise<void> {
    return new Promise<void>((resolve) => {
      this.resolve = resolve;
    });
  }

  wait(signal: AbortSignal): Promise<void> {
    if (signal.aborted) {
      return Promise.reject(abortError());
    }
    return new Promise<void>((resolve, reject) => {
      const onAbort = (): void => {
        signal.removeEventListener("abort", onAbort);
        reject(abortError());
      };
      signal.addEventListener("abort", onAbort, { once: true });
      this.pending.then(() => {
        signal.removeEventListener("abort", onAbort);
        if (signal.aborted) {
          reject(abortError());
          return;
        }
        resolve();
      });
    });
  }

  settle(): void {
    this.resolve();
    this.pending = this.newPending();
  }
}

async function drive(
  events: readonly ToggleEvent[]
): Promise<{ control: ToggleControl; memento: MemoryMemento }> {
  const memento = new MemoryMemento();
  const gates = new Map<string, WorkspaceGate>();
  const runners = new Map<string, Promise<void>>();

  const gateFor = (spelling: string): WorkspaceGate => {
    const key = toggleWorkspaceKey(spelling);
    let gate = gates.get(key);
    if (!gate) {
      gate = new WorkspaceGate();
      gates.set(key, gate);
    }
    return gate;
  };

  const control = new ToggleControl({
    workspaceState: memento,
    transition: {
      activate: (workspace, signal) => gateFor(workspace).wait(signal),
      deactivate: (workspace, signal) => gateFor(workspace).wait(signal),
    },
    reportStatus: () => undefined,
  });

  const { steps } = applyToggleEvents(events);

  for (let index = 0; index < events.length; index += 1) {
    const event = events[index]!;
    const key = toggleWorkspaceKey(event.workspace);
    switch (event.kind) {
      case "request": {
        const pending = control.set(
          event.workspace,
          event.value,
          event.origin as ToggleOrigin
        );
        let finished = false;
        void pending.then(
          () => {
            finished = true;
          },
          () => {
            finished = true;
          }
        );
        await Promise.resolve();
        if (!finished) {
          runners.set(key, pending);
        }
        break;
      }
      case "settle": {
        gateFor(event.workspace).settle();
        const runner = runners.get(key);
        if (runner) {
          await runner;
          runners.delete(key);
        }
        break;
      }
      case "reopen": {
        control.reopen(event.workspace);
        const runner = runners.get(key);
        if (runner) {
          await runner;
          runners.delete(key);
        }
        break;
      }
    }

    const step = steps[index]!;
    for (const [workspaceKey, state] of step.workspaces) {
      assert.equal(
        control.displayed(workspaceKey),
        displayedValue(state),
        `displayed mismatch after ${event.kind} for ${workspaceKey}`
      );
      assert.equal(
        control.persisted(workspaceKey),
        state.persisted,
        `persisted mismatch after ${event.kind} for ${workspaceKey}`
      );
      assert.equal(
        control.isTransitioning(workspaceKey),
        state.inFlight !== undefined,
        `in-flight mismatch after ${event.kind} for ${workspaceKey}`
      );
    }

    for (const workspaceKey of quiescentWorkspaces(step)) {
      assert.equal(
        control.displayed(workspaceKey),
        control.persisted(workspaceKey),
        `displayed must equal persisted at quiescent point ${workspaceKey}`
      );
    }

    const primary = step.workspaces.get(PRIMARY_WORKSPACE_KEY);
    if (primary) {
      for (const alias of WORKSPACE_ALIASES) {
        assert.equal(toggleWorkspaceKey(alias), PRIMARY_WORKSPACE_KEY);
        assert.equal(control.persisted(alias), primary.persisted);
        assert.equal(control.displayed(alias), displayedValue(primary));
      }
    }
  }

  return { control, memento };
}

test("P28: ToggleControl matches the toggle event reference model", async () => {
  await fc.assert(
    fc.asyncProperty(arbToggleEventSequence(), async (events) => {
      const { control, memento } = await drive(events);
      const { final } = applyToggleEvents(events);

      for (const [workspaceKey, state] of final.workspaces) {
        assert.equal(control.displayed(workspaceKey), displayedValue(state));
        assert.equal(control.persisted(workspaceKey), state.persisted);
      }

      const restored = new ToggleControl({
        workspaceState: memento,
        transition: {
          activate: async () => undefined,
          deactivate: async () => undefined,
        },
        reportStatus: () => undefined,
      });
      for (const [workspaceKey, state] of final.workspaces) {
        assert.equal(
          restored.persisted(workspaceKey),
          state.persisted,
          "reopen from storage must restore persisted Toggle_State"
        );
        assert.equal(restored.displayed(workspaceKey), state.persisted);
        assert.equal(restored.isTransitioning(workspaceKey), false);
      }

      const stored =
        memento.get<Record<string, ToggleState>>(TOGGLE_STORAGE_KEY) ?? {};
      for (const key of Object.keys(stored)) {
        assert.equal(toggleWorkspaceKey(key), key, "storage keys must be canonical");
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("P28: a mid-transition request converges on the newest value", async () => {
  await fc.assert(
    fc.asyncProperty(arbInterruptedToggleSequence(), async (events) => {
      assert.ok(
        events.some((event) => event.kind === "request" && event.duringTransition)
      );
      const { control } = await drive(events);
      const { final, steps } = applyToggleEvents(events);
      for (const [workspaceKey, state] of final.workspaces) {
        assert.equal(control.displayed(workspaceKey), displayedValue(state));
        assert.equal(control.persisted(workspaceKey), state.persisted);
      }
      for (let index = 0; index < events.length; index += 1) {
        const event = events[index]!;
        if (event.kind !== "request" || !event.duringTransition) {
          continue;
        }
        const after = steps[index]!;
        const key = toggleWorkspaceKey(event.workspace);
        const state = after.workspaces.get(key);
        assert.ok(state);
        assert.equal(state.inFlight, event.value);
      }
    }),
    { numRuns: PROPERTY_RUNS }
  );
});

test("P28: applying the current quiescent value does not start a transition", async () => {
  await fc.assert(
    fc.asyncProperty(fc.constantFrom<ToggleState>("On", "Off"), async (value) => {
      const memento = new MemoryMemento();
      if (value === "On") {
        const priming = new ToggleControl({
          workspaceState: memento,
          transition: {
            activate: async () => undefined,
            deactivate: async () => undefined,
          },
          reportStatus: () => undefined,
        });
        await priming.set(WORKSPACE_ALIASES[0], "On");
      }
      let starts = 0;
      const control = new ToggleControl({
        workspaceState: memento,
        transition: {
          activate: async () => {
            starts += 1;
          },
          deactivate: async () => {
            starts += 1;
          },
        },
        reportStatus: () => undefined,
      });
      const before = control.displayed(WORKSPACE_ALIASES[1]);
      assert.equal(before, value);
      await control.set(WORKSPACE_ALIASES[2], value);
      assert.equal(starts, 0);
      assert.equal(control.displayed(WORKSPACE_ALIASES[3]), before);
      assert.equal(control.persisted(WORKSPACE_ALIASES[4]), before);
      assert.equal(control.isTransitioning(WORKSPACE_ALIASES[0]), false);
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
