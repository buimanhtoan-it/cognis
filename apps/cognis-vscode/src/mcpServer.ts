/**
 * Per-workspace HTTP MCP server lifecycle (panel-managed).
 *
 * Cognis ships ``cognis-mcpd`` with two transports:
 *   * stdio — the editor spawns and owns it (no URL, zero management).
 *   * http  — a standalone server with a localhost URL the editor connects to.
 *
 * This module manages the http mode. The user clicks Start in the panel and
 * Cognis spawns one ``cognis-mcpd --transport http`` per workspace, bound to a
 * deterministic localhost port; clicking Stop terminates it. The mcp.json is
 * written in the url form so the editor connects to whatever is running.
 *
 * The pure helpers (port derivation, command builder, state shape) live in
 * this file at the top with no VS Code dependency, so they are unit-testable
 * without a VS Code harness — that is where the regression bar sits.
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import * as crypto from "node:crypto";
import * as net from "node:net";
import * as path from "node:path";
import * as vscode from "vscode";

import { getOutputChannel } from "./cli";
import { resolveMcpdInvocation } from "./binary";
import {
  reconcileOrphanLease,
  removeLeaseForPid,
  verifyLeaseOwner,
  type OwnerVerification,
} from "./lease";
import { modelEnv } from "./model";

// ---------------------------------------------------------------------------
// Pure helpers (no VS Code, no spawn) — unit-testable in plain Node.
// ---------------------------------------------------------------------------

const LOOPBACK_HOST = "127.0.0.1";

/** Reserved-by-OS bands and the ephemeral range we steer clear of. */
const PORT_FLOOR = 49152;
const PORT_CEIL = 65535;

/**
 * Derive a deterministic localhost port from the repo path. Stable across
 * extension restarts so the URL the user pasted into mcp.json keeps working.
 *
 * Uses SHA-256 of the canonical (lowercased on Windows, normalized) repo path,
 * mod the size of the IANA "dynamic / private" port band [49152, 65535]. This
 * is large enough that workspaces will essentially never collide; we still
 * allow callers to step a few ports forward when binding fails.
 */
export function derivePort(repoRoot: string, offset: number = 0): number {
  const norm = path.normalize(repoRoot);
  const key = process.platform === "win32" ? norm.toLowerCase() : norm;
  const digest = crypto.createHash("sha256").update(key).digest();
  const span = PORT_CEIL - PORT_FLOOR + 1;
  const base = digest.readUInt32BE(0) % span;
  return PORT_FLOOR + ((base + offset) % span);
}

/** The URL clients (editor / curl / etc.) connect to. */
export function buildMcpUrl(host: string, port: number): string {
  return `http://${host}:${port}/mcp`;
}

/**
 * The mcp.json server block that points an editor at the running HTTP server.
 * ``type: "http"`` + ``url`` is the broadly-compatible streamable-http form
 * (VS Code / Cursor). Pure so the written config shape is unit-tested.
 */
export function buildHttpMcpServerBlock(url: string): { type: string; url: string } {
  return { type: "http", url };
}

/** True when an mcp.json server block is the HTTP (url) form, not stdio. */
export function isHttpServerBlock(block: unknown): boolean {
  return (
    typeof block === "object" &&
    block !== null &&
    typeof (block as { url?: unknown }).url === "string"
  );
}

/**
 * The flags (without the ``<binary> mcpd`` prefix) that put the mcpd surface
 * into HTTP transport on *host*:*port*. Kept in one place so the wire shape is
 * unit-testable and shared with ``resolveMcpdInvocation``.
 */
export function buildMcpdHttpFlags(host: string, port: number): string[] {
  return ["--transport", "http", "--host", host, "--port", String(port)];
}

/**
 * Env var that marks a cognis-mcpd process as a model-free thin stdio proxy
 * (Requirements 2.8, 2.11). Mirrored in `bins/cognis-mcpd/src/proxy.rs` and
 * re-exported from `mcpRuntime` for the process probe (kept free of the
 * VS Code API so unit tests can import it without a harness).
 */
export const THIN_PROXY_ENV = "COGNIS_MCP_PROXY";

/**
 * Env var holding the loopback HTTP URL of the heavy daemon a thin proxy
 * forwards to. Optional — when absent the proxy acquire-or-spawns the heavy.
 */
export const PROXY_TARGET_ENV = "COGNIS_MCP_PROXY_TARGET";

/**
 * How the editor-owned stdio server block launches mcpd.
 *
 * * ``"proxy"`` — thin stdio proxy: forwards JSON-RPC to one heavy daemon,
 *   loads no ONNX / holds no repository DB (Requirements 2.8, 2.11).
 * * ``"heavy"`` — legacy compatible path: the editor-spawned process is itself
 *   the heavy daemon (preservation 3.8 escape hatch).
 */
export type McpStdioMode = "proxy" | "heavy";

/**
 * The flags (without the ``<binary> mcpd`` prefix) that put the mcpd surface
 * into thin-proxy mode. The proxy speaks stdio to the editor and forwards to a
 * single heavy repository daemon over loopback HTTP.
 */
export function buildMcpdProxyFlags(targetUrl?: string): string[] {
  const flags = ["--proxy"];
  if (targetUrl && targetUrl.trim()) {
    flags.push("--proxy-target", targetUrl.trim());
  }
  return flags;
}

/**
 * True when a stdio server block is the model-free thin-proxy form (args
 * contain ``--proxy`` / ``--transport proxy``, or env has
 * ``COGNIS_MCP_PROXY=1``). Pure so unit tests can classify config without
 * spawning.
 */
export function isThinProxyServerBlock(block: unknown): boolean {
  if (typeof block !== "object" || block === null) {
    return false;
  }
  const b = block as { args?: unknown; env?: unknown };
  const args = Array.isArray(b.args) ? b.args.map(String) : [];
  if (args.includes("--proxy")) {
    return true;
  }
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === "--transport" && (args[i + 1] ?? "").toLowerCase() === "proxy") {
      return true;
    }
    if (args[i].toLowerCase() === "--transport=proxy") {
      return true;
    }
  }
  const env =
    typeof b.env === "object" && b.env !== null
      ? (b.env as Record<string, unknown>)
      : {};
  return env[THIN_PROXY_ENV] === "1" || env[THIN_PROXY_ENV] === 1;
}

/**
 * True when a stdio server block is a *heavy* cognis mcpd (command form that
 * is not a thin proxy). Used by process-cardinality accounting so thin proxies
 * do not count toward the heavy-daemon budget (Requirement 2.11).
 */
export function isHeavyStdioServerBlock(block: unknown): boolean {
  if (typeof block !== "object" || block === null) {
    return false;
  }
  if (isHttpServerBlock(block)) {
    return false;
  }
  if (isThinProxyServerBlock(block)) {
    return false;
  }
  const b = block as { command?: unknown };
  return typeof b.command === "string" && b.command.length > 0;
}

/**
 * The stdio mcp.json server block that points an editor at the single
 * ``cognis`` **binary**, dispatched to its ``mcpd`` surface. This is what
 * ``mcp.json`` carries once the binary backend is installed — no Python entry
 * point. ``env`` (COGNIS_DB_PATH etc.) is preserved verbatim. Pure so the
 * written shape is unit-tested.
 */
export function buildBinaryStdioServerBlock(
  binaryPath: string,
  env: Record<string, string>,
  extraArgs: string[] = []
): { command: string; args: string[]; env: Record<string, string> } {
  return { command: binaryPath, args: ["mcpd", ...extraArgs], env };
}

/**
 * A model-free thin-proxy stdio server block: the editor spawns
 * ``<binary> mcpd --proxy``, which forwards JSON-RPC to one heavy repository
 * daemon and loads no ONNX / holds no repository DB (Requirements 2.8, 2.11).
 * Marks the process via ``COGNIS_MCP_PROXY=1`` so the runtime probe can
 * classify it. Optional ``targetUrl`` pins the heavy endpoint; when omitted
 * the proxy acquire-or-spawns the heavy itself.
 */
export function buildBinaryThinProxyServerBlock(
  binaryPath: string,
  env: Record<string, string>,
  targetUrl?: string
): { command: string; args: string[]; env: Record<string, string> } {
  const proxyEnv: Record<string, string> = {
    ...env,
    [THIN_PROXY_ENV]: "1",
  };
  if (targetUrl && targetUrl.trim()) {
    proxyEnv[PROXY_TARGET_ENV] = targetUrl.trim();
  }
  return buildBinaryStdioServerBlock(
    binaryPath,
    proxyEnv,
    buildMcpdProxyFlags(targetUrl)
  );
}

/**
 * Normalize any stdio server block into the binary multi-call form
 * (``command: <binary>``, ``args: ["mcpd", ...]``), preserving any trailing
 * flags and the ``env``. Tolerates a legacy ``-m <module>`` interpreter prefix
 * from an older on-disk config. Pure so the normalization is unit-tested.
 */
export function rewriteServerBlockToBinary(
  block: { command?: string; args?: string[]; env?: Record<string, string> },
  binaryPath: string
): { command: string; args: string[]; env: Record<string, string> } {
  const original = block.args ?? [];
  // Drop a leading ``-m <module>`` interpreter prefix (legacy Python form); keep
  // any extra flags.
  let rest = original[0] === "-m" ? original.slice(2) : original;
  // Drop any leading ``mcpd`` selector token(s) so re-writing an already-binary
  // block is idempotent. `buildBinaryStdioServerBlock` prepends exactly one
  // ``mcpd`` below; without this a repeated rewrite would accumulate
  // ``["mcpd", "mcpd", …]`` (the surface tolerates the extra positional, but the
  // config is wrong and grows on every rewrite).
  while (rest[0] === "mcpd") {
    rest = rest.slice(1);
  }
  // Strip thin-proxy flags/env so a heavy rewrite of a previous proxy block is
  // a clean heavy stdio launch (preservation 3.8 escape hatch).
  const cleaned: string[] = [];
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (a === "--proxy") {
      continue;
    }
    if (a === "--proxy-target") {
      i += 1; // skip value
      continue;
    }
    if (a.startsWith("--proxy-target=")) {
      continue;
    }
    if (a === "--transport" && (rest[i + 1] ?? "").toLowerCase() === "proxy") {
      i += 1;
      continue;
    }
    if (a.toLowerCase() === "--transport=proxy") {
      continue;
    }
    cleaned.push(a);
  }
  const env = { ...(block.env ?? {}) };
  delete env[THIN_PROXY_ENV];
  delete env[PROXY_TARGET_ENV];
  return buildBinaryStdioServerBlock(binaryPath, env, cleaned);
}

/**
 * Normalize a stdio server block into the thin-proxy binary form
 * (``command: <binary>``, ``args: ["mcpd", "--proxy", …]``,
 * ``env.COGNIS_MCP_PROXY=1``). Idempotent under repeated application. Pure so
 * the selection path is unit-testable without spawning.
 */
export function rewriteServerBlockToThinProxy(
  block: { command?: string; args?: string[]; env?: Record<string, string> },
  binaryPath: string,
  targetUrl?: string
): { command: string; args: string[]; env: Record<string, string> } {
  // Preserve non-proxy env; strip any previous proxy-target so a fresh target
  // (or none) wins.
  const baseEnv = { ...(block.env ?? {}) };
  delete baseEnv[THIN_PROXY_ENV];
  delete baseEnv[PROXY_TARGET_ENV];
  // Prefer an explicit target, else one already on the block, else let the
  // proxy acquire-or-spawn.
  const existingTarget =
    typeof block.env?.[PROXY_TARGET_ENV] === "string"
      ? block.env[PROXY_TARGET_ENV]
      : undefined;
  const target = (targetUrl ?? existingTarget)?.trim() || undefined;
  return buildBinaryThinProxyServerBlock(binaryPath, baseEnv, target);
}

export type McpServerPhase = "stopped" | "starting" | "running" | "error";

export interface McpServerState {
  phase: McpServerPhase;
  /** Loopback host the server is bound to (always 127.0.0.1 today). */
  host: string;
  /** Bound port; ``undefined`` while stopped. */
  port?: number;
  /** Server URL (``http://host:port/mcp``); ``undefined`` while stopped. */
  url?: string;
  /** OS pid of the running server, if known. */
  pid?: number;
  /** Last error message, populated when ``phase === "error"``. */
  lastError?: string;
}

export const STOPPED_STATE: McpServerState = Object.freeze({
  phase: "stopped",
  host: LOOPBACK_HOST,
});

// ---------------------------------------------------------------------------
// Per-workspace lifecycle (uses VS Code APIs).
// ---------------------------------------------------------------------------

interface ServerHandle {
  proc: ChildProcessWithoutNullStreams;
  state: McpServerState;
  /**
   * Number of local clients holding this HTTP server open. Reference-aware
   * graceful shutdown (Requirements 2.7): the server is only stopped when the
   * last client releases (or on a forced teardown). A freshly launched handle
   * starts at 1.
   */
  refCount: number;
}

const servers = new Map<string, ServerHandle>();
const stateEmitter = new vscode.EventEmitter<{
  repoRoot: string;
  state: McpServerState;
}>();

/** Fires whenever a workspace's MCP server transitions phase. */
export const onDidChangeMcpServerState = stateEmitter.event;

/** Ports to try before giving up (deterministic base, then small offsets). */
const MAX_BIND_ATTEMPTS = 4;
/**
 * How long to wait for the port to accept connections. Generous because the
 * server warms the DB and (by default) the embedding model on the main thread
 * *before* it binds — a cold model load can take ~15-20s.
 */
const READY_TIMEOUT_MS = 60_000;

function publish(repoRoot: string, state: McpServerState): void {
  stateEmitter.fire({ repoRoot, state });
}

/** Current state for *repoRoot* — never undefined; "stopped" by default. */
export function getMcpServerState(repoRoot: string): McpServerState {
  return servers.get(repoRoot)?.state ?? STOPPED_STATE;
}

/** True when the server for *repoRoot* is starting or running. */
export function isMcpServerRunning(repoRoot: string): boolean {
  const phase = getMcpServerState(repoRoot).phase;
  return phase === "starting" || phase === "running";
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** One TCP connect attempt; resolves true if the port accepts a connection. */
function tcpConnectOnce(host: string, port: number, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.connect({ host, port });
    let settled = false;
    const done = (ok: boolean) => {
      if (settled) {
        return;
      }
      settled = true;
      socket.destroy();
      resolve(ok);
    };
    socket.once("connect", () => done(true));
    socket.once("error", () => done(false));
    socket.setTimeout(timeoutMs, () => done(false));
  });
}

/** Poll until the port binds (server ready) or the deadline passes. */
export async function waitForBind(
  host: string,
  port: number,
  timeoutMs: number
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await tcpConnectOnce(host, port, 1_000)) {
      return true;
    }
    await delay(250);
  }
  return false;
}

/**
 * Launch cognis-mcpd on *port* and resolve once it is either accepting
 * connections (``running``) or has failed (process exited early / never bound).
 * The persistent handle + exit handler outlive this promise so a later crash
 * still flips the panel to "error".
 */
function launchOnPort(
  repoRoot: string,
  port: number,
  channel: vscode.OutputChannel
): Promise<{ running: boolean; error?: string }> {
  return new Promise((resolve) => {
    const { command, args } = resolveMcpdInvocation(
      buildMcpdHttpFlags(LOOPBACK_HOST, port)
    );
    channel.appendLine(`[mcp-http] starting ${command} ${args.join(" ")} (cwd=${repoRoot})`);

    const proc = spawn(command, args, {
      cwd: repoRoot,
      env: { ...process.env, ...modelEnv(), PYTHONUNBUFFERED: "1", PYTHONUTF8: "1" },
    });
    const handle: ServerHandle = {
      proc,
      state: {
        phase: "starting",
        host: LOOPBACK_HOST,
        port,
        url: buildMcpUrl(LOOPBACK_HOST, port),
        pid: proc.pid ?? undefined,
      },
      refCount: 1,
    };
    servers.set(repoRoot, handle);
    // The heavy daemon writes the authoritative `mcpd.lease` on start (Task
    // 6.1); also reconcile from the extension so a reloaded host can reclaim a
    // live orphan safely by owner identity (pid + process-start id + nonce)
    // instead of a bare pid (Requirements 2.7, 2.13).
    reconcileOrphanLease(repoRoot, "mcpd", proc.pid ?? undefined);
    publish(repoRoot, handle.state);

    let settled = false;
    const settle = (running: boolean, error?: string) => {
      if (settled) {
        return;
      }
      settled = true;
      resolve({ running, error });
    };

    proc.stdout.on("data", (chunk: Buffer) => channel.append(`[mcp-http] ${chunk}`));
    proc.stderr.on("data", (chunk: Buffer) => channel.append(`[mcp-http] ${chunk}`));
    proc.on("exit", (code, signal) => {
      if (servers.get(repoRoot) !== handle) {
        return; // a newer spawn already replaced us
      }
      const reason =
        code === 0 || signal === "SIGTERM"
          ? undefined
          : `cognis-mcpd exited with code=${code} signal=${signal}`;
      handle.state = reason
        ? { phase: "error", host: LOOPBACK_HOST, lastError: reason }
        : STOPPED_STATE;
      servers.delete(repoRoot);
      // Clean our lease only when it still records this pid (never clobber a
      // newer owner's record — safe non-destruction, 3.9).
      removeLeaseForPid(repoRoot, "mcpd", proc.pid ?? undefined);
      publish(repoRoot, handle.state);
      // If it exits before we saw a successful bind, this attempt failed.
      settle(false, reason ?? "server exited before binding");
    });

    // Authoritative readiness: the port actually accepts connections.
    void (async () => {
      const ok = await waitForBind(LOOPBACK_HOST, port, READY_TIMEOUT_MS);
      if (settled || servers.get(repoRoot) !== handle) {
        return;
      }
      if (ok) {
        handle.state = { ...handle.state, phase: "running" };
        publish(repoRoot, handle.state);
        settle(true);
      } else {
        channel.appendLine(`[mcp-http] timed out waiting for ${LOOPBACK_HOST}:${port} to bind`);
        try {
          proc.kill();
        } catch {
          /* best effort */
        }
        settle(false, "timed out waiting for the server to bind");
      }
    })();
  });
}

/**
 * Start the HTTP MCP server for *repoRoot*. Idempotent: if one is already
 * running/starting for this workspace, return its current state. Tries the
 * deterministic port first, then a few offsets if the port is already taken,
 * so a busy port degrades to a clear retry instead of a hard failure.
 */
export async function startMcpServer(repoRoot: string): Promise<McpServerState> {
  const existing = servers.get(repoRoot);
  if (existing && (existing.state.phase === "running" || existing.state.phase === "starting")) {
    // Another local client wants this server — reference-aware sharing: bump
    // the hold count so only the last release stops it (Requirements 2.7).
    existing.refCount += 1;
    return existing.state;
  }
  const channel = getOutputChannel();
  let lastError = "could not start cognis-mcpd";
  for (let attempt = 0; attempt < MAX_BIND_ATTEMPTS; attempt += 1) {
    const port = derivePort(repoRoot, attempt);
    const outcome = await launchOnPort(repoRoot, port, channel);
    if (outcome.running) {
      return getMcpServerState(repoRoot);
    }
    lastError = outcome.error ?? lastError;
    const more = attempt + 1 < MAX_BIND_ATTEMPTS;
    channel.appendLine(
      `[mcp-http] port ${port} failed: ${lastError}${more ? " — trying next port" : ""}`
    );
  }
  const errorState: McpServerState = {
    phase: "error",
    host: LOOPBACK_HOST,
    lastError,
  };
  servers.delete(repoRoot);
  publish(repoRoot, errorState);
  return errorState;
}

export interface StopMcpServerOptions {
  /**
   * When true, ignore the reference count and stop immediately (forced
   * teardown: deactivate / remove-from-workspace). Default false implements
   * reference-aware graceful shutdown — only the last client release stops
   * the server (Requirements 2.7).
   */
  force?: boolean;
}

/**
 * Release one local client hold on the HTTP MCP server for *repoRoot*.
 *
 * Reference-aware: the server is only terminated when the last client releases
 * (or when `force: true`). Termination is lease-verified — a pid whose
 * process-start identity no longer matches the recorded `mcpd.lease` is never
 * killed (a PID-reused unrelated process; preservation 3.9). Idempotent:
 * returns the stopped state if nothing is running.
 */
export async function stopMcpServer(
  repoRoot: string,
  options?: StopMcpServerOptions
): Promise<McpServerState> {
  const force = options?.force === true;
  const handle = servers.get(repoRoot);
  if (!handle) {
    return STOPPED_STATE;
  }
  if (!force) {
    handle.refCount = Math.max(0, handle.refCount - 1);
    if (handle.refCount > 0) {
      // Other local clients still hold the server open — leave it running.
      return handle.state;
    }
  }
  const pid = handle.proc.pid;
  // Guard against terminating a PID-reused unrelated process: if the lease
  // records this pid but the live process-start identity differs, refuse
  // (safe non-destruction, 3.9). "match"/"unknown" proceed — killing our own
  // child by its live process handle is always safe.
  const verdict: OwnerVerification = verifyLeaseOwner(repoRoot, "mcpd", pid);
  if (verdict === "mismatch") {
    getOutputChannel().appendLine(
      `[mcp-http] refusing to kill pid=${pid}: lease process-start identity ` +
        `does not match (pid reuse); safe non-destruction (3.9)`
    );
    servers.delete(repoRoot);
    publish(repoRoot, STOPPED_STATE);
    return STOPPED_STATE;
  }
  try {
    handle.proc.kill();
  } catch (err) {
    getOutputChannel().appendLine(
      `[mcp-http] failed to terminate pid=${handle.proc.pid}: ${
        err instanceof Error ? err.message : String(err)
      }`
    );
  }
  // Clean our lease only when the pid still matches (never clobber a newer
  // owner's record — safe non-destruction, 3.9).
  removeLeaseForPid(repoRoot, "mcpd", pid);
  // The exit handler will publish STOPPED. For symmetry return the optimistic
  // state immediately so the caller's UI flips at once.
  return STOPPED_STATE;
}

/** Stop every running MCP server (used on extension deactivation). Forced. */
export async function stopAllMcpServers(): Promise<void> {
  await Promise.all(
    [...servers.keys()].map((repo) => stopMcpServer(repo, { force: true }))
  );
}
