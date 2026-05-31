/**
 * `requireAuth` middleware. Reads a bearer token from `Authorization`,
 * delegates to `jwt.validate`, and stamps the resolved claims onto
 * `req.user` for downstream handlers. The middleware itself is thin —
 * the planted bug lives in `jwt.validate`, not here.
 */

import type { NextFunction, Request, Response } from "express";

import {
  type AccessTokenClaims,
  JwtError,
  type ValidationContext,
  validate,
} from "../auth/jwt";
import type { AppConfig } from "../utils/secrets";
import type { Logger } from "../utils/logger";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

/** Augment Express's Request locally so handlers see `req.user`. */
declare module "express-serve-static-core" {
  interface Request {
    user?: AccessTokenClaims;
    requestId?: string;
  }
}

export interface AuthDeps {
  config: AppConfig;
  logger: Logger;
  validation?: ValidationContext;
}

/* ------------------------------------------------------------------------ */
/*  Helpers                                                                 */
/* ------------------------------------------------------------------------ */

const BEARER_PREFIX = "bearer ";

export function extractBearer(headerValue: string | undefined): string | null {
  if (!headerValue) return null;
  if (!headerValue.toLowerCase().startsWith(BEARER_PREFIX)) {
    return null;
  }
  const token = headerValue.slice(BEARER_PREFIX.length).trim();
  return token.length === 0 ? null : token;
}

function jwtErrorToStatus(err: JwtError): number {
  switch (err.code) {
    case "MISSING":
    case "MALFORMED":
    case "BAD_SIGNATURE":
    case "TAMPERED":
      return 401;
    case "EXPIRED":
    case "BAD_ISSUER":
    case "BAD_AUDIENCE":
    case "INACTIVE":
      return 401;
    case "INTROSPECTION_TIMEOUT":
    case "INTROSPECTION_FAILED":
      return 503;
    default:
      return 401;
  }
}

/* ------------------------------------------------------------------------ */
/*  requireAuth                                                             */
/* ------------------------------------------------------------------------ */

/**
 * Build a `requireAuth` middleware bound to the given config + logger.
 * The middleware:
 *   - extracts the bearer token from `Authorization`
 *   - calls `jwt.validate` (which is where the auth-timeout bug lives)
 *   - on success, populates `req.user` and calls `next()`
 *   - on failure, responds with the appropriate status code
 */
export function requireAuth(deps: AuthDeps) {
  return async function requireAuthMiddleware(
    req: Request,
    res: Response,
    next: NextFunction,
  ): Promise<void> {
    const token = extractBearer(req.header("authorization"));
    if (!token) {
      res.status(401).json({
        error: "missing_authorization",
        message: "Authorization header with bearer token required",
        requestId: req.requestId,
      });
      return;
    }

    try {
      // NOTE: this call inherits the planted auth-timeout bug from
      //       `jwt.validate`. Do not "fix" it here — the indexer + retrieval
      //       tests expect the symptom to surface so capsule composition
      //       points at the real culprit.
      const claims = await validate(token, deps.config, deps.validation);
      req.user = claims;
      next();
    } catch (err) {
      if (err instanceof JwtError) {
        deps.logger.warn(
          { code: err.code, requestId: req.requestId },
          "auth rejected",
        );
        res.status(jwtErrorToStatus(err)).json({
          error: err.code.toLowerCase(),
          message: err.message,
          requestId: req.requestId,
        });
        return;
      }
      deps.logger.error(
        { err: err instanceof Error ? err.message : String(err), requestId: req.requestId },
        "auth middleware crashed",
      );
      res.status(500).json({
        error: "internal_error",
        message: "auth middleware error",
        requestId: req.requestId,
      });
    }
  };
}

/**
 * Stricter wrapper that also enforces a role list. Composes on top of
 * `requireAuth` so the auth-timeout failure mode propagates identically.
 */
export function requireRole(role: string, deps: AuthDeps) {
  const inner = requireAuth(deps);
  return async function requireRoleMiddleware(
    req: Request,
    res: Response,
    next: NextFunction,
  ): Promise<void> {
    await inner(req, res, async (err?: unknown) => {
      if (err) {
        next(err);
        return;
      }
      if (!req.user || !req.user.roles.includes(role)) {
        res.status(403).json({
          error: "forbidden",
          message: `role ${role} required`,
          requestId: req.requestId,
        });
        return;
      }
      next();
    });
  };
}

/**
 * Soft variant: populate `req.user` if a valid bearer is present, but
 * never reject the request. Used by routes that want to personalise their
 * response without forcing login.
 */
export function attachUserIfPresent(deps: AuthDeps) {
  return async function attachUserMiddleware(
    req: Request,
    _res: Response,
    next: NextFunction,
  ): Promise<void> {
    const token = extractBearer(req.header("authorization"));
    if (!token) {
      next();
      return;
    }
    try {
      req.user = await validate(token, deps.config, deps.validation);
    } catch {
      // Soft failure — drop silently.
    }
    next();
  };
}
