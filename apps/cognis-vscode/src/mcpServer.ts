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
  return buildBinaryStdioServerBlock(binaryPath, block.env ?? {}, rest);
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
    };
    servers.set(repoRoot, handle);
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

/**
 * Stop the HTTP MCP server for *repoRoot*. Idempotent: returns the stopped
 * state if nothing is running.
 */
export async function stopMcpServer(repoRoot: string): Promise<McpServerState> {
  const handle = servers.get(repoRoot);
  if (!handle) {
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
  // The exit handler will publish STOPPED. For symmetry return the optimistic
  // state immediately so the caller's UI flips at once.
  return STOPPED_STATE;
}

/** Stop every running MCP server (used on extension deactivation). */
export async function stopAllMcpServers(): Promise<void> {
  await Promise.all([...servers.keys()].map((repo) => stopMcpServer(repo)));
}
