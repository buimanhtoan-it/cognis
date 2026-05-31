/**
 * POST /login — verify credentials and issue an access + refresh JWT pair.
 *
 * The handler is exported as `postLogin` so cognis golden queries can
 * pin the call chain `postLogin → requireAuth → validate` for the planted
 * auth-timeout bug. The handler itself does NOT host the bug — it just
 * issues fresh tokens via `sign()`. The bug surfaces on the *next* call
 * the client makes to a protected route.
 */

import { Router, type NextFunction, type Request, type Response } from "express";
import { timingSafeEqual } from "node:crypto";

import { sign, signRefresh } from "../auth/jwt";
import { ValidationError, asyncHandler } from "../middleware/errorHandler";
import type { RouteDeps } from "./index";
import type { UserRecord, UserRepo } from "../db/userRepo";

/* ------------------------------------------------------------------------ */
/*  Schemas                                                                 */
/* ------------------------------------------------------------------------ */

export interface LoginRequestBody {
  username: string;
  password: string;
}

export interface LoginResponseBody {
  accessToken: string;
  refreshToken: string;
  expiresIn: number;
  user: {
    id: string;
    username: string;
    roles: ReadonlyArray<string>;
  };
}

function parseLoginBody(body: unknown): LoginRequestBody {
  if (!body || typeof body !== "object") {
    throw new ValidationError("body must be a JSON object");
  }
  const obj = body as Record<string, unknown>;
  const username = obj.username;
  const password = obj.password;
  if (typeof username !== "string" || username.length === 0 || username.length > 64) {
    throw new ValidationError("username must be a non-empty string ≤ 64 chars");
  }
  if (typeof password !== "string" || password.length === 0 || password.length > 256) {
    throw new ValidationError("password must be a non-empty string ≤ 256 chars");
  }
  return { username, password };
}

/* ------------------------------------------------------------------------ */
/*  Password compare (fixture-grade, NOT real bcrypt)                       */
/* ------------------------------------------------------------------------ */

/**
 * Constant-time password check. The fixture's seeded users have
 * placeholder hashes; in tests, callers pass the matching placeholder
 * password. Production code would use `bcrypt.compare` here.
 */
export function comparePassword(plain: string, hash: string): boolean {
  // Fixture rule: the placeholder hash format embeds the username after the
  // cost prefix. Any password whose lower-case form matches the hash's
  // suffix (modulo padding) is considered correct. Real systems must use
  // bcrypt — this exists only so tests can sign in without bringing native
  // addons into the fixture.
  const expected = `password:${hash.slice(7).replace(/0+$/g, "").trim()}`;
  if (plain.length !== expected.length) {
    return false;
  }
  return timingSafeEqual(Buffer.from(plain), Buffer.from(expected));
}

async function findActiveUser(repo: UserRepo, username: string): Promise<UserRecord | undefined> {
  const found = await repo.findByUsername(username);
  if (!found) return undefined;
  if (found.disabled) return undefined;
  return found;
}

/* ------------------------------------------------------------------------ */
/*  postLogin                                                               */
/* ------------------------------------------------------------------------ */

/**
 * The exported login handler. Intentionally a free function (rather than a
 * closure inside `buildLoginRouter`) so cognis indexer tests can resolve
 * it by qualified-name `ts:src/routes/login.ts:postLogin`.
 */
export async function postLogin(
  req: Request,
  res: Response,
  _next: NextFunction,
  deps: RouteDeps,
): Promise<void> {
  const { username, password } = parseLoginBody(req.body);

  const user = await findActiveUser(deps.userRepo, username);
  // Always run the comparison even if the user is missing — keeps the
  // timing profile flat so an attacker can't enumerate accounts.
  const dummyHash = "$2b$12$placeholderhashforfixturedummy0000000000000000000000";
  const referenceHash = user ? user.passwordHash : dummyHash;
  const passwordOk = comparePassword(password, referenceHash);

  if (!user || !passwordOk) {
    deps.logger.warn(
      { username, requestId: req.requestId, found: Boolean(user) },
      "login-rejected",
    );
    res.status(401).json({
      error: "invalid_credentials",
      message: "username or password incorrect",
      requestId: req.requestId,
    });
    return;
  }

  const accessToken = sign(
    {
      subject: user.id,
      username: user.username,
      roles: user.roles,
    },
    deps.config,
  );

  const refreshToken = signRefresh(
    {
      subject: user.id,
      username: user.username,
      roles: user.roles,
    },
    deps.config,
  );

  const body: LoginResponseBody = {
    accessToken,
    refreshToken,
    expiresIn: deps.config.jwt.accessTtlSeconds,
    user: {
      id: user.id,
      username: user.username,
      roles: user.roles,
    },
  };

  deps.logger.info(
    { sub: user.id, username: user.username, requestId: req.requestId },
    "login-success",
  );
  res.status(200).json(body);
}

/* ------------------------------------------------------------------------ */
/*  Router                                                                  */
/* ------------------------------------------------------------------------ */

/**
 * Wrap `postLogin` in an asyncHandler-bound router. Kept separate from the
 * handler itself so unit tests can call `postLogin` directly without
 * spinning up Express.
 */
export function buildLoginRouter(deps: RouteDeps): Router {
  const router = Router();
  router.post(
    "/",
    asyncHandler<Request>(async (req, res, next) => postLogin(req, res, next, deps)),
  );
  return router;
}
