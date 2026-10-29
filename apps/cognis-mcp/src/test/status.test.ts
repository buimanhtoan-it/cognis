// Harness first: status.ts imports diagnostics.ts, which imports vscode.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { trace } from "../diagnostics";
import {
  deriveStatusState,
  deriveStatusSnapshot,
  startStatusSurfaceRefresh,
  statusPollCeilingMs,
  statusReadBudgetMs,
  type StatusInput,
} from "../status";
import type { HealthReport } from "../types";

function okHealth(overrides: Partial<HealthReport> = {}): HealthReport {
  return {
    runtime_version: "0.9.0",
    overall: "ok",
    checks: {
      config: { status: "ok", message: "ok" },
      db: { status: "ok", message: "ok" },
      index: { status: "ok", message: "ok" },
    },
    capabilities: {
      semantic_backend: true,
      accelerated_vector_search: true,
    },
    source_generation: 4,
    semantic_generation: 4,
    pending_vector_count: 0,
    semantic_available: true,
    fingerprint_match: true,
    ...overrides,
  };
}

function readyFullInput(overrides: Partial<StatusInput> = {}): StatusInput {
  return {
    health: okHealth(),
    readErrors: [],
    ...overrides,
  };
}

function makeTempRepo(): string {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-status-surface-"));
  fs.mkdirSync(path.join(repo, ".cognis"), { recursive: true });
  return repo;
}

function cleanup(repoRoot: string): void {
  try {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

function initTrace(): { dir: string; read: () => Array<Record<string, unknown>> } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-status-diag-"));
  trace.init({ globalStorageUri: { fsPath: dir } } as never, "0.8.13");
  trace.setMinLevel("debug");
  const file = path.join(dir, "diagnostics.jsonl");
  return {
    dir,
    read: () =>
      fs.existsSync(file)
        ? fs
            .readFileSync(file, "utf8")
            .trim()
            .split("\n")
            .filter(Boolean)
            .map((line) => JSON.parse(line) as Record<string, unknown>)
        : [],
  };
}

function waitUntil(predicate: () => boolean, timeoutMs: number): Promise<boolean> {
  const started = Date.now();
  return new Promise((resolve) => {
    const tick = (): void => {
      if (predicate()) {
        resolve(true);
        return;
      }
      if (Date.now() - started >= timeoutMs) {
        resolve(false);
        return;
      }
      setTimeout(tick, 50);
    };
    tick();
  });
}

test("deriveStatusState reports Ready_Full only for the full conjunction", () => {
  const report = deriveStatusState(readyFullInput());
  assert.equal(report.state, "Ready_Full");
  assert.match(report.detail, /ready/i);
});

test("deriveStatusState names each Ready_Lexical degradation, including fallback mode", () => {
  const cases: Array<{ input: StatusInput; needle: RegExp }> = [
    {
      input: readyFullInput({
        health: okHealth({
          capabilities: {
            semantic_backend: false,
            accelerated_vector_search: false,
          },
          semantic_available: false,
        }),
      }),
      needle: /semantic backend is unavailable/i,
    },
    {
      input: readyFullInput({
        health: okHealth({ pending_vector_count: 12, semantic_available: true }),
      }),
      needle: /12 embeddings are still pending/i,
    },
    {
      input: readyFullInput({
        health: okHealth({
          source_generation: 8,
          semantic_generation: 3,
          pending_vector_count: 0,
        }),
      }),
      needle: /semantic generation is still catching up/i,
    },
    {
      input: readyFullInput({
        health: okHealth({ fingerprint_match: false, semantic_available: false }),
      }),
      needle: /fingerprint does not match/i,
    },
    {
      input: readyFullInput({
        health: okHealth({
          capabilities: {
            semantic_backend: true,
            accelerated_vector_search: false,
          },
        }),
      }),
      needle: /vector search running in fallback mode/i,
    },
  ];

  for (const { input, needle } of cases) {
    const report = deriveStatusState(input);
    assert.equal(report.state, "Ready_Lexical", report.detail);
    assert.match(report.detail, needle);
  }
});

test("deriveStatusState decision order: unreadable sources outrank Ready_Full and Working", () => {
  const report = deriveStatusState(
    readyFullInput({
      transition: "reconcile_and_index",
      readErrors: ["owner-status.json", "health"],
    })
  );
  assert.equal(report.state, "Needs_Attention");
  assert.match(report.detail, /owner-status\.json/);
  assert.match(report.detail, /health/);
  assert.equal(report.nextAction?.command, "cognis.showDiagnostics");
});

test("deriveStatusState decision order: blocking condition outranks Working", () => {
  const report = deriveStatusState(
    readyFullInput({
      transition: "verify_engine_binary",
      health: okHealth({ missing_indexes: ["idx_symbol_name_lower"] }),
    })
  );
  assert.equal(report.state, "Needs_Attention");
  assert.match(report.blockingCondition ?? "", /idx_symbol_name_lower/);
  assert.equal(report.nextAction?.command, "cognis.clearAndReindex");
});

test("deriveStatusState reports Working with phase name and both queue depths", () => {
  const report = deriveStatusState(
    readyFullInput({
      transition: "verify_model_assets",
      health: okHealth({ pending_vector_count: 11 }),
      owner: {
        active: true,
        phase: "embedding",
        message: "",
        pendingCount: 3,
        pendingFiles: [],
        inflightCount: 1,
        inflightFiles: [],
        recentFiles: [],
        updatedAt: 1_730_000_000,
        watcherQueueDepth: 7,
        pendingVectorCount: 11,
      },
    })
  );
  assert.equal(report.state, "Working");
  assert.equal(report.phase, "verify model assets");
  assert.equal(report.watcherQueueDepth, 7);
  assert.equal(report.embeddingQueueDepth, 11);
  assert.match(report.detail, /verify model assets/);
  assert.match(report.detail, /watcher queue 7/);
  assert.match(report.detail, /embedding queue 11/);
});

test("deriveStatusState treats a checksum failure as Needs_Attention with one next action", () => {
  const report = deriveStatusState(
    readyFullInput({
      owner: {
        active: false,
        phase: "idle",
        message: "engine checksum mismatch",
        pendingCount: 0,
        pendingFiles: [],
        inflightCount: 0,
        inflightFiles: [],
        recentFiles: [],
        updatedAt: 1_730_000_000,
        lastError: "checksum verification failed",
      },
    })
  );
  assert.equal(report.state, "Needs_Attention");
  assert.equal(report.blockingCondition, "Checksum verification failed");
  assert.equal(report.nextAction?.command, "cognis.installBackend");
});

test("deriveStatusState is a pure function of StatusInput", () => {
  const input = readyFullInput({
    lease: {
      owner_nonce: "abc",
      pid: 12,
      process_start_id: "start",
      heartbeat_at: 1_730_000_010,
      expiry: 1_730_000_040,
    },
  });
  assert.deepEqual(deriveStatusState(input), deriveStatusState(input));
});

test("status poll ceiling is at most 2s and the unreadability budget is 5s", () => {
  assert.ok(statusPollCeilingMs <= 2000);
  assert.equal(statusReadBudgetMs, 5000);
});

test("unreadable sources stay silent until the 5s budget, then become Needs_Attention", () => {
  const repo = makeTempRepo();
  try {
    const t0 = 10_000;
    const early = deriveStatusSnapshot({
      repoRoot: repo,
      health: undefined,
      healthReadFailed: true,
      mcpEnabled: false,
      configured: true,
      nowMs: t0,
    });
    assert.ok(!early.input.readErrors.includes("health"));

    const stillEarly = deriveStatusSnapshot({
      repoRoot: repo,
      health: undefined,
      healthReadFailed: true,
      mcpEnabled: false,
      configured: true,
      nowMs: t0 + statusReadBudgetMs - 1,
    });
    assert.ok(!stillEarly.input.readErrors.includes("health"));

    const late = deriveStatusSnapshot({
      repoRoot: repo,
      health: okHealth(),
      healthReadFailed: true,
      mcpEnabled: false,
      configured: true,
      nowMs: t0 + statusReadBudgetMs,
    });
    assert.ok(late.input.readErrors.includes("health"));
    assert.equal(late.report.state, "Needs_Attention");
    assert.match(late.report.detail, /health/);
    assert.notEqual(late.report.state, "Ready_Full");
  } finally {
    cleanup(repo);
  }
});

test("status transitions are appended to the diagnostics log with derived-from values", () => {
  const repo = makeTempRepo();
  const { read } = initTrace();
  try {
    deriveStatusSnapshot({
      repoRoot: repo,
      health: okHealth(),
      mcpEnabled: true,
      configured: true,
    });
    deriveStatusSnapshot({
      repoRoot: repo,
      health: okHealth(),
      mcpEnabled: true,
      configured: true,
    });
    deriveStatusSnapshot({
      repoRoot: repo,
      health: okHealth({ pending_vector_count: 4 }),
      mcpEnabled: true,
      configured: true,
    });

    const transitions = read().filter((entry) => {
      const data = entry.data as { repoRoot?: string } | undefined;
      return (
        entry.scope === "status" &&
        entry.message === "Status surface transition" &&
        data?.repoRoot === repo
      );
    });
    assert.equal(transitions.length, 2);
    const first = transitions[0]?.data as Record<string, unknown>;
    const second = transitions[1]?.data as Record<string, unknown>;
    assert.equal(first.state, "Ready_Full");
    assert.equal(second.state, "Ready_Lexical");
    assert.equal(typeof transitions[0]?.ts, "string");
    assert.ok(first.derivedFrom);
    assert.equal(
      (second.derivedFrom as { pendingVectorCount?: number }).pendingVectorCount,
      4
    );
  } finally {
    cleanup(repo);
  }
});

test("startStatusSurfaceRefresh notifies within the 2s ceiling when owner-status.json changes", async () => {
  const repo = makeTempRepo();
  let fired = 0;
  const handle = startStatusSurfaceRefresh({
    repoRoot: repo,
    onDidChange: () => {
      fired += 1;
    },
  });
  try {
    fs.writeFileSync(
      path.join(repo, ".cognis", "owner-status.json"),
      JSON.stringify({
        active: true,
        phase: "watching",
        message: "",
        pendingCount: 0,
        pendingFiles: [],
        inflightCount: 0,
        inflightFiles: [],
        recentFiles: [],
        updatedAt: Date.now() / 1000,
        sourceGeneration: 1,
      }),
      "utf8"
    );
    const observed = await waitUntil(() => fired > 0, statusPollCeilingMs + 750);
    assert.equal(observed, true, "status surface refresh must observe owner-status.json within 2s");
  } finally {
    handle.dispose();
    cleanup(repo);
  }
});
