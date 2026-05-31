import type { HealthReport, RepairPlan } from "./types";

interface BuildRepairPlanArgs {
  configExists: boolean;
  mcpConfigured: boolean;
  health?: HealthReport;
  stateLiveIndexing: boolean;
  liveIndexingRunning: boolean;
}

export function buildRepairPlan({
  configExists,
  mcpConfigured,
  health,
  stateLiveIndexing,
  liveIndexingRunning,
}: BuildRepairPlanArgs): RepairPlan {
  const failedChecks = health
    ? Object.entries(health.checks)
        .filter(([, check]) => check.status === "fail")
        .map(([name]) => name)
    : [];

  const needsBootstrap =
    !configExists ||
    failedChecks.some((name) =>
      ["config", "db", "index", "version"].includes(name)
    );

  const needsReindex =
    health?.checks.version?.status === "fail" ||
    health?.checks.index?.status === "fail";

  return {
    needsBootstrap,
    needsReindex,
    needsMcp: !mcpConfigured || Boolean(health && health.overall !== "ok"),
    needsLiveIndexing: !liveIndexingRunning && stateLiveIndexing,
    health,
  };
}
