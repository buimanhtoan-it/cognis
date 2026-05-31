/**
 * Public health + readiness probes. Both are intentionally cheap — they
 * never call into the JWT validator (and so cannot trip the planted
 * auth-timeout bug) so an external load balancer can rely on them under
 * any condition the rest of the service is in.
 */

import { Router, type Request, type Response } from "express";

import { asyncHandler } from "../middleware/errorHandler";
import type { RouteDeps } from "./index";
import { monotonicMs } from "../utils/time";

const startedAt = monotonicMs();

export interface HealthBody {
  status: "ok";
  uptimeMs: number;
  pid: number;
  nodeVersion: string;
}

export interface ReadinessBody {
  status: "ready" | "degraded";
  checks: Record<string, boolean>;
  userCount: number;
}

export function healthRouter(deps: RouteDeps): Router {
  const router = Router();

  router.get(
    "/",
    asyncHandler<Request>(async (_req: Request, res: Response) => {
      const body: HealthBody = {
        status: "ok",
        uptimeMs: Math.round(monotonicMs() - startedAt),
        pid: process.pid,
        nodeVersion: process.version,
      };
      res.status(200).json(body);
    }),
  );

  router.get(
    "/readiness",
    asyncHandler<Request>(async (_req: Request, res: Response) => {
      const checks: Record<string, boolean> = {
        userRepo: deps.userRepo.size() > 0,
        jwtConfigured: deps.config.jwt.secret.length > 0,
        introspectionConfigured: deps.config.introspection.url.length > 0,
      };
      const ready = Object.values(checks).every((v) => v);
      const body: ReadinessBody = {
        status: ready ? "ready" : "degraded",
        checks,
        userCount: deps.userRepo.size(),
      };
      res.status(ready ? 200 : 503).json(body);
    }),
  );

  return router;
}
