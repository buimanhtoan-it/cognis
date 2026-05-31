import { runCliJson } from "./cli";
import type { HealthReport } from "./types";

export async function fetchHealth(repoRoot: string): Promise<HealthReport> {
  return runCliJson<HealthReport>(repoRoot, ["health", "--json"]);
}

export function formatHealthSummary(report: HealthReport): string {
  const lines = [`Overall: ${report.overall}`, `Runtime: ${report.runtime_version}`];
  for (const [name, check] of Object.entries(report.checks)) {
    lines.push(`  ${name}: ${check.status} — ${check.message}`);
  }
  return lines.join("\n");
}
