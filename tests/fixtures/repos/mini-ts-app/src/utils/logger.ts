/**
 * Thin pino-shaped logger. We don't depend on pino at type level so the
 * fixture compiles without `npm install`. The `Logger` type matches the
 * subset we actually use.
 */

import { formatDuration, monotonicMs } from "./time";

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error" | "fatal";

const LEVEL_RANK: Record<LogLevel, number> = {
  trace: 10,
  debug: 20,
  info: 30,
  warn: 40,
  error: 50,
  fatal: 60,
};

export interface LogEvent {
  level: LogLevel;
  msg: string;
  time: number;
  [key: string]: unknown;
}

export interface Logger {
  trace(meta: Record<string, unknown> | string, msg?: string): void;
  debug(meta: Record<string, unknown> | string, msg?: string): void;
  info(meta: Record<string, unknown> | string, msg?: string): void;
  warn(meta: Record<string, unknown> | string, msg?: string): void;
  error(meta: Record<string, unknown> | string, msg?: string): void;
  fatal(meta: Record<string, unknown> | string, msg?: string): void;
  child(bindings: Record<string, unknown>): Logger;
}

export interface LoggerOptions {
  level?: LogLevel;
  bindings?: Record<string, unknown>;
  sink?: (event: LogEvent) => void;
}

/**
 * Default sink writes a single JSON line per event to stdout. We accept
 * a custom sink to make tests deterministic.
 */
function defaultSink(event: LogEvent): void {
  process.stdout.write(`${JSON.stringify(event)}\n`);
}

export function createLogger(options: LoggerOptions = {}): Logger {
  const level = options.level ?? "info";
  const bindings = options.bindings ?? {};
  const sink = options.sink ?? defaultSink;
  const minRank = LEVEL_RANK[level];

  function emit(eventLevel: LogLevel, payload: Record<string, unknown> | string, msg?: string): void {
    if (LEVEL_RANK[eventLevel] < minRank) {
      return;
    }
    const event: LogEvent = {
      level: eventLevel,
      time: Date.now(),
      msg: typeof payload === "string" ? payload : msg ?? "",
      ...bindings,
      ...(typeof payload === "object" ? payload : {}),
    };
    sink(event);
  }

  const logger: Logger = {
    trace: (meta, msg) => emit("trace", meta, msg),
    debug: (meta, msg) => emit("debug", meta, msg),
    info: (meta, msg) => emit("info", meta, msg),
    warn: (meta, msg) => emit("warn", meta, msg),
    error: (meta, msg) => emit("error", meta, msg),
    fatal: (meta, msg) => emit("fatal", meta, msg),
    child: (childBindings) =>
      createLogger({
        level,
        bindings: { ...bindings, ...childBindings },
        sink,
      }),
  };

  return logger;
}

/**
 * Helper for timing a block of work and logging the duration once it
 * resolves. The returned wrapper logs at `info` on success and `error`
 * on rejection, both with the same `op` label.
 */
export async function timed<T>(
  logger: Logger,
  op: string,
  fn: () => Promise<T>,
): Promise<T> {
  const start = monotonicMs();
  try {
    const value = await fn();
    const elapsed = monotonicMs() - start;
    logger.info({ op, durationMs: elapsed, duration: formatDuration(elapsed), ok: true }, "op-complete");
    return value;
  } catch (err) {
    const elapsed = monotonicMs() - start;
    logger.error(
      {
        op,
        durationMs: elapsed,
        duration: formatDuration(elapsed),
        ok: false,
        err: err instanceof Error ? { name: err.name, message: err.message } : { message: String(err) },
      },
      "op-failed",
    );
    throw err;
  }
}

export function levelFromString(value: string | undefined, fallback: LogLevel = "info"): LogLevel {
  if (!value) return fallback;
  const lower = value.toLowerCase() as LogLevel;
  if (lower in LEVEL_RANK) {
    return lower;
  }
  return fallback;
}
