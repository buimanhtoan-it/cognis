/**
 * Centralised error-to-response translator. Every route handler in this
 * fixture surfaces failures via `next(err)` (or `throw` inside an async
 * wrapper); the middleware here turns that into a stable JSON envelope so
 * cognis bug-reporting tests can rely on a consistent error shape.
 */

import type { ErrorRequestHandler, NextFunction, Request, Response } from "express";

import { JwtError } from "../auth/jwt";
import { DuplicateUserError, UserNotFoundError } from "../db/userRepo";
import { TimeoutError } from "../utils/time";
import type { Logger } from "../utils/logger";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface ErrorEnvelope {
  error: string;
  message: string;
  requestId?: string;
  details?: unknown;
}

export interface ErrorHandlerDeps {
  logger: Logger;
  /** When true, error responses include the message verbatim. */
  exposeMessages?: boolean;
}

/**
 * App-level explicit HTTP error. Route handlers can `throw new HttpError(...)`
 * to short-circuit with a known status code without reaching for express's
 * res.status() chain.
 */
export class HttpError extends Error {
  public readonly status: number;
  public readonly code: string;
  public readonly details?: unknown;

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export class ValidationError extends HttpError {
  constructor(message: string, details?: unknown) {
    super(400, "validation_error", message, details);
    this.name = "ValidationError";
  }
}

/* ------------------------------------------------------------------------ */
/*  Helpers                                                                 */
/* ------------------------------------------------------------------------ */

/**
 * Map an arbitrary thrown value to a stable HTTP envelope. The function is
 * exported so tests can assert mappings without spinning up Express.
 */
export function classifyError(err: unknown): { status: number; envelope: ErrorEnvelope } {
  if (err instanceof HttpError) {
    return {
      status: err.status,
      envelope: {
        error: err.code,
        message: err.message,
        details: err.details,
      },
    };
  }

  if (err instanceof JwtError) {
    return {
      status: err.code.startsWith("INTROSPECTION_") ? 503 : 401,
      envelope: {
        error: err.code.toLowerCase(),
        message: err.message,
      },
    };
  }

  if (err instanceof TimeoutError) {
    return {
      status: 504,
      envelope: {
        error: "timeout",
        message: err.message,
      },
    };
  }

  if (err instanceof UserNotFoundError) {
    return {
      status: 404,
      envelope: {
        error: "user_not_found",
        message: err.message,
      },
    };
  }

  if (err instanceof DuplicateUserError) {
    return {
      status: 409,
      envelope: {
        error: "duplicate_user",
        message: err.message,
      },
    };
  }

  return {
    status: 500,
    envelope: {
      error: "internal_error",
      message: "an unexpected error occurred",
    },
  };
}

/**
 * Wrap an async route handler so a thrown promise rejection is forwarded
 * to `next(err)` instead of crashing the process. Express doesn't do this
 * for us until v5.
 */
export function asyncHandler<R extends Request = Request>(
  handler: (req: R, res: Response, next: NextFunction) => Promise<unknown>,
) {
  return function wrappedAsyncHandler(req: R, res: Response, next: NextFunction): void {
    Promise.resolve()
      .then(() => handler(req, res, next))
      .catch(next);
  };
}

/* ------------------------------------------------------------------------ */
/*  Middleware                                                              */
/* ------------------------------------------------------------------------ */

/**
 * 404 handler — registered AFTER all route modules so an unmatched request
 * falls through here and gets a typed envelope instead of express's default
 * HTML body.
 */
export function notFoundHandler() {
  return function notFoundMiddleware(req: Request, res: Response, _next: NextFunction): void {
    res.status(404).json({
      error: "not_found",
      message: `no route for ${req.method} ${req.originalUrl}`,
      requestId: req.requestId,
    });
  };
}

/**
 * Final error middleware. Express recognises an error handler by its
 * 4-argument signature, so we keep `_next` even though we never call it.
 */
export function errorHandler(deps: ErrorHandlerDeps): ErrorRequestHandler {
  const exposeMessages = deps.exposeMessages ?? true;

  return function appErrorHandler(
    err: unknown,
    req: Request,
    res: Response,
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    _next: NextFunction,
  ): void {
    const { status, envelope } = classifyError(err);

    if (status >= 500) {
      deps.logger.error(
        {
          status,
          requestId: req.requestId,
          code: envelope.error,
          err: err instanceof Error ? { name: err.name, message: err.message } : { value: String(err) },
        },
        "request-failed",
      );
    } else {
      deps.logger.warn(
        {
          status,
          requestId: req.requestId,
          code: envelope.error,
        },
        "request-rejected",
      );
    }

    const body: ErrorEnvelope = {
      error: envelope.error,
      message: exposeMessages ? envelope.message : "request failed",
      requestId: req.requestId,
    };
    if (envelope.details !== undefined) {
      body.details = envelope.details;
    }
    res.status(status).json(body);
  };
}
