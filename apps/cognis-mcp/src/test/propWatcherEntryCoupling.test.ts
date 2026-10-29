/**
 * Property 29: Watcher and MCP entry are coupled in both directions (task 16.10).
 *
 * For any sequence of lifecycle events, at every observation (reconciler tick)
 * outside a transition: the watcher runs iff a Managed_MCP_Entry exists for
 * that workspace (after the 5 s restore budget), and every entry Cognis writes
 * carries the Cognis-owned marker.
 *
 * Validates: Requirements 3.10, 3.11, 3.12
 * Properties: P29
 */
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import {
  ENTRY_RESTORE_BUDGET_MS,
  WatcherEntryReconciler,
} from "../activation";
import {
  isCognisOwnedMarker,
  isManagedForWorkspace,
  stampManagedMcpEntry,
} from "../mcpManagedEntry";
import { PROPERTY_RUNS } from "./generators";

const ROOTS = ["/ws-alpha", "/ws-beta"] as const;
const WORKSPACE_KEY: Record<(typeof ROOTS)[number], string> = {
  "/ws-alpha": "wire-alpha",
  "/ws-beta": "wire-beta",
};

type Root = (typeof ROOTS)[number];

type LifecycleEvent =
  | { type: "startWatcher"; root: Root }
  | { type: "stopWatcher"; root: Root }
  | { type: "deleteEntry"; root: Root }
  | { type: "beginTransition"; root: Root }
  | { type: "endTransition"; root: Root }
  | { type: "tick"; root: Root }
  | { type: "advanceMs"; ms: number };

const arbRoot = fc.constantFrom(...ROOTS);

const arbEvent: fc.Arbitrary<LifecycleEvent> = fc.oneof(
  {
    weight: 4,
    arbitrary: arbRoot.map((root) => ({ type: "tick" as const, root })),
  },
  {
    weight: 2,
    arbitrary: arbRoot.map((root) => ({ type: "startWatcher" as const, root })),
  },
  {
    weight: 2,
    arbitrary: arbRoot.map((root) => ({ type: "stopWatcher" as const, root })),
  },
  {
    weight: 2,
    arbitrary: arbRoot.map((root) => ({ type: "deleteEntry" as const, root })),
  },
  {
    weight: 1,
    arbitrary: arbRoot.map((root) => ({ type: "beginTransition" as const, root })),
  },
  {
    weight: 1,
    arbitrary: arbRoot.map((root) => ({ type: "endTransition" as const, root })),
  },
  {
    weight: 2,
    arbitrary: fc.constantFrom(0, 1, 4_999, 5_000, 5_001, 8_000).map((ms) => ({
      type: "advanceMs" as const,
      ms,
    })),
  }
);

interface World {
  watcher: Record<Root, boolean>;
  entry: Record<Root, object | undefined>;
  missingSince: Record<Root, number | undefined>;
  inTransition: Record<Root, boolean>;
  clock: number;
}

function emptyWorld(): World {
  return {
    watcher: { "/ws-alpha": false, "/ws-beta": false },
    entry: { "/ws-alpha": undefined, "/ws-beta": undefined },
    missingSince: { "/ws-alpha": undefined, "/ws-beta": undefined },
    inTransition: { "/ws-alpha": false, "/ws-beta": false },
    clock: 0,
  };
}

function writeEntry(root: Root): object {
  return stampManagedMcpEntry(
    { command: "cognis", args: ["mcpd"] },
    WORKSPACE_KEY[root]
  );
}

function assertObservation(world: World, root: Root): void {
  if (world.inTransition[root]) {
    return;
  }
  const watcher = world.watcher[root];
  const entry = world.entry[root];
  if (!watcher) {
    assert.equal(
      entry,
      undefined,
      `${root}: watcher stopped must remove the Managed_MCP_Entry`
    );
    return;
  }
  if (entry) {
    assert.equal(
      isCognisOwnedMarker(entry),
      true,
      `${root}: Cognis-written entry must carry the owned marker`
    );
    assert.equal(isManagedForWorkspace(entry, WORKSPACE_KEY[root]), true);
    return;
  }
  const since = world.missingSince[root] ?? world.clock;
  assert.ok(
    world.clock - since < ENTRY_RESTORE_BUDGET_MS,
    `${root}: a missing entry while the watcher runs must be restored within 5 s`
  );
}

async function applySequence(events: LifecycleEvent[]): Promise<void> {
  const world = emptyWorld();
  const reconciler = new WatcherEntryReconciler({
    isWatcherRunning: (root) => world.watcher[root as Root] ?? false,
    isEntryPresent: (root) => world.entry[root as Root] !== undefined,
    restoreEntry: async (root) => {
      world.entry[root as Root] = writeEntry(root as Root);
    },
    removeEntry: async (root) => {
      world.entry[root as Root] = undefined;
    },
    inTransition: (root) => world.inTransition[root as Root] ?? false,
    now: () => world.clock,
    restoreBudgetMs: ENTRY_RESTORE_BUDGET_MS,
  });

  for (const event of events) {
    switch (event.type) {
      case "startWatcher":
        world.watcher[event.root] = true;
        break;
      case "stopWatcher":
        world.watcher[event.root] = false;
        break;
      case "deleteEntry":
        world.entry[event.root] = undefined;
        break;
      case "beginTransition":
        world.inTransition[event.root] = true;
        break;
      case "endTransition":
        world.inTransition[event.root] = false;
        break;
      case "advanceMs":
        world.clock += event.ms;
        break;
      case "tick":
        await reconciler.tick(event.root);
        if (world.watcher[event.root] && world.entry[event.root] === undefined) {
          world.missingSince[event.root] ??= world.clock;
        } else {
          world.missingSince[event.root] = undefined;
        }
        assertObservation(world, event.root);
        break;
    }
  }
}

test("Property 29: watcher running iff marked Managed_MCP_Entry outside a transition", async () => {
  await fc.assert(
    fc.asyncProperty(fc.array(arbEvent, { minLength: 1, maxLength: 16 }), async (events) => {
      await applySequence(events);
    }),
    { numRuns: PROPERTY_RUNS }
  );
});
