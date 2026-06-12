/**
 * Structured diagnostics + trace logging for the extension.
 *
 * Why this exists: the extension and the Python backend are two processes in
 * two languages wired across several JSON contracts. When something drifts in
 * production the only artifact today is an ephemeral OutputChannel the user has
 * to think to open. That is why "e2e green, prod broken" was invisible.
 *
 * This module gives every boundary a single, structured sink:
 *   - human-readable lines mirrored into the existing "Cognis" OutputChannel
 *     (live debugging), and
 *   - append-only JSON Lines under the extension's global storage
 *     (``diagnostics.jsonl``), size-rotated, so a user can attach a full trace
 *     to a bug report and we can reconstruct exactly what happened — every CLI
 *     call, every command, every contract-validation failure, with timings.
 *
 * It is intentionally dependency-free and privacy-first: it records event
 * shapes, scopes, durations, exit codes, and version skew — never query text,
 * file contents, or secrets. Callers pass a small ``data`` bag; keep it to
 * identifiers and counts.
 */
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { getOutputChannel } from "./cli";

export type TraceLevel = "debug" | "info" | "warn" | "error";

export interface TraceEntry {
  ts: string;
  level: TraceLevel;
  scope: string;
  message: string;
  /** Identifiers/counts only — never query text, file contents, or secrets. */
  data?: Record<string, unknown>;
  /** Milliseconds, present on span-completion entries. */
  durationMs?: number;
  extVersion?: string;
}

const LEVEL_RANK: Record<TraceLevel, number> = {
  debug: 10,
  info: 20,
  warn: 30,
  error: 40,
};

/** Rotate the JSONL sink at this size so a long-lived install never grows unbounded. */
const MAX_LOG_BYTES = 5 * 1024 * 1024; // 5 MiB

class Trace {
  private logFile: string | undefined;
  private extVersion: string | undefined;
  private minLevel: TraceLevel = "debug";
  /** Buffer entries emitted before init() so nothing from early activation is lost. */
  private preInit: TraceEntry[] = [];

  /**
   * Wire the JSONL sink to the extension's global storage and record the
   * extension version (so every entry can be correlated with a release, which
   * is how we detect version-skew failures). Safe to call once at activation.
   */
  init(context: vscode.ExtensionContext, extVersion?: string): void {
    this.extVersion = extVersion;
    try {
      // Testability/support hook: COGNIS_DIAGNOSTICS_DIR forces the JSONL sink
      // to a known directory (used by the full-stack host e2e to read the trace,
      // and handy for support to collect logs to a chosen folder). Defaults to
      // the extension's global storage.
      const override = process.env.COGNIS_DIAGNOSTICS_DIR?.trim();
      const dir = override || context.globalStorageUri.fsPath;
      fs.mkdirSync(dir, { recursive: true });
      this.logFile = path.join(dir, "diagnostics.jsonl");
      this.rotateIfNeeded();
      const flushed = this.preInit.splice(0);
      for (const entry of flushed) {
        this.append({ ...entry, extVersion: entry.extVersion ?? this.extVersion });
      }
    } catch (err) {
      // Never let diagnostics wiring break activation.
      getOutputChannel().appendLine(
        `[diagnostics] could not open log file: ${
          err instanceof Error ? err.message : String(err)
        }`
      );
    }
  }

  /** Absolute path of the JSONL sink, once init() has run. */
  logFilePath(): string | undefined {
    return this.logFile;
  }

  setMinLevel(level: TraceLevel): void {
    this.minLevel = level;
  }

  debug(scope: string, message: string, data?: Record<string, unknown>): void {
    this.record("debug", scope, message, data);
  }

  info(scope: string, message: string, data?: Record<string, unknown>): void {
    this.record("info", scope, message, data);
  }

  warn(scope: string, message: string, data?: Record<string, unknown>): void {
    this.record("warn", scope, message, data);
  }

  error(scope: string, message: string, data?: Record<string, unknown>): void {
    this.record("error", scope, message, data);
  }

  /**
   * Time an async operation, logging start/end (or failure) with a duration.
   * The single instrument for every boundary call so latency and failures are
   * always captured without per-call boilerplate. Re-throws so control flow is
   * unchanged.
   */
  async span<T>(
    scope: string,
    name: string,
    fn: () => Promise<T>,
    data?: Record<string, unknown>
  ): Promise<T> {
    const started = Date.now();
    this.record("debug", scope, `${name} started`, data);
    try {
      const result = await fn();
      this.record("info", scope, `${name} ok`, data, Date.now() - started);
      return result;
    } catch (err) {
      this.record(
        "error",
        scope,
        `${name} failed`,
        { ...data, error: err instanceof Error ? err.message : String(err) },
        Date.now() - started
      );
      throw err;
    }
  }

  private record(
    level: TraceLevel,
    scope: string,
    message: string,
    data?: Record<string, unknown>,
    durationMs?: number
  ): void {
    if (LEVEL_RANK[level] < LEVEL_RANK[this.minLevel]) {
      return;
    }
    const entry: TraceEntry = {
      ts: new Date().toISOString(),
      level,
      scope,
      message,
      ...(data && Object.keys(data).length ? { data } : {}),
      ...(durationMs !== undefined ? { durationMs } : {}),
      extVersion: this.extVersion,
    };
    this.mirror(entry);
    if (this.logFile) {
      this.append(entry);
    } else {
      this.preInit.push(entry);
    }
  }

  private mirror(entry: TraceEntry): void {
    const suffix = entry.durationMs !== undefined ? ` (${entry.durationMs}ms)` : "";
    const extra = entry.data ? ` ${JSON.stringify(entry.data)}` : "";
    getOutputChannel().appendLine(
      `[${entry.level}] ${entry.scope}: ${entry.message}${suffix}${extra}`
    );
  }

  private append(entry: TraceEntry): void {
    try {
      this.rotateIfNeeded();
      fs.appendFileSync(this.logFile!, `${JSON.stringify(entry)}\n`, "utf8");
    } catch {
      // Swallow: a diagnostics write must never break the feature it observes.
    }
  }

  private rotateIfNeeded(): void {
    if (!this.logFile) {
      return;
    }
    try {
      if (fs.existsSync(this.logFile) && fs.statSync(this.logFile).size >= MAX_LOG_BYTES) {
        const rotated = `${this.logFile}.1`;
        fs.rmSync(rotated, { force: true });
        fs.renameSync(this.logFile, rotated);
      }
    } catch {
      // Best effort; if rotation fails we keep appending to the current file.
    }
  }
}

/** Application-wide structured trace logger. */
export const trace = new Trace();
