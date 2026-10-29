import type { HandshakeResult } from "./contract";
import {
  compatibilitySnapshotFromHandshake,
  type CompatibilitySnapshot,
  type UnavailableCompatibilitySnapshot,
} from "./compatibility";
import { canonicalizePath } from "./mcpCanonical";

export const DEFAULT_COMPATIBILITY_TTL_MS = 30_000;

export interface ProbeOptions {
  /** Ignore a completed snapshot and start (or join) a forced generation. */
  force?: boolean;
}

type CompatibilityProbe = (
  repoRoot: string,
  expectedEngineVersion?: string
) => Promise<HandshakeResult | undefined>;

interface InflightProbe {
  generation: number;
  force: boolean;
  promise: Promise<CompatibilitySnapshot>;
}

interface RootState {
  generation: number;
  invalidated: boolean;
  snapshot?: CompatibilitySnapshot;
  inflight?: InflightProbe;
}

/**
 * Rejection used only when a lifecycle boundary makes a probe unsafe to use.
 * Probe execution failures themselves are represented by unavailable snapshots.
 */
export class StaleCompatibilityProbeError extends Error {
  constructor(message = "Compatibility probe was superseded or invalidated") {
    super(message);
    this.name = "StaleCompatibilityProbeError";
  }
}

/**
 * Per-workspace compatibility cache with TTL, single-flight, and latest-wins
 * lifecycle guards. The canonical root is used only as the cache key; probes
 * receive the caller's path so filesystem casing/aliases remain usable.
 */
export class CompatibilityCoordinator {
  private readonly roots = new Map<string, RootState>();
  private lifecycleGeneration = 0;
  private disposed = false;

  constructor(
    private readonly probe: CompatibilityProbe,
    private readonly ttlMs = DEFAULT_COMPATIBILITY_TTL_MS,
    private readonly now: () => number = Date.now
  ) {
    if (!Number.isFinite(ttlMs) || ttlMs < 0) {
      throw new RangeError("Compatibility TTL must be a non-negative finite number");
    }
  }

  /** Return a fresh cached snapshot, join a matching flight, or start a probe. */
  getOrProbe(
    repoRoot: string,
    expectedEngineVersion: string | undefined,
    opts: ProbeOptions = {}
  ): Promise<CompatibilitySnapshot> {
    this.assertActive();
    const key = canonicalizePath(repoRoot);
    const state = this.stateFor(key);
    const force = opts.force === true;

    if (!force && state.snapshot && this.isFresh(state.snapshot)) {
      return Promise.resolve(state.snapshot);
    }

    if (state.inflight) {
      if (!force || state.inflight.force) {
        return state.inflight.promise;
      }
      // A forced request supersedes a normal in-flight generation immediately.
    }

    return this.startProbe(
      key,
      repoRoot,
      expectedEngineVersion,
      state,
      force
    );
  }

  /** Return the currently committed snapshot without performing any I/O. */
  peek(repoRoot: string): CompatibilitySnapshot | undefined {
    return this.roots.get(canonicalizePath(repoRoot))?.snapshot;
  }

  /** Invalidate all cached and in-flight work for a closed workspace root. */
  evict(repoRoot: string): void {
    const key = canonicalizePath(repoRoot);
    const state = this.roots.get(key);
    if (!state) {
      return;
    }
    state.invalidated = true;
    state.generation += 1;
    state.snapshot = undefined;
    state.inflight = undefined;
    this.roots.delete(key);
  }

  /** Invalidate every root and prevent further probes after deactivation. */
  dispose(): void {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.lifecycleGeneration += 1;
    for (const state of this.roots.values()) {
      state.invalidated = true;
      state.generation += 1;
      state.snapshot = undefined;
      state.inflight = undefined;
    }
    this.roots.clear();
  }

  private stateFor(key: string): RootState {
    const existing = this.roots.get(key);
    if (existing) {
      return existing;
    }
    const created: RootState = { generation: 0, invalidated: false };
    this.roots.set(key, created);
    return created;
  }

  private isFresh(snapshot: CompatibilitySnapshot): boolean {
    const age = this.now() - snapshot.observedAt;
    return age >= 0 && age < this.ttlMs;
  }

  private startProbe(
    key: string,
    repoRoot: string,
    expectedEngineVersion: string | undefined,
    state: RootState,
    force: boolean
  ): Promise<CompatibilitySnapshot> {
    const generation = state.generation + 1;
    state.generation = generation;
    const lifecycleGeneration = this.lifecycleGeneration;

    const promise = Promise.resolve()
      .then(() => this.probe(repoRoot, expectedEngineVersion))
      .then(
        (result) => this.finishProbe(key, state, generation, lifecycleGeneration, result),
        () => this.finishProbe(key, state, generation, lifecycleGeneration, undefined)
      );

    state.inflight = { generation, force, promise };
    return promise;
  }

  private finishProbe(
    key: string,
    state: RootState,
    generation: number,
    lifecycleGeneration: number,
    result: HandshakeResult | undefined
  ): CompatibilitySnapshot {
    if (!this.canCommit(key, state, generation, lifecycleGeneration)) {
      throw new StaleCompatibilityProbeError();
    }

    const observedAt = this.now();
    const previousSnapshot = state.snapshot;
    const snapshot: CompatibilitySnapshot = result
      ? compatibilitySnapshotFromHandshake(result, generation, observedAt)
      : previousSnapshot?.phase === "confirmed" &&
          previousSnapshot.result.compatibility !== "ok"
        ? compatibilitySnapshotFromHandshake(
            previousSnapshot.result,
            generation,
            observedAt
          )
        : this.unavailableSnapshot(generation, observedAt);

    state.snapshot = snapshot;
    if (state.inflight?.generation === generation) {
      state.inflight = undefined;
    }
    return snapshot;
  }

  private canCommit(
    key: string,
    state: RootState,
    generation: number,
    lifecycleGeneration: number
  ): boolean {
    return (
      !this.disposed &&
      lifecycleGeneration === this.lifecycleGeneration &&
      !state.invalidated &&
      this.roots.get(key) === state &&
      state.generation === generation
    );
  }

  private unavailableSnapshot(
    generation: number,
    observedAt: number
  ): UnavailableCompatibilitySnapshot {
    return { phase: "unavailable", generation, observedAt };
  }

  private assertActive(): void {
    if (this.disposed) {
      throw new Error("CompatibilityCoordinator has been disposed");
    }
  }
}
