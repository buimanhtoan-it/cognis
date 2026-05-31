/**
 * Protected user endpoints. Every route in this module sits behind
 * `requireAuth`, which means every request flows through the planted
 * auth-timeout bug in `jwt.validate`. Cognis bugfix-mode tests use this
 * surface to confirm capsule retrieval lands on the real culprit.
 */

import { Router, type NextFunction, type Request, type Response } from "express";

import { ValidationError, asyncHandler } from "../middleware/errorHandler";
import type { RouteDeps } from "./index";
import type { UserPatch, UserRecord } from "../db/userRepo";

/* ------------------------------------------------------------------------ */
/*  DTOs                                                                    */
/* ------------------------------------------------------------------------ */

export interface UserDto {
  id: string;
  username: string;
  email: string;
  roles: ReadonlyArray<string>;
  disabled: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface PatchUserBody {
  email?: string;
  roles?: ReadonlyArray<string>;
  disabled?: boolean;
}

function toDto(record: UserRecord): UserDto {
  return {
    id: record.id,
    username: record.username,
    email: record.email,
    roles: record.roles,
    disabled: record.disabled,
    createdAt: record.createdAt,
    updatedAt: record.updatedAt,
  };
}

const EMAIL_RE = /^[^@\s]+@[^@\s]+\.[^@\s]+$/;
const ROLE_RE = /^[a-z][a-z0-9_-]{0,31}$/;

function parsePatchBody(body: unknown): UserPatch {
  if (!body || typeof body !== "object") {
    throw new ValidationError("body must be a JSON object");
  }
  const obj = body as Record<string, unknown>;
  const patch: UserPatch = {};

  if ("email" in obj) {
    if (typeof obj.email !== "string" || !EMAIL_RE.test(obj.email)) {
      throw new ValidationError("email must be a valid email address");
    }
    patch.email = obj.email;
  }

  if ("roles" in obj) {
    if (!Array.isArray(obj.roles)) {
      throw new ValidationError("roles must be an array of strings");
    }
    const cleaned: string[] = [];
    for (const role of obj.roles) {
      if (typeof role !== "string" || !ROLE_RE.test(role)) {
        throw new ValidationError(`invalid role: ${String(role)}`);
      }
      cleaned.push(role);
    }
    patch.roles = cleaned;
  }

  if ("disabled" in obj) {
    if (typeof obj.disabled !== "boolean") {
      throw new ValidationError("disabled must be a boolean");
    }
    patch.disabled = obj.disabled;
  }

  if (Object.keys(patch).length === 0) {
    throw new ValidationError("patch body has no recognised fields");
  }

  return patch;
}

/* ------------------------------------------------------------------------ */
/*  Handlers                                                                */
/* ------------------------------------------------------------------------ */

export async function getMe(
  req: Request,
  res: Response,
  _next: NextFunction,
  deps: RouteDeps,
): Promise<void> {
  // requireAuth has already populated req.user; the cast is safe because
  // a missing claim would have triggered an early 401 before reaching us.
  const claims = req.user!;
  const record = await deps.userRepo.getById(claims.sub);
  res.status(200).json(toDto(record));
}

export async function patchMe(
  req: Request,
  res: Response,
  _next: NextFunction,
  deps: RouteDeps,
): Promise<void> {
  const claims = req.user!;
  const patch = parsePatchBody(req.body);

  // Only admins may flip the `disabled` flag or grant `admin` role.
  const wantsDisabledChange = "disabled" in patch;
  const grantsAdmin = patch.roles?.includes("admin") ?? false;
  const isAdmin = claims.roles.includes("admin");
  if ((wantsDisabledChange || grantsAdmin) && !isAdmin) {
    res.status(403).json({
      error: "forbidden",
      message: "admin role required to modify privileged fields",
      requestId: req.requestId,
    });
    return;
  }

  const updated = await deps.userRepo.update(claims.sub, patch);
  deps.logger.info(
    {
      sub: claims.sub,
      requestId: req.requestId,
      changedFields: Object.keys(patch),
    },
    "user-patched",
  );
  res.status(200).json(toDto(updated));
}

export async function listUsers(
  req: Request,
  res: Response,
  _next: NextFunction,
  deps: RouteDeps,
): Promise<void> {
  if (!req.user || !req.user.roles.includes("admin")) {
    res.status(403).json({
      error: "forbidden",
      message: "admin role required",
      requestId: req.requestId,
    });
    return;
  }
  const limit = clampInt(req.query.limit, 1, 100, 25);
  const offset = clampInt(req.query.offset, 0, 10_000, 0);
  const records = await deps.userRepo.list(limit, offset);
  res.status(200).json({
    items: records.map(toDto),
    limit,
    offset,
  });
}

function clampInt(raw: unknown, lo: number, hi: number, fallback: number): number {
  if (typeof raw !== "string") return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(lo, Math.min(hi, Math.floor(n)));
}

/* ------------------------------------------------------------------------ */
/*  Router                                                                  */
/* ------------------------------------------------------------------------ */

export function buildUsersRouter(deps: RouteDeps): Router {
  const router = Router();

  // Every route is gated by deps.auth (== requireAuth) — that's where the
  // planted auth-timeout bug surfaces.
  router.get(
    "/me",
    deps.auth,
    asyncHandler<Request>(async (req, res, next) => getMe(req, res, next, deps)),
  );

  router.patch(
    "/me",
    deps.auth,
    asyncHandler<Request>(async (req, res, next) => patchMe(req, res, next, deps)),
  );

  router.get(
    "/",
    deps.auth,
    asyncHandler<Request>(async (req, res, next) => listUsers(req, res, next, deps)),
  );

  return router;
}
