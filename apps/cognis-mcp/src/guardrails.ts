/**
 * Real-time Algorithmic Complexity, Optimization & Change-Impact Guardrails.
 *
 * Bridges Cognis's Speculative Shadow AST + CSIA (Code Spreading-Impact Analysis)
 * engine into VS Code / Cursor's native `DiagnosticCollection` and `CodeLens`
 * surfaces so both human developers and AI coding agents (which read
 * `vscode.languages.getDiagnostics()` after every edit) receive continuous,
 * sub-second feedback on:
 *   - Time & Space Big-O complexity per function/method
 *   - Actionable optimization advisories (`OPT-001`..`OPT-007`)
 *   - Inter-procedural blast radius, loop-amplified callers, and contract breakage
 *
 * Pure analysis & formatting helpers live at the top of this module with no
 * direct runtime requirement on a live VS Code window so they are unit-testable
 * under `node --test`.
 */
import * as path from "node:path";
import * as vscode from "vscode";
import { callMcpTool } from "./mcpServer";
import { getWorkspaceFolder, isWorkspaceConfigured } from "./workspace";

// ---------------------------------------------------------------------------
// Pure data types & fast local analyzer (unit-testable in plain Node)
// ---------------------------------------------------------------------------

export interface GuardrailAdvisory {
  ruleId: string;
  severity: "info" | "warning" | "error";
  title: string;
  currentComplexity: string;
  targetComplexity: string;
  line: number;
  rationale: string;
  suggestedPattern: string;
  reuseCandidates?: string[];
}

export interface FunctionComplexityLens {
  name: string;
  line: number;
  endLine: number;
  timeComplexity: string;
  spaceComplexity: string;
  maxLoopDepth: number;
  isRecursive: boolean;
  impactSeverity?: string;
  impactScore?: number;
  blastRadiusCallers?: number;
  advisories: GuardrailAdvisory[];
}

export interface DocumentGuardrailAnalysis {
  functions: FunctionComplexityLens[];
  advisories: GuardrailAdvisory[];
  overallSeverity?: string;
  overallImpactScore?: number;
}

const SUPPORTED_LANGUAGE_IDS = new Set([
  "typescript",
  "typescriptreact",
  "javascript",
  "javascriptreact",
  "python",
  "rust",
  "go",
  "java",
  "csharp",
  "c",
  "cpp",
  "ruby",
  "php",
]);

const FUNCTION_DECL_PATTERNS: RegExp[] = [
  // TS/JS/Rust/Go/Kotlin/Swift
  /^\s*(?:export\s+)?(?:default\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|function|func)\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]/,
  // Python
  /^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(/,
  // TS/JS/Java/C# class methods
  /^\s*(?:public|private|protected|internal|static|async|override|virtual|\s)+([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*\)\s*(?::\s*[^{]+)?\{/,
];

const LOOP_LINE_PATTERN =
  /\b(?:for|while)\s*\(|\bfor\s+[A-Za-z0-9_,\s]+\s+(?:in|of)\b|\bloop\s*\{|\.\s*(?:forEach|map|filter|flatMap|reduce|find|some|every)\s*\(/;

const LINEAR_SEARCH_IN_LOOP_PATTERN =
  /\.\s*(?:find|includes|indexOf|contains|any|position)\s*\(|\bin\s+[A-Za-z_][A-Za-z0-9_.]*/;

const IO_IN_LOOP_PATTERN =
  /\b(?:await\s+|fetch\s*\(|axios\.|db\.| prisma\.|sql|query\s*\(|execute\s*\(|readFile|writeFile|fs\.|http\.|reqwest|rusqlite)/;

const SORT_IN_LOOP_PATTERN = /\.\s*(?:sort|sort_by|sort_unstable|sorted)\s*\(/;

const STRING_CONCAT_IN_LOOP_PATTERN = /\b[A-Za-z_][A-Za-z0-9_]*\s*\+=\s*(?:["'`]|String|format!)/;

/**
 * Fast, deterministic single-file complexity and optimization analyzer that
 * runs in <2ms on keystroke debounce before/alongside the full Rust MCP
 * `analyze_patch_impact` call.
 */
export function analyzeSourceLocally(source: string): DocumentGuardrailAnalysis {
  const lines = source.split(/\r?\n/);
  const functions: FunctionComplexityLens[] = [];
  const allAdvisories: GuardrailAdvisory[] = [];

  interface ActiveFn {
    name: string;
    startLine: number;
    braceDepthAtStart: number;
    indentAtStart: number;
    isPythonDef: boolean;
    maxLoopDepth: number;
    hasAllocInLoop: boolean;
    isRecursive: boolean;
    advisories: GuardrailAdvisory[];
  }

  let currentFn: ActiveFn | null = null;
  let braceDepth = 0;
  const loopBraceStack: number[] = [];
  const pythonLoopIndentStack: number[] = [];

  const finalizeCurrentFn = (endLine: number) => {
    if (!currentFn) {
      return;
    }
    let timeComplexity = "O(1)";
    if (currentFn.isRecursive && currentFn.maxLoopDepth >= 1) {
      timeComplexity = "O(2^n)";
    } else if (currentFn.maxLoopDepth === 1) {
      timeComplexity = "O(n)";
    } else if (currentFn.maxLoopDepth === 2) {
      timeComplexity = "O(n^2)";
    } else if (currentFn.maxLoopDepth >= 3) {
      timeComplexity = `O(n^${currentFn.maxLoopDepth})`;
    } else if (currentFn.isRecursive) {
      timeComplexity = "O(n)";
    }

    if (
      currentFn.maxLoopDepth >= 2 &&
      !currentFn.advisories.some((a) => a.ruleId === "OPT-001")
    ) {
      const adv: GuardrailAdvisory = {
        ruleId: "OPT-001",
        severity: currentFn.maxLoopDepth >= 3 ? "error" : "warning",
        title: `Nested loop complexity ${timeComplexity} in '${currentFn.name}'`,
        currentComplexity: timeComplexity,
        targetComplexity: "O(n)",
        line: currentFn.startLine,
        rationale: `Function '${currentFn.name}' nests loops to depth ${currentFn.maxLoopDepth} (${timeComplexity}).`,
        suggestedPattern:
          "Pre-index inner collection into a HashMap/HashSet/Map before the outer loop to reduce lookup to O(1).",
      };
      currentFn.advisories.push(adv);
      allAdvisories.push(adv);
    }

    const spaceComplexity = currentFn.isRecursive
      ? "O(n)"
      : currentFn.hasAllocInLoop
      ? "O(n)"
      : "O(1)";

    functions.push({
      name: currentFn.name,
      line: currentFn.startLine,
      endLine,
      timeComplexity,
      spaceComplexity,
      maxLoopDepth: currentFn.maxLoopDepth,
      isRecursive: currentFn.isRecursive,
      advisories: currentFn.advisories,
    });
    currentFn = null;
  };

  for (let idx = 0; idx < lines.length; idx += 1) {
    const rawLine = lines[idx];
    const lineNum = idx + 1;
    const trimmed = rawLine.trim();
    if (
      !trimmed ||
      trimmed.startsWith("//") ||
      trimmed.startsWith("#") ||
      trimmed.startsWith("/*") ||
      trimmed.startsWith("*")
    ) {
      continue;
    }

    const indent = rawLine.search(/\S|$/);

    // Close Python function or loop scopes on dedent
    if (currentFn?.isPythonDef && indent <= currentFn.indentAtStart && lineNum > currentFn.startLine) {
      finalizeCurrentFn(lineNum - 1);
      pythonLoopIndentStack.length = 0;
    }
    while (
      pythonLoopIndentStack.length > 0 &&
      indent <= pythonLoopIndentStack[pythonLoopIndentStack.length - 1]
    ) {
      pythonLoopIndentStack.pop();
    }

    // Check if a new function starts on this line
    let matchedFnName: string | null = null;
    let isPythonDef = false;
    for (const pattern of FUNCTION_DECL_PATTERNS) {
      const m = rawLine.match(pattern);
      if (m && m[1] && !["if", "for", "while", "switch", "catch", "match"].includes(m[1])) {
        matchedFnName = m[1];
        isPythonDef = /^\s*(?:async\s+)?def\s+/.test(rawLine);
        break;
      }
    }

    if (matchedFnName) {
      finalizeCurrentFn(Math.max(1, lineNum - 1));
      loopBraceStack.length = 0;
      pythonLoopIndentStack.length = 0;
      currentFn = {
        name: matchedFnName,
        startLine: lineNum,
        braceDepthAtStart: braceDepth,
        indentAtStart: indent,
        isPythonDef,
        maxLoopDepth: 0,
        hasAllocInLoop: false,
        isRecursive: false,
        advisories: [],
      };
    }

    // Detect loop entry on this line
    const isLoopLine = LOOP_LINE_PATTERN.test(rawLine);
    if (isLoopLine) {
      if (currentFn?.isPythonDef || trimmed.endsWith(":")) {
        pythonLoopIndentStack.push(indent);
      } else {
        loopBraceStack.push(braceDepth);
      }
    }

    const activeLoopDepth =
      loopBraceStack.length + pythonLoopIndentStack.length +
      (isLoopLine && !rawLine.includes("{") && !trimmed.endsWith(":") ? 1 : 0);

    if (currentFn) {
      if (activeLoopDepth > currentFn.maxLoopDepth) {
        currentFn.maxLoopDepth = activeLoopDepth;
      }

      // Check direct recursion after declaration line
      if (lineNum > currentFn.startLine) {
        const selfCallRe = new RegExp(`\\b${currentFn.name}\\s*\\(`);
        if (selfCallRe.test(rawLine)) {
          currentFn.isRecursive = true;
        }
      }

      if (activeLoopDepth >= 1) {
        // OPT-001: linear scan inside loop
        if (LINEAR_SEARCH_IN_LOOP_PATTERN.test(rawLine) && !isLoopLine) {
          const adv: GuardrailAdvisory = {
            ruleId: "OPT-001",
            severity: "warning",
            title: "Linear lookup inside loop causes O(n^2) time complexity",
            currentComplexity: "O(n^2)",
            targetComplexity: "O(n)",
            line: lineNum,
            rationale: `Line ${lineNum} performs a linear scan inside a loop of depth ${activeLoopDepth}.`,
            suggestedPattern:
              "Lift inner collection into a Set/Map/HashSet outside the loop and use O(1) lookup.",
          };
          currentFn.maxLoopDepth = Math.max(currentFn.maxLoopDepth, activeLoopDepth + 1);
          currentFn.advisories.push(adv);
          allAdvisories.push(adv);
        }

        // OPT-002: sort inside loop
        if (SORT_IN_LOOP_PATTERN.test(rawLine)) {
          const adv: GuardrailAdvisory = {
            ruleId: "OPT-002",
            severity: "warning",
            title: "Sorting inside a loop causes O(n^2 log n) complexity",
            currentComplexity: "O(n^2 log n)",
            targetComplexity: "O(n log n)",
            line: lineNum,
            rationale: `Line ${lineNum} sorts a collection repeatedly inside a loop.`,
            suggestedPattern:
              "Hoist sorting before the loop or maintain a BinaryHeap / BTreeSet across iterations.",
          };
          currentFn.advisories.push(adv);
          allAdvisories.push(adv);
        }

        // OPT-003: I/O or DB/network call inside loop (N+1 query pattern)
        if (IO_IN_LOOP_PATTERN.test(rawLine)) {
          const adv: GuardrailAdvisory = {
            ruleId: "OPT-003",
            severity: "error",
            title: "N+1 I/O or database/network operation inside loop",
            currentComplexity: "O(n) I/O",
            targetComplexity: "O(1) batched I/O",
            line: lineNum,
            rationale: `Line ${lineNum} executes synchronous/awaited I/O inside a loop (depth ${activeLoopDepth}).`,
            suggestedPattern:
              "Collect keys/requests in the loop and execute a single batched query or bulk I/O call outside the loop.",
          };
          currentFn.advisories.push(adv);
          allAdvisories.push(adv);
        }

        // OPT-005: String concatenation in loop
        if (STRING_CONCAT_IN_LOOP_PATTERN.test(rawLine)) {
          currentFn.hasAllocInLoop = true;
          const adv: GuardrailAdvisory = {
            ruleId: "OPT-005",
            severity: "info",
            title: "Repeated string concatenation inside loop",
            currentComplexity: "O(n^2)",
            targetComplexity: "O(n)",
            line: lineNum,
            rationale: `Line ${lineNum} appends to a string inside a loop, risking repeated buffer reallocations.`,
            suggestedPattern:
              "Pre-allocate buffer capacity or collect fragments into an array and join once.",
          };
          currentFn.advisories.push(adv);
          allAdvisories.push(adv);
        }
      }
    }

    // Update brace depth and pop closed loops/functions
    for (const ch of rawLine) {
      if (ch === "{") {
        braceDepth += 1;
      } else if (ch === "}") {
        braceDepth = Math.max(0, braceDepth - 1);
        while (
          loopBraceStack.length > 0 &&
          braceDepth <= loopBraceStack[loopBraceStack.length - 1]
        ) {
          loopBraceStack.pop();
        }
        if (
          currentFn &&
          !currentFn.isPythonDef &&
          braceDepth <= currentFn.braceDepthAtStart &&
          lineNum > currentFn.startLine
        ) {
          finalizeCurrentFn(lineNum);
        }
      }
    }
  }

  finalizeCurrentFn(lines.length);
  return { functions, advisories: allAdvisories };
}

/**
 * Format a guardrail advisory into the structured `[Big-O] + [Blast Radius] + [Optimal Fix]`
 * diagnostic message consumed by both human developers and AI coding agents.
 */
export function formatGuardrailDiagnostic(
  adv: GuardrailAdvisory,
  impactSeverity?: string,
  blastRadiusCallers?: number
): string {
  const impactBadge =
    impactSeverity && impactSeverity !== "low"
      ? ` | Impact: ${impactSeverity.toUpperCase()}${
          blastRadiusCallers ? ` (${blastRadiusCallers} callers)` : ""
        }`
      : "";
  const reuseSuffix =
    adv.reuseCandidates && adv.reuseCandidates.length > 0
      ? ` Reuse existing helper: ${adv.reuseCandidates.join(", ")}.`
      : "";
  return `[Cognis ${adv.ruleId}] ${adv.title} (${adv.currentComplexity} → ${adv.targetComplexity}${impactBadge}). Fix: ${adv.suggestedPattern}${reuseSuffix}`;
}

/**
 * Parse the JSON payload returned by the `analyze_patch_impact` MCP tool and
 * merge its inter-procedural complexity, CSIA blast radius, and optimization
 * advisories into a `DocumentGuardrailAnalysis`.
 */
export function parsePatchImpactMcpResult(
  rawResult: unknown,
  fallback: DocumentGuardrailAnalysis
): DocumentGuardrailAnalysis {
  if (!rawResult || typeof rawResult !== "object") {
    return fallback;
  }
  const envelope = rawResult as {
    content?: Array<{ type?: string; text?: string }>;
  };
  const firstText = envelope.content?.[0]?.text;
  if (!firstText) {
    return fallback;
  }

  try {
    const parsed = JSON.parse(firstText) as {
      complexity_reports?: Array<{
        qualified_name?: string;
        line_start?: number;
        line_end?: number;
        propagated_time?: string;
        space_complexity?: string;
        max_loop_depth?: number;
        recursion_kind?: string;
        advisories?: Array<{
          rule_id?: string;
          severity?: string;
          title?: string;
          current_complexity?: string;
          target_complexity?: string;
          hotspot_line?: number;
          problem?: string;
          prescription?: string;
          repo_asset_ref?: string;
        }>;
      }>;
      impact_report?: {
        impact_level?: string;
        composite_risk_score?: number;
        reached_symbol_count?: number;
        complexity_amplifications?: Array<{
          caller_name?: string;
          cumulative_loop_depth?: number;
          amplification_factor?: number;
          time_before?: string;
          time_after?: string;
          call_chain?: string[];
        }>;
        breaking_call_sites?: Array<{
          caller_name?: string;
          caller_line?: number;
          target_symbol?: string;
          reason?: string;
        }>;
      };
      symbol_reports?: Array<{
        qualified_name?: string;
        line_start?: number;
        line_end?: number;
        total_time?: { notation?: string };
        total_space?: { notation?: string };
        ast_summary?: {
          max_loop_depth?: number;
          is_direct_recursive?: boolean;
          is_mutual_recursive?: boolean;
        };
        advisories?: Array<{
          rule_id?: string;
          severity?: string;
          title?: string;
          current_complexity?: string;
          target_complexity?: string;
          line?: number;
          rationale?: string;
          suggested_pattern?: string;
          reuse_candidates?: string[];
        }>;
      }>;
      impact?: {
        severity?: string;
        overall_impact_score?: number;
        affected_callers?: Array<unknown>;
        complexity_amplifications?: Array<{
          caller_qualified_name?: string;
          modified_symbol?: string;
          enclosing_loop_depth?: number;
          amplification_factor?: number;
          caller_old_complexity?: string;
          caller_new_complexity?: string;
        }>;
        contract_breakages?: Array<{
          caller_qualified_name?: string;
          caller_line?: number;
          callee_qualified_name?: string;
          reason?: string;
        }>;
      };
    };

    const impactSeverity =
      parsed.impact_report?.impact_level ?? parsed.impact?.severity ?? "low";
    const impactScore =
      parsed.impact_report?.composite_risk_score ??
      parsed.impact?.overall_impact_score ??
      0;
    const blastRadiusCallers =
      parsed.impact_report?.reached_symbol_count ??
      parsed.impact?.affected_callers?.length ??
      0;

    const functions: FunctionComplexityLens[] = [];
    const advisories: GuardrailAdvisory[] = [];

    for (const rep of parsed.complexity_reports ?? []) {
      const fnAdvisories: GuardrailAdvisory[] = (rep.advisories ?? []).map((a) => {
        const mapped: GuardrailAdvisory = {
          ruleId: a.rule_id ?? "OPT-001",
          severity:
            a.severity === "critical" || a.severity === "error" || a.severity === "high"
              ? "error"
              : a.severity === "warning" || a.severity === "medium"
              ? "warning"
              : "info",
          title: a.title ?? "Algorithmic optimization available",
          currentComplexity: a.current_complexity ?? "O(n^2)",
          targetComplexity: a.target_complexity ?? "O(n)",
          line: a.hotspot_line ?? rep.line_start ?? 1,
          rationale: a.problem ?? "",
          suggestedPattern: a.prescription ?? "",
          reuseCandidates: a.repo_asset_ref ? [a.repo_asset_ref] : [],
        };
        advisories.push(mapped);
        return mapped;
      });

      const shortName =
        (rep.qualified_name ?? "symbol").split("::").pop()?.split(".").pop() ??
        "symbol";

      functions.push({
        name: shortName,
        line: rep.line_start ?? 1,
        endLine: rep.line_end ?? rep.line_start ?? 1,
        timeComplexity: rep.propagated_time ?? "O(1)",
        spaceComplexity: rep.space_complexity ?? "O(1)",
        maxLoopDepth: rep.max_loop_depth ?? 0,
        isRecursive:
          Boolean(rep.recursion_kind) && rep.recursion_kind !== "none",
        impactSeverity,
        impactScore,
        blastRadiusCallers,
        advisories: fnAdvisories,
      });
    }

    for (const rep of parsed.symbol_reports ?? []) {
      const fnAdvisories: GuardrailAdvisory[] = (rep.advisories ?? []).map((a) => {
        const mapped: GuardrailAdvisory = {
          ruleId: a.rule_id ?? "OPT-001",
          severity:
            a.severity === "critical" || a.severity === "error"
              ? "error"
              : a.severity === "warning"
              ? "warning"
              : "info",
          title: a.title ?? "Algorithmic optimization available",
          currentComplexity: a.current_complexity ?? "O(n^2)",
          targetComplexity: a.target_complexity ?? "O(n)",
          line: a.line ?? rep.line_start ?? 1,
          rationale: a.rationale ?? "",
          suggestedPattern: a.suggested_pattern ?? "",
          reuseCandidates: a.reuse_candidates ?? [],
        };
        advisories.push(mapped);
        return mapped;
      });

      const shortName =
        (rep.qualified_name ?? "symbol").split("::").pop()?.split(".").pop() ??
        "symbol";

      functions.push({
        name: shortName,
        line: rep.line_start ?? 1,
        endLine: rep.line_end ?? rep.line_start ?? 1,
        timeComplexity: rep.total_time?.notation ?? "O(1)",
        spaceComplexity: rep.total_space?.notation ?? "O(1)",
        maxLoopDepth: rep.ast_summary?.max_loop_depth ?? 0,
        isRecursive: Boolean(
          rep.ast_summary?.is_direct_recursive || rep.ast_summary?.is_mutual_recursive
        ),
        impactSeverity,
        impactScore,
        blastRadiusCallers,
        advisories: fnAdvisories,
      });
    }

    // Surface inter-procedural loop amplification paths as high-visibility advisories
    for (const amp of parsed.impact_report?.complexity_amplifications ?? []) {
      const targetSym = amp.call_chain?.[amp.call_chain.length - 1] ?? "symbol";
      advisories.push({
        ruleId: "CSIA-AMP",
        severity: "warning",
        title: `Caller '${amp.caller_name ?? "caller"}' invokes '${targetSym}' inside loop (depth ${
          amp.cumulative_loop_depth ?? 1
        }, ${amp.amplification_factor ?? 4}x amplification)`,
        currentComplexity: amp.time_after ?? "O(n^2)",
        targetComplexity: amp.time_before ?? "O(n)",
        line: functions[0]?.line ?? 1,
        rationale: "Complexity regression is amplified along caller loop chain.",
        suggestedPattern:
          "Batch call outside caller loop or keep callee complexity at O(1).",
      });
    }
    for (const amp of parsed.impact?.complexity_amplifications ?? []) {
      advisories.push({
        ruleId: "CSIA-AMP",
        severity: "warning",
        title: `Caller '${amp.caller_qualified_name ?? "caller"}' invokes '${
          amp.modified_symbol ?? "symbol"
        }' inside loop (depth ${amp.enclosing_loop_depth ?? 1}, ${
          amp.amplification_factor ?? 4
        }x amplification)`,
        currentComplexity: amp.caller_new_complexity ?? "O(n^2)",
        targetComplexity: amp.caller_old_complexity ?? "O(n)",
        line: functions[0]?.line ?? 1,
        rationale: `Complexity regression is amplified along caller loop chain.`,
        suggestedPattern:
          "Batch call outside caller loop or keep callee complexity at O(1).",
      });
    }

    // Surface contract breakages
    for (const brk of parsed.impact_report?.breaking_call_sites ?? []) {
      advisories.push({
        ruleId: "CSIA-CONTRACT",
        severity: "error",
        title: `Signature change breaks caller '${brk.caller_name ?? "caller"}'`,
        currentComplexity: "breaking",
        targetComplexity: "compatible",
        line: functions[0]?.line ?? 1,
        rationale: brk.reason ?? "Signature changed with active downstream callers.",
        suggestedPattern:
          "Update downstream call sites or provide a backwards-compatible overload/default parameter.",
      });
    }
    for (const brk of parsed.impact?.contract_breakages ?? []) {
      advisories.push({
        ruleId: "CSIA-CONTRACT",
        severity: "error",
        title: `Signature change breaks caller '${brk.caller_qualified_name ?? "caller"}'`,
        currentComplexity: "breaking",
        targetComplexity: "compatible",
        line: functions[0]?.line ?? 1,
        rationale: brk.reason ?? "Signature changed with active downstream callers.",
        suggestedPattern:
          "Update downstream call sites or provide a backwards-compatible overload/default parameter.",
      });
    }

    if (functions.length === 0) {
      return {
        ...fallback,
        overallSeverity: impactSeverity,
        overallImpactScore: impactScore,
      };
    }

    return {
      functions,
      advisories: advisories.length > 0 ? advisories : fallback.advisories,
      overallSeverity: impactSeverity,
      overallImpactScore: impactScore,
    };
  } catch {
    return fallback;
  }
}

// ---------------------------------------------------------------------------
// VS Code DiagnosticCollection + CodeLens Controller
// ---------------------------------------------------------------------------

export class CognisGuardrailController implements vscode.CodeLensProvider, vscode.Disposable {
  private readonly diagnostics: vscode.DiagnosticCollection | undefined;
  private readonly codeLensListeners = new Set<() => void>();
  public readonly onDidChangeCodeLenses: vscode.Event<void> = (listener) => {
    this.codeLensListeners.add(listener);
    return {
      dispose: () => {
        this.codeLensListeners.delete(listener);
      },
    };
  };

  private readonly cache = new Map<string, DocumentGuardrailAnalysis>();
  private readonly debounceTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly disposables: vscode.Disposable[] = [];

  constructor() {
    if (vscode.languages?.createDiagnosticCollection) {
      this.diagnostics = vscode.languages.createDiagnosticCollection("cognis");
      this.disposables.push(this.diagnostics);
    }
    if (vscode.languages?.registerCodeLensProvider) {
      this.disposables.push(
        vscode.languages.registerCodeLensProvider({ scheme: "file" }, this)
      );
    }
    if (vscode.workspace?.onDidChangeTextDocument) {
      this.disposables.push(
        vscode.workspace.onDidChangeTextDocument((e) => {
          if (this.shouldAnalyze(e.document)) {
            this.scheduleLocalAnalysis(e.document);
          }
        })
      );
    }
    if (vscode.workspace?.onDidSaveTextDocument) {
      this.disposables.push(
        vscode.workspace.onDidSaveTextDocument((doc) => {
          if (this.shouldAnalyze(doc) && this.isAnalyzeOnSaveEnabled()) {
            void this.analyzeDocumentWithMcp(doc);
          }
        })
      );
    }
    if (vscode.workspace?.onDidChangeConfiguration) {
      this.disposables.push(
        vscode.workspace.onDidChangeConfiguration((e) => {
          if (e.affectsConfiguration("cognis.guardrails")) {
            if (!this.isGuardrailsEnabled()) {
              this.cache.clear();
              this.diagnostics?.clear();
            } else {
              for (const editor of vscode.window?.visibleTextEditors ?? []) {
                if (this.shouldAnalyze(editor.document)) {
                  this.runLocalAnalysis(editor.document);
                }
              }
            }
            for (const listener of [...this.codeLensListeners]) {
              listener();
            }
          }
        })
      );
    }
    if (vscode.workspace?.onDidOpenTextDocument) {
      this.disposables.push(
        vscode.workspace.onDidOpenTextDocument((doc) => {
          if (this.shouldAnalyze(doc)) {
            this.runLocalAnalysis(doc);
          }
        })
      );
    }
    if (vscode.workspace?.onDidCloseTextDocument) {
      this.disposables.push(
        vscode.workspace.onDidCloseTextDocument((doc) => {
          const key = doc.uri.toString();
          const timer = this.debounceTimers.get(key);
          if (timer) {
            clearTimeout(timer);
            this.debounceTimers.delete(key);
          }
          this.cache.delete(key);
          this.diagnostics?.delete(doc.uri);
        })
      );
    }

    // Analyze currently visible editors immediately on activation
    for (const editor of vscode.window?.visibleTextEditors ?? []) {
      if (this.shouldAnalyze(editor.document)) {
        this.runLocalAnalysis(editor.document);
      }
    }
  }

  private isGuardrailsEnabled(): boolean {
    return (
      vscode.workspace
        ?.getConfiguration?.("cognis")
        ?.get<boolean>("guardrails.enabled", true) ?? true
    );
  }

  private isCodeLensEnabled(): boolean {
    return (
      vscode.workspace
        ?.getConfiguration?.("cognis")
        ?.get<boolean>("guardrails.codeLens", true) ?? true
    );
  }

  private isAnalyzeOnSaveEnabled(): boolean {
    return (
      vscode.workspace
        ?.getConfiguration?.("cognis")
        ?.get<boolean>("guardrails.analyzeOnSave", true) ?? true
    );
  }

  private shouldAnalyze(doc: vscode.TextDocument): boolean {
    if (!this.isGuardrailsEnabled()) {
      return false;
    }
    if (doc.uri.scheme !== "file") {
      return false;
    }
    return SUPPORTED_LANGUAGE_IDS.has(doc.languageId);
  }

  private scheduleLocalAnalysis(doc: vscode.TextDocument): void {
    const key = doc.uri.toString();
    const existing = this.debounceTimers.get(key);
    if (existing) {
      clearTimeout(existing);
    }
    const timer = setTimeout(() => {
      this.debounceTimers.delete(key);
      this.runLocalAnalysis(doc);
    }, 250);
    this.debounceTimers.set(key, timer);
  }

  private runLocalAnalysis(doc: vscode.TextDocument): DocumentGuardrailAnalysis {
    const analysis = analyzeSourceLocally(doc.getText());
    this.publishAnalysis(doc, analysis);
    return analysis;
  }

  public async analyzeDocumentWithMcp(doc: vscode.TextDocument): Promise<void> {
    const localAnalysis = this.runLocalAnalysis(doc);
    const folder = getWorkspaceFolder();
    if (!folder || !isWorkspaceConfigured(folder.uri.fsPath)) {
      return;
    }
    const repoRoot = folder.uri.fsPath;
    const relPath = path.relative(repoRoot, doc.uri.fsPath).replace(/\\/g, "/");
    if (relPath.startsWith("..")) {
      return;
    }

    const res = await callMcpTool(
      repoRoot,
      "analyze_patch_impact",
      {
        file_path: relPath,
        new_content: doc.getText(),
      },
      { timeoutMs: 5_000 }
    );
    if (!res.ok || !res.result) {
      return;
    }
    const merged = parsePatchImpactMcpResult(res.result, localAnalysis);
    this.publishAnalysis(doc, merged);
  }

  private publishAnalysis(
    doc: vscode.TextDocument,
    analysis: DocumentGuardrailAnalysis
  ): void {
    this.cache.set(doc.uri.toString(), analysis);

    if (this.diagnostics) {
      const vscodeDiagnostics: vscode.Diagnostic[] = [];
      for (const adv of analysis.advisories) {
        const lineIdx = Math.max(0, Math.min(doc.lineCount - 1, adv.line - 1));
        const lineText = doc.lineAt(lineIdx).text;
        const startCol = Math.max(0, lineText.search(/\S|$/));
        const range = new vscode.Range(
          lineIdx,
          startCol,
          lineIdx,
          Math.max(startCol + 1, lineText.length)
        );
        const severity =
          adv.severity === "error"
            ? vscode.DiagnosticSeverity.Warning
            : adv.severity === "warning"
            ? vscode.DiagnosticSeverity.Warning
            : vscode.DiagnosticSeverity.Information;

        const diag = new vscode.Diagnostic(
          range,
          formatGuardrailDiagnostic(
            adv,
            analysis.overallSeverity,
            analysis.functions[0]?.blastRadiusCallers
          ),
          severity
        );
        diag.source = "cognis-guardrail";
        diag.code = adv.ruleId;
        vscodeDiagnostics.push(diag);
      }

      this.diagnostics.set(doc.uri, vscodeDiagnostics);
    }
    for (const listener of [...this.codeLensListeners]) {
      listener();
    }
  }

  public provideCodeLenses(
    document: vscode.TextDocument
  ): vscode.CodeLens[] {
    if (!this.shouldAnalyze(document) || !this.isCodeLensEnabled()) {
      return [];
    }
    const analysis =
      this.cache.get(document.uri.toString()) ?? this.runLocalAnalysis(document);
    const lenses: vscode.CodeLens[] = [];

    for (const fn of analysis.functions) {
      const lineIdx = Math.max(0, Math.min(document.lineCount - 1, fn.line - 1));
      const range = new vscode.Range(lineIdx, 0, lineIdx, 0);
      const topAdvisory = fn.advisories[0];
      const blastSuffix =
        fn.blastRadiusCallers && fn.blastRadiusCallers > 0
          ? ` · Blast: ${fn.blastRadiusCallers} caller(s)`
          : "";
      const advisorySuffix = topAdvisory
        ? ` · ⚠ ${topAdvisory.ruleId}: ${topAdvisory.currentComplexity} → ${topAdvisory.targetComplexity}`
        : "";

      lenses.push(
        new vscode.CodeLens(range, {
          title: `$(pulse) Cognis: Time ${fn.timeComplexity} · Space ${fn.spaceComplexity}${blastSuffix}${advisorySuffix}`,
          tooltip: topAdvisory
            ? `${topAdvisory.title}\nSuggested fix: ${topAdvisory.suggestedPattern}`
            : `Estimated complexity for ${fn.name}: Time ${fn.timeComplexity}, Space ${fn.spaceComplexity}`,
          command: "cognis.openPanel",
        })
      );
    }

    return lenses;
  }

  public dispose(): void {
    for (const timer of this.debounceTimers.values()) {
      clearTimeout(timer);
    }
    this.debounceTimers.clear();
    this.cache.clear();
    this.codeLensListeners.clear();
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}
