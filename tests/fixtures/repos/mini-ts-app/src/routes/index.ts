/**
 * Single point of route registration. Centralised so `createApp` doesn't
 * leak knowledge of individual route modules and so cognis indexer tests
 * can verify a deterministic set of HTTP-route attributes.
 */

import type { Express, RequestHandler } from "express";

import type { UserRepo } from "../db/userRepo";
import type { AppConfig } from "../utils/secrets";
import type { Logger } from "../utils/logger";

import { healthRouter } from "./health";
import { buildLoginRouter } from "./login";
import { buildUsersRouter } from "./users";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface RouteDeps {
  config: AppConfig;
  logger: Logger;
  userRepo: UserRepo;
  /** Pre-built `requireAuth` middleware bound to the app deps. */
  auth: RequestHandler;
}

/* ------------------------------------------------------------------------ */
/*  registerRoutes                                                          */
/* ------------------------------------------------------------------------ */

/**
 * Mount every route module under its canonical prefix.
 *
 *   /health       → health probes (public)
 *   /login        → POST /login (public)
 *   /users        → /users/me GET + PATCH (auth required)
 *
 * The function is intentionally small — its only job is composition.
 */
export function registerRoutes(app: Express, deps: RouteDeps): void {
  app.use("/health", healthRouter(deps));
  app.use("/login", buildLoginRouter(deps));
  app.use("/users", buildUsersRouter(deps));
}
