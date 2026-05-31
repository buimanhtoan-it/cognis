/**
 * Per-request structured logging middleware. Stamps every request with a
 * request id, logs entry + exit, and never echoes raw authorization
 * headers. The redaction pass is intentionally aggressive — the cognis
 * enricher should never see a real bearer flow through fixture logs.
 */

import type { NextFunction, Request, Response } from "express";
import { randomUUID } from "node:crypto";

import type { Logger } from "../utils/logger";
import { monotonicMs, formatDuration } from "../utils/time";
import { redactBearer, scrubSecrets } from "../utils/secrets";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface LoggingDeps {
  logger: Logger;
  /** Header name that, if present, overrides the auto-generated request id. */
  requestIdHeader?: string;
  /** Pre-flight predicate — return false to skip logging entirely. */
  shouldLog?: (req: Request) => boolean;
}

const DEFAULT_REQUEST_ID_HEADER = "x-request-id";

/* ------------------------------------------------------------------------ */
/*  Helpers                                                                 */
/* ------------------------------------------------------------------------ */

/**
 * Pick a request id off the inbound headers, or mint a fresh UUID. The
 * returned value is also echoed back via `X-Request-Id` so downstream
 * services can correlate.
 */
export function resolveRequestId(req: Request, headerName: string): string {
  const fromHeader = req.header(headerName);
  if (typeof fromHeader === "string" && fromHeader.length > 0 && fromHeader.length <= 128) {
    return fromHeader;
  }
  return randomUUID();
}

/**
 * Build a redaction-friendly snapshot of a header bag. We purposefully do
 * NOT log the full header map; a small allow-list keeps the surface tight.
 */
function snapshotHeaders(req: Request): Record<string, string> {
  const result: Record<string, string> = {};
  const interesting = ["user-agent", "accept", "content-type", "content-length", "host"];
  for (const name of interesting) {
    const value = req.header(name);
    if (typeof value === "string") {
      result[name] = value.slice(0, 256);
    }
  }
  const auth = req.header("authorization");
  if (typeof auth === "string") {
    result.authorization = redactBearer(auth);
  }
  return result;
}

/**
 * Format a query string for logging. Drops anything that looks like a
 * credential (long hex/base64 blobs, jwt-shaped strings).
 */
export function safeQuery(req: Request): string | undefined {
  const original = (req.originalUrl ?? req.url ?? "").split("?")[1];
  if (!original) return undefined;
  return scrubSecrets(original).slice(0, 512);
}

/* ------------------------------------------------------------------------ */
/*  Middleware                                                              */
/* ------------------------------------------------------------------------ */

/**
 * Build a request-logging middleware bound to `deps.logger`. The middleware
 * stamps `req.requestId` (consumed by `requireAuth`/error handlers) and
 * emits two log lines per request: `req-start` and `req-end`.
 */
export function requestLogger(deps: LoggingDeps) {
  const headerName = (deps.requestIdHeader ?? DEFAULT_REQUEST_ID_HEADER).toLowerCase();
  const shouldLog = deps.shouldLog ?? (() => true);

  return function requestLoggingMiddleware(
    req: Request,
    res: Response,
    next: NextFunction,
  ): void {
    if (!shouldLog(req)) {
      next();
      return;
    }
    const requestId = resolveRequestId(req, headerName);
    req.requestId = requestId;
    res.setHeader("X-Request-Id", requestId);

    const startedAt = monotonicMs();
    const childLogger = deps.logger.child({ requestId, method: req.method, path: req.path });

    childLogger.info(
      {
        headers: snapshotHeaders(req),
        query: safeQuery(req),
        ip: req.ip,
      },
      "req-start",
    );

    res.on("finish", () => {
      const durationMs = monotonicMs() - startedAt;
      childLogger.info(
        {
          status: res.statusCode,
          durationMs,
          duration: formatDuration(durationMs),
        },
        "req-end",
      );
    });

    res.on("close", () => {
      if (!res.writableEnded) {
        const durationMs = monotonicMs() - startedAt;
        childLogger.warn(
          {
            status: res.statusCode,
            durationMs,
            duration: formatDuration(durationMs),
          },
          "req-aborted",
        );
      }
    });

    next();
  };
}

/**
 * Convenience helper to stamp a request id even when the full logger
 * middleware is disabled (e.g. inside tests). Mirrors the small subset of
 * `requestLogger` that downstream middleware actually depends on.
 */
export function requestIdOnly(headerName = DEFAULT_REQUEST_ID_HEADER) {
  return function requestIdMiddleware(
    req: Request,
    res: Response,
    next: NextFunction,
  ): void {
    const requestId = resolveRequestId(req, headerName.toLowerCase());
    req.requestId = requestId;
    res.setHeader("X-Request-Id", requestId);
    next();
  };
}
