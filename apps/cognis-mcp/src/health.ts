import { runCli } from "./cli";
import { trace } from "./diagnostics";
import type { HealthReport } from "./types";

export async function fetchHealth(repoRoot: string): Promise<HealthReport> {
  const result = await runCli(repoRoot, ["health", "--json"]);
  const text = result.stdout.trim();
  const jsonStart = text.indexOf("{");
  const jsonText = jsonStart >= 0 ? text.slice(jsonStart) : text;
  try {
    return JSON.parse(jsonText) as HealthReport;
  } catch (err) {
    trace.error("contract", "health --json returned unparseable JSON", {
      repoRoot,
      exitCode: result.exitCode,
      bytes: jsonText.length,
      head: jsonText.slice(0, 200),
      error: err instanceof Error ? err.message : String(err),
    });
    throw new Error(
      `cognis health --json returned output the extension could not parse as JSON. ` +
        "See Cognis: Show Diagnostics Log for details."
    );
  }
}

export function formatHealthSummary(report: HealthReport): string {
  const lines = [`Overall: ${report.overall}`, `Runtime: ${report.runtime_version}`];
  for (const [name, check] of Object.entries(report.checks)) {
    lines.push(`  ${name}: ${check.status} — ${check.message}`);
  }
  return lines.join("\n");
}
