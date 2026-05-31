/**
 * Tiny clock helpers. Centralised so tests can monkey-patch a single export
 * instead of every consumer reaching for `Date.now()` directly.
 */

/** Returns wall-clock milliseconds since the unix epoch. */
export function nowMs(): number {
  return Date.now();
}

/** Returns a high-resolution monotonic timestamp in milliseconds. */
export function monotonicMs(): number {
  const [s, ns] = process.hrtime();
  return s * 1000 + ns / 1e6;
}

/** Returns wall-clock seconds since the unix epoch (JWT-style). */
export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/**
 * Sleep for `ms` milliseconds. Wrapped in a function so we can stub it in
 * tests without depending on a fake-timer library.
 */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Race a promise against a timeout. Resolves with the original value or
 * rejects with a TimeoutError. Used by network-bound code paths that must
 * stay below a budget.
 */
export class TimeoutError extends Error {
  public readonly timeoutMs: number;
  constructor(message: string, timeoutMs: number) {
    super(message);
    this.name = "TimeoutError";
    this.timeoutMs = timeoutMs;
  }
}

export async function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  label = "operation",
): Promise<T> {
  let handle: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_, reject) => {
    handle = setTimeout(() => {
      reject(new TimeoutError(`${label} exceeded ${timeoutMs}ms`, timeoutMs));
    }, timeoutMs);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (handle !== undefined) {
      clearTimeout(handle);
    }
  }
}

/** Format a duration in milliseconds as a human-readable string. */
export function formatDuration(ms: number): string {
  if (ms < 1) {
    return `${(ms * 1000).toFixed(0)}µs`;
  }
  if (ms < 1000) {
    return `${ms.toFixed(1)}ms`;
  }
  if (ms < 60_000) {
    return `${(ms / 1000).toFixed(2)}s`;
  }
  return `${(ms / 60_000).toFixed(2)}min`;
}

/** Number of milliseconds in a single day. Used for cookie/jwt expiry math. */
export const ONE_DAY_MS = 86_400_000;

/** Convert seconds to milliseconds, refusing NaN and negative numbers. */
export function secondsToMs(seconds: number): number {
  if (!Number.isFinite(seconds) || seconds < 0) {
    throw new RangeError(`secondsToMs: invalid value ${seconds}`);
  }
  return Math.floor(seconds * 1000);
}
