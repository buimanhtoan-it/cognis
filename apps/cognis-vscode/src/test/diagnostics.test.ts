// Harness first: installs the vscode stub before diagnostics.ts (which imports
// vscode + cli.getOutputChannel) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import { trace } from "../diagnostics";

function freshTrace(): { dir: string; read: () => Array<Record<string, unknown>> } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-diag-"));
  // Minimal ExtensionContext shape: trace.init only reads globalStorageUri.fsPath.
  trace.init({ globalStorageUri: { fsPath: dir } } as never, "9.9.9");
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

test("trace.info writes a structured JSONL entry with scope, message, data, version", () => {
  const { read } = freshTrace();
  trace.info("unit", "hello world", { count: 3 });
  const entries = read();
  const last = entries[entries.length - 1];
  assert.equal(last.level, "info");
  assert.equal(last.scope, "unit");
  assert.equal(last.message, "hello world");
  assert.equal(last.extVersion, "9.9.9");
  assert.deepEqual(last.data, { count: 3 });
  assert.equal(typeof last.ts, "string");
});

test("trace.span logs ok with a duration on success and returns the value", async () => {
  const { read } = freshTrace();
  const result = await trace.span("flow", "Set Up Workspace", async () => 42, {
    repoRoot: "/x",
  });
  assert.equal(result, 42);
  const ok = read().find((e) => e.scope === "flow" && e.message === "Set Up Workspace ok");
  assert.ok(ok, "expected a 'flow: Set Up Workspace ok' entry");
  assert.equal(ok!.level, "info");
  assert.equal(typeof ok!.durationMs, "number");
});

test("trace.span logs a failure entry and re-throws so control flow is unchanged", async () => {
  const { read } = freshTrace();
  await assert.rejects(
    () =>
      trace.span("flow", "Repair Setup", async () => {
        throw new Error("boom");
      }),
    /boom/
  );
  const fail = read().find((e) => e.scope === "flow" && e.message === "Repair Setup failed");
  assert.ok(fail, "expected a 'flow: Repair Setup failed' entry");
  assert.equal(fail!.level, "error");
  const data = fail!.data as Record<string, unknown>;
  assert.match(String(data.error), /boom/);
});

test("trace respects the minimum level (debug suppressed at info)", () => {
  const { read } = freshTrace();
  trace.setMinLevel("info");
  const before = read().length;
  trace.debug("unit", "should be suppressed");
  assert.equal(read().length, before, "debug entry must not be written at info level");
  trace.warn("unit", "should appear");
  assert.equal(read().length, before + 1);
  trace.setMinLevel("debug");
});
