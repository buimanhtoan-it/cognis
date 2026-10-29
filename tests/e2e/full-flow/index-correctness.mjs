#!/usr/bin/env node
/**
 * Index-correctness E2E scenarios (cognis-one-toggle-engine task 19.3).
 *
 * Requirements 24.1–24.8, 11.2, 19.5. Real processes inside an Isolation_Root.
 *
 * Usage (from repo root):
 *   node tests/e2e/full-flow/index-correctness.mjs
 *   node tests/e2e/full-flow/index-correctness.mjs --binary target/debug/cognis.exe
 */

import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { platform } from "node:os";

import {
  Harness,
  ScenarioError,
  runWithHarness,
  sleep,
} from "./harness.mjs";
import {
  McpClient,
  engineName,
  endpointPort,
  findDefaultBinary,
  parseJsonStdout,
  portAccepts,
  runCli,
  teeStderr,
  waitUntil,
} from "./scenario.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));

const INTERRUPT_STAGES = [1, 5, 13];

function parseArgs(argv) {
  const out = { binary: "", help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--binary") {
      const v = argv[++i];
      if (!v) throw new Error("missing value for --binary");
      out.binary = v;
    } else if (a === "-h" || a === "--help") {
      out.help = true;
    } else if (a.startsWith("-")) {
      throw new Error(`unknown flag: ${a}`);
    }
  }
  return out;
}

function writeTree(root, files) {
  for (const [rel, body] of Object.entries(files)) {
    const full = join(root, rel);
    mkdirSync(dirname(full), { recursive: true });
    writeFileSync(full, body, "utf8");
  }
}

function removeTreeFile(root, rel) {
  try {
    unlinkSync(join(root, rel));
  } catch {
    // already gone
  }
}

function patchStubBackend(repo) {
  const cfg = join(repo, ".cognis", "config.yaml");
  const text = readFileSync(cfg, "utf8");
  writeFileSync(cfg, text.replace(/backend:\s*\S+/, "backend: stub"), "utf8");
}

function cognisEnv(h, ws, extra = {}) {
  const dbPath = join(ws, ".cognis", "uckg.db");
  return {
    COGNIS_REPO_ROOT: ws,
    COGNIS_DB_PATH: dbPath,
    COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0",
    COGNIS_AUDIT_LOG: join(h.iso.logs, `${basename(ws)}-audit.log`),
    ...extra,
  };
}

function dbDir(ws) {
  return join(ws, ".cognis");
}

function copyDb(fromDir, toDir) {
  mkdirSync(toDir, { recursive: true });
  for (const name of ["uckg.db", "uckg.db-wal", "uckg.db-shm"]) {
    const src = join(fromDir, name);
    const dest = join(toDir, name);
    if (existsSync(src)) {
      copyFileSync(src, dest);
    } else if (existsSync(dest)) {
      unlinkSync(dest);
    }
  }
}

function killPid(pid) {
  if (!pid) return;
  try {
    if (platform() === "win32") {
      execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], {
        stdio: "ignore",
        windowsHide: true,
      });
    } else {
      process.kill(pid, "SIGKILL");
    }
  } catch {
    // already gone
  }
}

function extractHits(structured) {
  if (!structured || typeof structured !== "object") return [];
  if (Array.isArray(structured.result)) return structured.result;
  if (Array.isArray(structured)) return structured;
  return [];
}

function isTruncatedOrDeadline(structured, resp) {
  if (structured?.truncated === true) return true;
  const envelope = structured?.error ?? resp?.result?.structuredContent?.error;
  if (envelope && typeof envelope === "object") {
    const msg = `${envelope.code ?? ""} ${envelope.message ?? ""}`.toLowerCase();
    return (
      msg.includes("deadline") ||
      msg.includes("timeout") ||
      msg.includes("wall time") ||
      envelope.retryable === true
    );
  }
  if (resp?.error) return true;
  return false;
}

function openSqlite(dbPath) {
  return import("node:sqlite").then(({ DatabaseSync }) => new DatabaseSync(dbPath));
}

async function digestOf(h, binary, ws, env) {
  const out = await runCli(h, binary, ["--repo-root", ws, "digest"], env);
  return parseJsonStdout(out.stdout, "digest");
}

async function digestDiff(h, binary, leftWs, right, env) {
  const rightDb =
    /\.db$/i.test(right) && existsSync(right)
      ? right
      : join(right, ".cognis", "uckg.db");
  const out = await runCli(
    h,
    binary,
    ["--repo-root", leftWs, "digest", "--diff", rightDb],
    env
  );
  return parseJsonStdout(out.stdout, "digest-diff");
}

async function initAndIndex(h, binary, ws, env, { full = true, skipEmbeddings = true } = {}) {
  mkdirSync(ws, { recursive: true });
  await runCli(h, binary, ["--repo-root", ws, "init"], env);
  const args = ["--repo-root", ws, "index"];
  if (full) args.push("--full");
  if (skipEmbeddings) args.push("--skip-embeddings");
  await runCli(h, binary, args, env, 180_000);
}

async function startOwner(h, binary, ws, env) {
  const owner = h.spawn(
    binary,
    ["owner", "--repo-root", ws, "--host", "127.0.0.1", "--port", "0"],
    env,
    { cwd: ws }
  );
  const tag = basename(ws);
  owner.stdout?.pipe(createWriteStream(join(h.iso.logs, `${tag}-owner.stdout.log`)));
  teeStderr(owner, join(h.iso.logs, `${tag}-owner.stderr.log`));
  const endpointPath = join(ws, ".cognis", "mcpd.endpoint");
  const endpointText = await waitUntil(`${tag} owner endpoint`, 60_000, async () => {
    if (owner.exitCode !== null) {
      throw new ScenarioError(`${tag} owner endpoint`, {
        exitCode: owner.exitCode,
        stderr: existsSync(join(h.iso.logs, `${tag}-owner.stderr.log`))
          ? readFileSync(join(h.iso.logs, `${tag}-owner.stderr.log`), "utf8").slice(0, 4000)
          : "",
      });
    }
    if (!existsSync(endpointPath)) return null;
    const text = readFileSync(endpointPath, "utf8");
    const port = endpointPort(text);
    if (!port) return null;
    if (!(await portAccepts(port))) return null;
    return text;
  });
  return { owner, endpointText };
}

async function startMcp(h, binary, ws, env) {
  const mcpd = h.spawn(binary, ["mcpd"], env, { cwd: ws, stdin: "pipe" });
  teeStderr(mcpd, join(h.iso.logs, `${basename(ws)}-mcpd.stderr.log`));
  const mcp = new McpClient(mcpd);
  await mcp.initialize();
  return mcp;
}

async function assertDigestsEqual(h, binary, leftWs, rightWs, env, step) {
  const left = await digestOf(h, binary, leftWs, env);
  const right = await digestOf(h, binary, rightWs, env);
  if (left.combined === right.combined) {
    return;
  }
  const diff = await digestDiff(h, binary, leftWs, rightWs, env);
  throw new ScenarioError(step, {
    differing_categories: diff.differing_categories,
    rows: diff.rows,
    left,
    right,
  });
}

async function digestEquality(h, binary) {
  const inc = h.workspacePath("digest-inc");
  const reb = h.workspacePath("digest-reb");
  const envInc = cognisEnv(h, inc);
  const envReb = cognisEnv(h, reb);
  const alpha = {
    "src/alpha.py": "def digest_alpha():\n    return 1\n",
  };
  const edited = {
    "src/alpha.py": "def digest_alpha():\n    return 2\n",
    "src/beta.py": "def digest_beta():\n    return 3\n",
  };

  await h.step("digest-equality: initial", async () => {
    writeTree(inc, alpha);
    writeTree(reb, alpha);
    await initAndIndex(h, binary, inc, envInc);
    await initAndIndex(h, binary, reb, envReb);
    await assertDigestsEqual(h, binary, inc, reb, envInc, "digest-equality initial");
  });

  await h.step("digest-equality: add+edit", async () => {
    writeTree(inc, edited);
    await runCli(h, binary, ["--repo-root", inc, "index", "--skip-embeddings"], envInc, 180_000);
    rmSync(reb, { recursive: true, force: true });
    writeTree(reb, edited);
    await initAndIndex(h, binary, reb, envReb);
    await assertDigestsEqual(h, binary, inc, reb, envInc, "digest-equality add+edit");
  });

  await h.step("digest-equality: delete", async () => {
    removeTreeFile(inc, "src/beta.py");
    await runCli(h, binary, ["--repo-root", inc, "index", "--skip-embeddings"], envInc, 180_000);
    rmSync(reb, { recursive: true, force: true });
    writeTree(reb, { "src/alpha.py": edited["src/alpha.py"] });
    await initAndIndex(h, binary, reb, envReb);
    await assertDigestsEqual(h, binary, inc, reb, envInc, "digest-equality delete");
  });

  await h.step("digest-equality: rename", async () => {
    const renamed = {
      "src/gamma.py": "def digest_alpha():\n    return 2\n",
    };
    removeTreeFile(inc, "src/alpha.py");
    writeTree(inc, renamed);
    await runCli(h, binary, ["--repo-root", inc, "index", "--skip-embeddings"], envInc, 180_000);
    rmSync(reb, { recursive: true, force: true });
    writeTree(reb, renamed);
    await initAndIndex(h, binary, reb, envReb);
    await assertDigestsEqual(h, binary, inc, reb, envInc, "digest-equality rename");
  });
}

async function crashDuringPublish(h, binary) {
  const ws = h.workspacePath("crash-publish");
  const expected = h.workspacePath("crash-expected");
  const env = cognisEnv(h, ws);
  const envExp = cognisEnv(h, expected);
  const beforeSrc = { "src/crash_alpha.py": "def crash_alpha():\n    return 1\n" };
  const afterSrc = { "src/crash_alpha.py": "def crash_alpha():\n    return 2\n" };

  await h.step("crash-during-publish: baseline", async () => {
    writeTree(ws, beforeSrc);
    await initAndIndex(h, binary, ws, env);
    writeTree(expected, afterSrc);
    await initAndIndex(h, binary, expected, envExp);
  });

  const digestG = await digestOf(h, binary, ws, env);
  const digestG1 = await digestOf(h, binary, expected, envExp);
  const backup = join(h.iso.logs, "crash-db-backup");
  copyDb(dbDir(ws), backup);

  for (const stage of INTERRUPT_STAGES) {
    await h.step(`crash-during-publish: stage ${stage}`, async () => {
      copyDb(backup, dbDir(ws));
      writeTree(ws, afterSrc);
      const ready = join(h.iso.logs, `crash-ready-${stage}.txt`);
      try {
        unlinkSync(ready);
      } catch {
        // ignore
      }
      const child = h.spawn(
        binary,
        ["--repo-root", ws, "index", "--skip-embeddings"],
        {
          ...env,
          COGNIS_PUBLISH_INTERRUPT_STAGE: String(stage),
          COGNIS_PUBLISH_INTERRUPT_READY: ready,
        },
        { cwd: ws }
      );
      teeStderr(child, join(h.iso.logs, `crash-index-${stage}.stderr.log`));
      await waitUntil(`crash ready stage ${stage}`, 60_000, async () => {
        if (child.exitCode !== null && !existsSync(ready)) {
          throw new ScenarioError(`crash ready stage ${stage}`, {
            exitCode: child.exitCode,
            stderr: existsSync(join(h.iso.logs, `crash-index-${stage}.stderr.log`))
              ? readFileSync(join(h.iso.logs, `crash-index-${stage}.stderr.log`), "utf8").slice(
                  0,
                  4000
                )
              : "",
          });
        }
        return existsSync(ready) ? true : null;
      });
      killPid(child.pid);
      await sleep(500);
      const crashed = await digestOf(h, binary, ws, env);
      const ok =
        crashed.combined === digestG.combined || crashed.combined === digestG1.combined;
      if (!ok) {
        throw new ScenarioError(`crash-during-publish stage ${stage}`, {
          stage,
          crashed,
          previous: digestG,
          next: digestG1,
          vsPrevious: await digestDiff(h, binary, ws, join(backup, "uckg.db"), env),
          vsNext: await digestDiff(h, binary, ws, expected, env),
        });
      }
    });
  }
}

async function resumeEmbedding(h, binary) {
  const ws = h.workspacePath("resume-embed");
  const envIndex = cognisEnv(h, ws, { COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0" });
  await h.step("resume-embedding", async () => {
    writeTree(ws, { "src/resume.py": "def resume_embed_token():\n    return 1\n" });
    mkdirSync(ws, { recursive: true });
    await runCli(h, binary, ["--repo-root", ws, "init"], envIndex);
    // Default backend is `local` (ONNX). Lazy index demand-load fails offline and
    // records the symbols in the durable Embedding_Queue (Requirement 11.1).
    await runCli(h, binary, ["--repo-root", ws, "index", "--full"], envIndex, 180_000);

    const dbPath = join(ws, ".cognis", "uckg.db");
    const sqlite = await openSqlite(dbPath);
    let queued;
    try {
      queued = sqlite.prepare("SELECT COUNT(*) AS c FROM embedding_queue").get().c;
    } finally {
      sqlite.close();
    }
    h.assert("seeded embedding queue is non-empty", Number(queued) > 0, { queued });

    patchStubBackend(ws);
    const envOwner = cognisEnv(h, ws, { COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "1" });
    const { owner } = await startOwner(h, binary, ws, envOwner);
    await sleep(1500);
    killPid(owner.pid);
    await sleep(400);
    const after = await openSqlite(dbPath);
    let remaining;
    let maxAttempts;
    try {
      remaining = after.prepare("SELECT COUNT(*) AS c FROM embedding_queue").get().c;
      maxAttempts = after
        .prepare("SELECT COALESCE(MAX(attempts), 0) AS a FROM embedding_queue")
        .get().a;
    } finally {
      after.close();
    }
    h.assert(
      "owner resumed embedding without a user command",
      Number(remaining) === 0 || Number(maxAttempts) >= 1,
      { queued, remaining, maxAttempts }
    );
  });
}

async function fingerprintSwap(h, binary) {
  const ws = h.workspacePath("fp-swap");
  const env = cognisEnv(h, ws, { COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "1" });
  await h.step("fingerprint-swap", async () => {
    writeTree(ws, { "src/fp.py": "def fingerprint_swap_token():\n    return 1\n" });
    await runCli(h, binary, ["--repo-root", ws, "init"], env);
    patchStubBackend(ws);
    await runCli(h, binary, ["--repo-root", ws, "index", "--full"], env, 180_000);

    const dbPath = join(ws, ".cognis", "uckg.db");
    const sqlite = await openSqlite(dbPath);
    try {
      const row = sqlite
        .prepare("SELECT fingerprint, embedding_dim FROM vector_space WHERE id = 1")
        .get();
      h.assert("vector_space row present", Boolean(row?.fingerprint), row);
      const swapped = row.fingerprint.replace(/[0-9a-f]/g, (ch, i) =>
        i === 0 ? (ch === "a" ? "b" : "a") : ch
      );
      const next = swapped === row.fingerprint ? `aa${row.fingerprint.slice(2)}` : swapped;
      sqlite
        .prepare("UPDATE vector_space SET fingerprint = ? WHERE id = 1")
        .run(next);
    } finally {
      sqlite.close();
    }

    const envOwner = cognisEnv(h, ws, { COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP: "0" });
    const { owner } = await startOwner(h, binary, ws, envOwner);
    const mcp = await startMcp(h, binary, ws, envOwner);
    const semantic = await mcp.callTool("semantic_search", {
      query: "fingerprint_swap_token",
      k: 8,
    });
    h.assert(
      "semantic_available is false after fingerprint swap",
      semantic.structured?.semantic_available === false,
      semantic.structured
    );
    h.assert(
      "semantic results withheld",
      extractHits(semantic.structured).length === 0,
      semantic.structured
    );
    const lexical = await mcp.callTool("symbol_search", {
      query: "fingerprint_swap_token",
      k: 8,
    });
    h.assert(
      "lexical results still returned",
      extractHits(lexical.structured).length >= 1,
      lexical.structured
    );
    try {
      mcp.child.stdin?.end();
    } catch {
      // ignore
    }
    killPid(owner.pid);
    await sleep(300);
  });
}

async function concurrentLoad(h, binary) {
  const ws = h.workspacePath("concurrent");
  const env = cognisEnv(h, ws, {
    COGNIS_MCP_SOFT_TIMEOUT_S: "0",
    COGNIS_MCP_HARD_TIMEOUT_S: "10",
  });
  await h.step("concurrent-load", async () => {
    writeTree(ws, { "src/load.py": "def concurrent_load_token():\n    return 1\n" });
    await initAndIndex(h, binary, ws, env);
    const { owner } = await startOwner(h, binary, ws, env);
    const clients = [];
    for (let i = 0; i < 4; i += 1) {
      clients.push(await startMcp(h, binary, ws, env));
    }
    const results = await Promise.all(
      clients.map((mcp) =>
        mcp.callTool("symbol_search", { query: "concurrent_load_token", k: 8 }).catch((error) => ({
          error,
        }))
      )
    );
    for (const [i, result] of results.entries()) {
      if (result.error) {
        throw result.error;
      }
      h.assert(
        `concurrent call ${i} truncated or deadline-exceeded`,
        isTruncatedOrDeadline(result.structured, result.resp),
        result.structured ?? result.resp
      );
    }
    for (const mcp of clients) {
      try {
        mcp.child.stdin?.end();
      } catch {
        // ignore
      }
    }
    killPid(owner.pid);
    await sleep(300);
  });
}

async function disconnectMidCall(h, binary) {
  const ws = h.workspacePath("disconnect");
  const env = cognisEnv(h, ws, { COGNIS_E2E_STAGE_SLEEP_MS: "400" });
  await h.step("disconnect-mid-call", async () => {
    writeTree(ws, { "src/disc.py": "def disconnect_mid_token():\n    return 1\n" });
    await initAndIndex(h, binary, ws, env);
    const { owner } = await startOwner(h, binary, ws, env);
    const mcp = await startMcp(h, binary, ws, env);
    const genBefore = JSON.parse(
      readFileSync(join(ws, ".cognis", "owner-status.json"), "utf8")
    ).sourceGeneration;
    const pending = mcp.callTool("diffuse_context", {
      query: "disconnect_mid_token",
      k: 8,
    });
    await sleep(200);
    killPid(mcp.child.pid);
    const settled = pending.then(
      (value) => ({ value }),
      (error) => ({ error })
    );
    await Promise.race([settled, sleep(2000)]);
    await sleep(400);
    const status = JSON.parse(readFileSync(join(ws, ".cognis", "owner-status.json"), "utf8"));
    h.assert(
      "source_generation unchanged after disconnect",
      status.sourceGeneration === genBefore,
      { genBefore, status }
    );
    killPid(owner.pid);
    await sleep(300);
  });
}

async function editDuringCall(h, binary) {
  const ws = h.workspacePath("edit-call");
  const env = cognisEnv(h, ws, { COGNIS_E2E_STAGE_SLEEP_MS: "500" });
  await h.step("edit-during-call", async () => {
    writeTree(ws, { "src/edit.py": "def edit_during_alpha():\n    return 1\n" });
    await initAndIndex(h, binary, ws, env);
    const { owner } = await startOwner(h, binary, ws, env);
    const mcp = await startMcp(h, binary, ws, env);
    const genBefore = JSON.parse(
      readFileSync(join(ws, ".cognis", "owner-status.json"), "utf8")
    ).sourceGeneration;
    const pending = mcp.callTool("symbol_search", { query: "edit_during_alpha", k: 8 });
    await sleep(150);
    writeTree(ws, {
      "src/edit.py": "def edit_during_alpha():\n    return 1\n\ndef edit_during_omega():\n    return 2\n",
    });
    await runCli(h, binary, ["--repo-root", ws, "index", "--skip-embeddings"], env, 180_000);
    const { structured } = await pending;
    const pinned = structured?.source_generation;
    h.assert("Freshness_Metadata pins a generation", typeof pinned === "number", structured);
    h.assert(
      "results consistent with pinned generation (pre-edit)",
      pinned === genBefore,
      { pinned, genBefore, structured, hits: extractHits(structured) }
    );
    const blob = extractHits(structured)
      .map((hit) => `${hit.name ?? ""} ${hit.snippet ?? ""} ${hit.body_excerpt ?? ""}`)
      .join(" ")
      .toLowerCase();
    h.assert(
      "pinned generation does not include post-edit omega symbol",
      !blob.includes("edit_during_omega"),
      { blob, structured }
    );
    try {
      mcp.child.stdin?.end();
    } catch {
      // ignore
    }
    killPid(owner.pid);
    await sleep(300);
  });
}

async function runAll(hostBinary) {
  await runWithHarness(
    async (h) => {
      const dest = join(h.iso.bin, engineName());
      copyFileSync(hostBinary, dest);
      h.assertUnderIsolation("engine dest", dest);
      await digestEquality(h, dest);
      await crashDuringPublish(h, dest);
      await resumeEmbedding(h, dest);
      await fingerprintSwap(h, dest);
      await concurrentLoad(h, dest);
      await disconnectMidCall(h, dest);
      await editDuringCall(h, dest);
    },
    { timeoutMs: 600_000 }
  );
}

async function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(String(error.message ?? error));
    process.exitCode = 2;
    return;
  }
  if (opts.help) {
    console.log(`Index-correctness E2E (task 19.3)

Usage:
  node tests/e2e/full-flow/index-correctness.mjs [--binary PATH]
`);
    return;
  }
  const hostBinary = resolve(opts.binary || findDefaultBinary());
  if (!hostBinary || !existsSync(hostBinary)) {
    console.error(
      "error: no Engine_Binary found. Pass --binary or build with: cargo build -p cognis"
    );
    process.exitCode = 2;
    return;
  }
  console.log(`[index-correctness] binary: ${hostBinary}`);
  try {
    await runAll(hostBinary);
    console.log("[index-correctness] OK");
  } catch (error) {
    if (error instanceof ScenarioError) {
      console.error(`[index-correctness] FAILED at "${error.step}": ${error.message}`);
    } else {
      console.error(error);
    }
    process.exitCode = 1;
  }
}

const isMain =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isMain) {
  main();
}
