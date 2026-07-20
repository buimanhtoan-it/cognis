import * as assert from "node:assert";
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import * as readline from "node:readline";

import * as vscode from "vscode";

const EXTENSION_ID = "ToanBui.cognis-vscode";

interface IndexdStatus {
  pid?: number;
  active?: boolean;
  phase?: string;
  progress_percent?: number;
  pending_count?: number;
  inflight_count?: number;
  last_error?: string | null;
  updated_at?: number;
}

function readStatus(workspace: string): IndexdStatus | undefined {
  try {
    return JSON.parse(
      fs.readFileSync(
        path.join(workspace, ".cognis", "indexd-status.json"),
        "utf8"
      )
    ) as IndexdStatus;
  } catch {
    return undefined;
  }
}

function isAlive(pid: number | undefined): boolean {
  if (!pid || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitFor<T>(
  probe: () => T | undefined,
  timeoutMs: number,
  intervalMs = 500
): Promise<T | undefined> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = probe();
    if (value !== undefined) {
      return value;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  return probe();
}

function diagnosticsTail(): string {
  const dir = process.env.COGNIS_DIAGNOSTICS_DIR;
  const file = dir ? path.join(dir, "diagnostics.jsonl") : "";
  if (!file || !fs.existsSync(file)) {
    return "<no diagnostics.jsonl>";
  }
  return fs.readFileSync(file, "utf8").trim().split(/\r?\n/u).slice(-80).join("\n");
}

function isolatedEnv(
  workspace: string,
  dbPath: string
): Record<string, string | undefined> {
  const env: Record<string, string | undefined> = { ...process.env };
  delete env.COGNIS_MCP_FIXTURE;
  delete env.COGNIS_INDEXD_STATUS_PATH;
  delete env.COGNIS_ONNX_MODEL_DIR;
  env.COGNIS_DB_PATH = dbPath;
  env.COGNIS_REPO_ROOT = workspace;
  env.COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP = "0";
  return env;
}

class LiveMcpClient {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly lines: readline.Interface;
  private readonly pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (err: Error) => void }
  >();
  private nextId = 0;
  private stderr = "";

  constructor(binary: string, workspace: string, dbPath: string) {
    this.child = spawn(binary, ["mcpd"], {
      cwd: workspace,
      env: isolatedEnv(workspace, dbPath),
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stderr.on("data", (chunk: Buffer) => {
      this.stderr = (this.stderr + chunk.toString()).slice(-16_000);
    });
    this.child.on("error", (err) => this.rejectAll(err));
    this.child.on("exit", (code, signal) => {
      this.rejectAll(
        new Error(
          `mcpd exited before replying (code=${String(code)}, signal=${String(
            signal
          )})\n${this.stderr}`
        )
      );
    });
    this.lines = readline.createInterface({ input: this.child.stdout });
    this.lines.on("line", (line) => {
      const response = JSON.parse(line) as { id?: unknown };
      if (typeof response.id !== "number") {
        return;
      }
      const waiter = this.pending.get(response.id);
      if (waiter) {
        this.pending.delete(response.id);
        waiter.resolve(response);
      }
    });
  }
  private rejectAll(err: Error): void {
    for (const waiter of this.pending.values()) {
      waiter.reject(err);
    }
    this.pending.clear();
  }

  async request(method: string, params: unknown): Promise<any> {
    const id = ++this.nextId;
    const response = new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`mcpd timed out for ${method}\n${this.stderr}`));
      }, 30_000);
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      });
    });
    this.child.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"
    );
    return response;
  }

  notify(method: string): void {
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method }) + "\n");
  }

  async call(tool: string, args: Record<string, unknown>): Promise<any> {
    const response = await this.request("tools/call", {
      name: tool,
      arguments: args,
    });
    assert.equal(response.error, undefined, JSON.stringify(response));
    const text = response.result?.content?.[0]?.text;
    assert.equal(typeof text, "string", `invalid ${tool} response: ${JSON.stringify(response)}`);
    return JSON.parse(text);
  }

  async close(): Promise<void> {
    this.lines.close();
    this.child.stdin.end();
    if (!this.child.killed) {
      this.child.kill();
    }
    await Promise.race([
      new Promise<void>((resolve) => this.child.once("exit", () => resolve())),
      new Promise<void>((resolve) => setTimeout(resolve, 5_000)),
    ]);
    if (isAlive(this.child.pid)) {
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/PID", String(this.child.pid), "/T", "/F"], {
          stdio: "ignore",
        });
      } else if (this.child.pid) {
        process.kill(this.child.pid, "SIGKILL");
      }
    }
  }
}

function exactHit(
  hits: any[],
  name: string,
  expectedPath: string
): any | undefined {
  return hits.find(
    (hit) =>
      hit?.name === name &&
      typeof hit.file_path === "string" &&
      hit.file_path.replace(/\\/gu, "/") === expectedPath
  );
}

suite("Practical large-codebase: real VS Code + real Cognis repository corpus", () => {
  test("cold setup indexes tracked Rust/TS and serves exact MCP retrieval", async function () {
    this.timeout(900_000);
    const workspace = process.env.COGNIS_HOST_LARGE_WORKSPACE;
    const binary = process.env.COGNIS_BINARY_PATH;
    assert.ok(workspace, "large-host runner did not set workspace");
    assert.ok(binary && fs.existsSync(binary), "real Cognis binary is required; no skip");

    const expectedFiles = Number(process.env.COGNIS_HOST_LARGE_EXPECTED_FILES);
    assert.ok(expectedFiles >= 100, `large corpus is too small: ${expectedFiles}`);
    for (const rel of [
      "crates/cognis-store/src/lib.rs",
      "crates/cognis-indexer/src/pipeline.rs",
      "crates/cognis-core/src/warm_policy.rs",
      "apps/cognis-vscode/src/extension.ts",
      "apps/cognis-vscode/src/panel.ts",
    ]) {
      assert.ok(fs.existsSync(path.join(workspace, ...rel.split("/"))), `missing ${rel}`);
    }
    assert.ok(!fs.existsSync(path.join(workspace, ".benchmarks")));
    assert.ok(!fs.existsSync(path.join(workspace, "target")));
    assert.ok(!fs.existsSync(path.join(workspace, "node_modules")));

    const ext = vscode.extensions.getExtension(EXTENSION_ID);
    assert.ok(ext, `extension ${EXTENSION_ID} not loaded by the real host`);
    await ext.activate();

    let commandError: unknown;
    const coldStarted = Date.now();
    void Promise.resolve(vscode.commands.executeCommand("cognis.setupWorkspace")).catch(
      (err) => {
        commandError = err;
      }
    );
    const settled = await waitFor(() => {
      const status = readStatus(workspace);
      if (status?.last_error) {
        throw new Error(`indexd reported ${status.last_error}`);
      }
      return status?.phase === "watching" &&
        status.active === true &&
        status.progress_percent === 100 &&
        status.pending_count === 0 &&
        status.inflight_count === 0 &&
        isAlive(status.pid)
        ? status
        : undefined;
    }, 600_000);
    assert.ok(
      settled,
      `cold index did not reach authoritative watching/100/settled/live state` +
        `${commandError ? `; command error=${String(commandError)}` : ""}\n` +
        `status=${JSON.stringify(readStatus(workspace))}\n${diagnosticsTail()}`
    );

    const configPath = path.join(workspace, ".cognis", "config.yaml");
    const mcpPath = path.join(workspace, ".cursor", "mcp.json");
    const dbPath = path.join(workspace, ".cognis", "uckg.db");
    assert.ok(fs.existsSync(configPath), "real setup did not create config.yaml");
    assert.ok(fs.existsSync(mcpPath), "real setup did not write workspace mcp.json");
    assert.ok(fs.existsSync(dbPath), "real indexd did not create uckg.db");
    console.log(
      `[large-host] cold setup/index settled in ${(
        (Date.now() - coldStarted) /
        1000
      ).toFixed(1)}s (pid=${settled.pid})`
    );

    const health = spawnSync(binary, ["cli", "--repo-root", workspace, "health", "--json"], {
      cwd: workspace,
      encoding: "utf8",
      env: isolatedEnv(workspace, dbPath),
    });
    assert.equal(health.error, undefined, `health spawn failed: ${String(health.error)}`);
    assert.equal(health.status, 0, `health failed:\n${health.stdout}\n${health.stderr}`);
    const healthJson = JSON.parse(health.stdout) as {
      checks?: { index?: { status?: string; message?: string } };
    };
    const indexMessage = healthJson.checks?.index?.message ?? "";
    const symbolCount = Number(indexMessage.match(/^(\d+) symbols indexed/u)?.[1]);
    assert.equal(healthJson.checks?.index?.status, "ok", indexMessage);
    assert.ok(
      symbolCount >= 500,
      `large corpus indexed only ${symbolCount} symbols; expected at least 500`
    );
    console.log(`[large-host] UCKG contains ${symbolCount} symbols`);

    const mcp = new LiveMcpClient(binary, workspace, dbPath);
    try {
      const initialized = await mcp.request("initialize", {});
      assert.equal(initialized.result?.contractVersion, 1, JSON.stringify(initialized));
      mcp.notify("notifications/initialized");

      const databaseHits = (await mcp.call("symbol_search", {
        query: "Database",
        k: 20,
      })) as any[];
      const database = exactHit(
        databaseHits,
        "Database",
        "crates/cognis-store/src/lib.rs"
      );
      assert.ok(database, `exact Database hit missing: ${JSON.stringify(databaseHits)}`);

      const pipelineHits = (await mcp.call("symbol_search", {
        query: "IndexerPipeline",
        k: 20,
      })) as any[];
      assert.ok(
        exactHit(
          pipelineHits,
          "IndexerPipeline",
          "crates/cognis-indexer/src/pipeline.rs"
        ),
        `exact IndexerPipeline hit missing: ${JSON.stringify(pipelineHits)}`
      );

      const panelHits = (await mcp.call("symbol_search", {
        query: "CognisPanelProvider",
        k: 20,
      })) as any[];
      assert.ok(
        exactHit(
          panelHits,
          "CognisPanelProvider",
          "apps/cognis-vscode/src/panel.ts"
        ),
        `exact TypeScript hit missing: ${JSON.stringify(panelHits)}`
      );

      const lookup = await mcp.call("symbol_lookup", {
        name_or_id: database.symbol_id,
      });
      assert.equal(lookup.id, database.symbol_id);
      assert.equal(lookup.name, "Database");
      assert.equal(
        String(lookup.file_path).replace(/\\/gu, "/"),
        "crates/cognis-store/src/lib.rs"
      );

      const fromEnvHits = (await mcp.call("symbol_search", {
        query: "from_env",
        k: 30,
      })) as any[];
      const fromEnv = exactHit(
        fromEnvHits,
        "from_env",
        "crates/cognis-core/src/warm_policy.rs"
      );
      assert.ok(fromEnv, `exact from_env hit missing: ${JSON.stringify(fromEnvHits)}`);
      const trace = await mcp.call("dependency_trace", {
        symbol_id: fromEnv.symbol_id,
        direction: "out",
        depth: 1,
      });
      assert.ok(
        (trace.hits as any[]).some(
          (hit) =>
            hit.qualified_name ===
            "rs:crates/cognis-core/src/warm_policy.rs:SemanticWarmPolicy.from_env_value"
        ),
        `structural edge from_env -> from_env_value missing: ${JSON.stringify(trace)}`
      );
      console.log(
        "[large-host] exact MCP gates: Database, IndexerPipeline, " +
          "CognisPanelProvider, symbol_lookup, from_env -> from_env_value"
      );
    } finally {
      await mcp.close();
    }
    const traced = await waitFor(() => {
      const found = diagnosticsTail()
        .split(/\r?\n/u)
        .some((line) => {
          try {
            const entry = JSON.parse(line) as { scope?: string; message?: string };
            return (
              entry.scope === "flow" &&
              entry.message === "Cognis: Set Up Workspace ok"
            );
          } catch {
            return false;
          }
        });
      return found ? true : undefined;
    }, 10_000);
    assert.ok(traced, `successful setup flow trace missing\n${diagnosticsTail()}`);

    const indexdPid = settled.pid;
    await vscode.commands.executeCommand("cognis.cancelIndexing");
    const stopped = await waitFor(
      () => (!isAlive(indexdPid) ? true : undefined),
      10_000,
      200
    );
    assert.ok(stopped, `indexd pid ${indexdPid} survived cancelIndexing`);
  });
});
