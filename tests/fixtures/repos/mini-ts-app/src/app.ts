/**
 * `createApp` — wires every middleware and route module into an Express
 * instance and returns the assembled app together with the shared
 * dependencies (logger, repo, config) so tests can poke at them
 * directly without reaching back through the running server.
 */

import express, { type Express } from "express";

import { requireAuth } from "./middleware/auth";
import { errorHandler, notFoundHandler } from "./middleware/errorHandler";
import { requestLogger } from "./middleware/logging";
import { registerRoutes } from "./routes";
import { buildSeedRepo, UserRepo } from "./db/userRepo";
import { createLogger, type Logger } from "./utils/logger";
import type { AppConfig } from "./utils/secrets";
import type { ValidationContext } from "./auth/jwt";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface CreateAppOptions {
  config: AppConfig;
  /** Optional injected logger. When omitted, a default one is built. */
  logger?: Logger;
  /** Optional pre-seeded repo. When omitted, the fixture seed is used. */
  userRepo?: UserRepo;
  /** Optional jwt validation context (introspect stub for tests). */
  validation?: ValidationContext;
  /** Disable the built-in request logger (handy in unit tests). */
  silentRequestLog?: boolean;
}

export interface AppHandle {
  app: Express;
  logger: Logger;
  userRepo: UserRepo;
  config: AppConfig;
}

/* ------------------------------------------------------------------------ */
/*  createApp                                                               */
/* ------------------------------------------------------------------------ */

/**
 * Build a fully wired Express app. The returned handle exposes the same
 * `logger` and `userRepo` instances the app uses internally, so tests can
 * assert log output and seed/snapshot users without re-importing module
 * singletons.
 */
export function createApp(options: CreateAppOptions): AppHandle {
  const { config } = options;
  const logger =
    options.logger ?? createLogger({ level: config.logLevel, bindings: { svc: "mini-ts-app" } });
  const userRepo = options.userRepo ?? buildSeedRepo();

  const app = express();

  // JSON body parser. Tight cap so a misbehaving client can't exhaust memory.
  app.use(express.json({ limit: "256kb" }));
  app.use(express.urlencoded({ extended: false, limit: "32kb" }));

  if (!options.silentRequestLog) {
    app.use(requestLogger({ logger }));
  }

  // Build a single auth middleware factory bound to the current deps so
  // every protected route shares the exact same validation context.
  const auth = requireAuth({
    config,
    logger,
    validation: options.validation,
  });

  registerRoutes(app, {
    config,
    logger,
    userRepo,
    auth,
  });

  // Fall-throughs come after the route registry so unmatched paths land
  // on the 404 envelope rather than express's HTML default.
  app.use(notFoundHandler());
  app.use(errorHandler({ logger, exposeMessages: config.nodeEnv !== "production" }));

  return { app, logger, userRepo, config };
}
