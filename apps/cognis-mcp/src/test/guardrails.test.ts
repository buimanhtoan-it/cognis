/**
 * Unit tests for Cognis real-time complexity & change-impact guardrails
 * (`apps/cognis-mcp/src/guardrails.ts`).
 */
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";
import {
  analyzeSourceLocally,
  formatGuardrailDiagnostic,
  parsePatchImpactMcpResult,
} from "../guardrails";

test("analyzeSourceLocally detects O(n^2) nested loops and OPT-001 linear search in loop", () => {
  const tsSource = `
export function reconcileOrders(orders: Order[], blacklist: string[]) {
  const matched: Order[] = [];
  for (const order of orders) {
    if (blacklist.includes(order.customerId)) {
      matched.push(order);
    }
  }
  return matched;
}
`;
  const analysis = analyzeSourceLocally(tsSource);
  assert.equal(analysis.functions.length, 1);
  const fn = analysis.functions[0];
  assert.equal(fn.name, "reconcileOrders");
  assert.equal(fn.timeComplexity, "O(n^2)");
  assert.ok(fn.advisories.some((a) => a.ruleId === "OPT-001"));
});

test("analyzeSourceLocally detects OPT-003 N+1 I/O inside loop", () => {
  const tsSource = `
async function syncUsers(userIds: string[]) {
  for (const id of userIds) {
    await fetch("https://api.example.com/users/" + id);
  }
}
`;
  const analysis = analyzeSourceLocally(tsSource);
  assert.equal(analysis.functions.length, 1);
  assert.ok(analysis.advisories.some((a) => a.ruleId === "OPT-003"));
  const opt003 = analysis.advisories.find((a) => a.ruleId === "OPT-003")!;
  const msg = formatGuardrailDiagnostic(opt003, "high", 3);
  assert.match(msg, /\[Cognis OPT-003\]/);
  assert.match(msg, /Impact: HIGH \(3 callers\)/);
});

test("parsePatchImpactMcpResult merges inter-procedural CSIA blast radius and OPT advisories", () => {
  const fallback = analyzeSourceLocally("function helper() { return 1; }");
  const mcpEnvelope = {
    content: [
      {
        type: "text",
        text: JSON.stringify({
          symbol_reports: [
            {
              qualified_name: "orders.helper",
              line_start: 1,
              line_end: 10,
              total_time: { notation: "O(n^2)" },
              total_space: { notation: "O(n)" },
              ast_summary: {
                max_loop_depth: 2,
                is_direct_recursive: false,
                is_mutual_recursive: false,
              },
              advisories: [
                {
                  rule_id: "OPT-001",
                  severity: "warning",
                  title: "Pre-index inner lookup into HashSet/Map",
                  current_complexity: "O(n^2)",
                  target_complexity: "O(n)",
                  line: 4,
                  rationale: "Inner loop scans collection repeatedly.",
                  suggested_pattern: "Build a Map before the outer loop.",
                  reuse_candidates: ["utils.buildLookupMap (src/utils.ts:12)"],
                },
              ],
            },
          ],
          impact: {
            severity: "critical",
            overall_impact_score: 0.88,
            affected_callers: [{ symbol_id: "c1" }, { symbol_id: "c2" }],
            complexity_amplifications: [
              {
                caller_qualified_name: "api.handleBatch",
                modified_symbol: "orders.helper",
                enclosing_loop_depth: 1,
                amplification_factor: 4,
                caller_old_complexity: "O(n)",
                caller_new_complexity: "O(n^3)",
              },
            ],
            contract_breakages: [],
          },
        }),
      },
    ],
  };

  const merged = parsePatchImpactMcpResult(mcpEnvelope, fallback);
  assert.equal(merged.overallSeverity, "critical");
  assert.equal(merged.functions.length, 1);
  assert.equal(merged.functions[0].timeComplexity, "O(n^2)");
  assert.equal(merged.functions[0].blastRadiusCallers, 2);
  assert.ok(merged.advisories.some((a) => a.ruleId === "OPT-001"));
  assert.ok(merged.advisories.some((a) => a.ruleId === "CSIA-AMP"));
});

test("parsePatchImpactMcpResult parses canonical GuardrailCapsule payload from Rust MCP", () => {
  const fallback = analyzeSourceLocally("function reconcile() { return 1; }");
  const rustCapsuleEnvelope = {
    content: [
      {
        type: "text",
        text: JSON.stringify({
          verdict: "WARN",
          latency_ms: 4.2,
          generation: "speculative-shadow",
          summary_markdown: "### Guardrail Verdict",
          complexity_reports: [
            {
              symbol_id: "sym_1",
              qualified_name: "billing::reconcile",
              kind: "function",
              file_path: "src/billing.ts",
              line_start: 12,
              line_end: 34,
              local_time: "O(n^2)",
              propagated_time: "O(n^2)",
              space_complexity: "O(n)",
              effective_degree: 2.0,
              risk_score: 0.72,
              max_loop_depth: 2,
              cyclomatic: 4,
              cognitive: 6,
              recursion_kind: "none",
              io_in_loop: [],
              bottleneck_chain: [],
              advisories: [
                {
                  rule_id: "OPT-001",
                  title: "Linear lookup inside loop",
                  severity: "warning",
                  symbol_id: "sym_1",
                  qualified_name: "billing::reconcile",
                  file_path: "src/billing.ts",
                  hotspot_line: 18,
                  current_complexity: "O(n^2)",
                  target_complexity: "O(n)",
                  problem: "Repeated linear scan inside outer loop.",
                  prescription: "Pre-build a Map/Set before iterating.",
                  repo_asset_ref: "utils::indexById (src/utils.ts:8)",
                },
              ],
            },
          ],
          impact_report: {
            impact_level: "high",
            composite_risk_score: 0.81,
            dimensions: {
              structural_blast_radius: 0.4,
              complexity_amplification: 0.85,
              contract_breakage_risk: 0.5,
              test_coverage_gap: 0.2,
            },
            breaking_call_sites: [
              {
                caller_id: "sym_caller",
                caller_name: "api::postSettlement",
                caller_file: "src/api.ts",
                caller_line: 42,
                target_symbol: "billing::reconcile",
                reason: "Required parameter added",
                severity: 0.9,
              },
            ],
            complexity_amplifications: [
              {
                caller_id: "sym_batch",
                caller_name: "jobs::nightlyBatch",
                caller_file: "src/jobs.ts",
                caller_line: 90,
                cumulative_loop_depth: 1,
                amplification_factor: 4,
                time_before: "O(n^2)",
                time_after: "O(n^3)",
                call_chain: ["jobs::nightlyBatch", "billing::reconcile"],
              },
            ],
            affected_entrypoints: ["api::postSettlement"],
            minimal_test_suite: ["tests::testSettlement"],
            untested_high_risk_paths: [],
            reached_symbol_count: 5,
            reached_module_count: 3,
          },
          advisories: [],
        }),
      },
    ],
  };

  const merged = parsePatchImpactMcpResult(rustCapsuleEnvelope, fallback);
  assert.equal(merged.overallSeverity, "high");
  assert.equal(merged.functions.length, 1);
  assert.equal(merged.functions[0].name, "reconcile");
  assert.equal(merged.functions[0].timeComplexity, "O(n^2)");
  assert.equal(merged.functions[0].blastRadiusCallers, 5);
  assert.ok(merged.advisories.some((a) => a.ruleId === "OPT-001"));
  assert.ok(merged.advisories.some((a) => a.ruleId === "CSIA-AMP"));
  assert.ok(merged.advisories.some((a) => a.ruleId === "CSIA-CONTRACT"));
});
