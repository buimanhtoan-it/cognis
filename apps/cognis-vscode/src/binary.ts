import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import { getOutputChannel } from "./cli";

/**
 * Managed single-binary engine lifecycle (Rust engine, G2 / Requirement 1.1).
 *
 * The shipped engine is **one static `cognis` binary per platform** (see
 * `docs/distribution.md`). The extension downloads the prebuilt binary for the
 * user's platform from the GitHub Release, **verifies its SHA-256 checksum**
 * against the published `.sha256` sidecar, and stores it under the extension's
 * own global storage — so a user gets a working engine with **no Python, no
 * pip, no compiler**.
 *
 * The binary is busybox-style multi-call: a leading subcommand (`cli`, `mcpd`,
 * `indexd`) selects the surface, so a single artifact drives the CLI, the MCP
 * daemon, and the indexer. `mcp.json` therefore points at `<binary> mcpd`.
 *
 * The pure helpers at the top (triple/asset/url/checksum) have no VS Code or
 * network dependency, so the fetch+verify+platform-detection logic is fully
 * unit-testable offline — exactly where the regression bar sits when real
 * release assets cannot be downloaded in this environment.
 */

/** Human-friendly elapsed time: "8s", "1m20s", "12m03s". */
export function formatElapsed(ms: number): string {
  const totalSec = Math.max(0, Math.round(ms / 1000));
  if (totalSec < 60) {
    return `${totalSec}s`;
  }
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}m${s.toString().padStart(2, "0")}s`;
}

/** GitHub repo that publishes the release binaries (owner/name). */
const DEFAULT_BINARY_REPO = "buimanhtoan-it/cognis";

/**
 * The platforms the release matrix ships a self-contained binary for
 * (release.yml `dist-binaries`). Limited to the targets ort publishes a
 * prebuilt ONNX Runtime for, since `--features onnx-download` statically links
 * it: Windows x64, Linux x64, macOS arm64. Intel macOS (x86_64-apple-darwin)
 * and arm64 Linux have no ort prebuilt and are intentionally absent — a user on
 * one gets a clear "no binary for your platform" message rather than a 404.
 */
const TRIPLE_BY_PLATFORM: Record<string, Record<string, string>> = {
  win32: { x64: "x86_64-pc-windows-msvc" },
  darwin: { arm64: "aarch64-apple-darwin" },
  linux: { x64: "x86_64-unknown-linux-gnu" },
};

// ---------------------------------------------------------------------------
// Pure helpers (no VS Code, no network) — unit-testable in plain Node.
// ---------------------------------------------------------------------------

/**
 * Map a Node ``process.platform`` / ``process.arch`` pair to the Rust target
 * triple of the matching release binary, or ``undefined`` when no binary is
 * published for that platform (so callers can fall back / message clearly).
 */
export function detectTargetTriple(
  platform: string = process.platform,
  arch: string = process.arch
): string | undefined {
  return TRIPLE_BY_PLATFORM[platform]?.[arch];
}

/** The release asset filename for a target triple (``.exe`` on Windows). */
export function binaryAssetName(triple: string): string {
  const exe = triple.includes("windows") ? ".exe" : "";
  return `cognis-${triple}${exe}`;
}

/** The checksum sidecar filename for a target triple (``<asset>.sha256``). */
export function checksumAssetName(triple: string): string {
  return `${binaryAssetName(triple)}.sha256`;
}

/** The release tag for a version (``v<version>``), matching release.yml. */
export function releaseTag(version: string): string {
  return version.startsWith("v") ? version : `v${version}`;
}

/**
 * Base URL of the release assets directory. A non-empty ``override`` wins
 * verbatim (offline mirrors / pre-release testing) with any trailing slash
 * trimmed; otherwise it is the GitHub Release download path for ``repo`` + tag.
 */
export function releaseAssetBaseUrl(
  repo: string,
  version: string,
  override?: string
): string {
  const trimmed = override?.trim();
  if (trimmed) {
    return trimmed.replace(/\/+$/, "");
  }
  return `https://github.com/${repo}/releases/download/${releaseTag(version)}`;
}

/** Full download URL of a named asset under a base URL. */
export function assetDownloadUrl(baseUrl: string, assetName: string): string {
  return `${baseUrl.replace(/\/+$/, "")}/${assetName}`;
}

/**
 * Extract the expected SHA-256 hex digest from a ``.sha256`` sidecar.
 *
 * Sidecars are written in ``sha256sum -c`` format (``<hex>  <name>`` or
 * ``<hex> *<name>``) by ``cargo xtask dist``; a bare hex line is also accepted.
 * Returns the lowercased 64-char digest, or ``undefined`` when none is present.
 */
export function parseSha256Sidecar(text: string): string | undefined {
  const match = text.match(/\b([0-9a-fA-F]{64})\b/);
  return match ? match[1].toLowerCase() : undefined;
}

/** Lowercase hex SHA-256 of a buffer. */
export function sha256Hex(data: Buffer | Uint8Array): string {
  return crypto.createHash("sha256").update(data).digest("hex");
}

export interface ChecksumResult {
  ok: boolean;
  expected?: string;
  actual: string;
}

/**
 * Verify a downloaded binary against its sidecar. Pure so the
 * trust-on-download decision is unit-tested. ``ok`` is false when the sidecar
 * has no parseable digest (we never install an unverifiable binary).
 */
export function verifyDownloadedBinary(
  binary: Buffer | Uint8Array,
  sidecarText: string
): ChecksumResult {
  const expected = parseSha256Sidecar(sidecarText);
  const actual = sha256Hex(binary);
  return { ok: Boolean(expected) && actual === expected, expected, actual };
}

export type BackendSurface = "cli" | "mcpd" | "indexd";

/** The multi-call subcommand that selects a backend surface on the binary. */
export function binarySubcommand(surface: BackendSurface): string {
  return surface;
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

export class BinaryInstallError extends Error {
  readonly userMessage: string;
  constructor(userMessage: string) {
    super(userMessage);
    this.name = "BinaryInstallError";
    this.userMessage = userMessage;
  }
}

/**
 * Turn a failed binary fetch into a specific, actionable message. ``status`` is
 * the HTTP status (0 = network failure); ``triple`` the resolved platform.
 */
export function classifyBinaryFetchFailure(
  status: number,
  triple: string,
  version: string
): BinaryInstallError {
  if (status === 0) {
    return new BinaryInstallError(
      "Couldn't reach the Cognis release server to download the engine binary. " +
        "Check your internet connection (or proxy/VPN) and click Install backend again. " +
        "Behind a corporate proxy? Set HTTPS_PROXY and reload the window."
    );
  }
  if (status === 404) {
    return new BinaryInstallError(
      `No Cognis engine binary for your platform (${triple}) was found in release ${releaseTag(version)}. ` +
        "If you just published this release the upload may still be in progress — wait a minute and click Install backend again."
    );
  }
  return new BinaryInstallError(
    `Downloading the Cognis engine binary failed (HTTP ${status}). ` +
      "Open the Cognis output log for details, then try Install backend again."
  );
}

// ---------------------------------------------------------------------------
// Managed binary location + lifecycle (uses VS Code APIs).
// ---------------------------------------------------------------------------

let managedRootDir: string | undefined;
let expectedBinaryVersion: string | undefined;
let binaryRepo: string = DEFAULT_BINARY_REPO;

/** Folder that holds the managed binary (``<globalStorage>/bin``). */
export function managedBinDir(): string | undefined {
  return managedRootDir ? path.join(managedRootDir, "bin") : undefined;
}

/** Path to the managed binary on disk (``.exe`` on Windows), or undefined. */
export function managedBinaryPath(): string | undefined {
  // Test / bring-your-own override: an explicit prebuilt binary path (env or
  // setting) wins, so a harness can drive the real engine binary without the
  // download/install dance, and power users can point at their own build.
  const override = binaryPathOverride();
  if (override) {
    return override;
  }
  const dir = managedBinDir();
  if (!dir) {
    return undefined;
  }
  const exe = process.platform === "win32" ? ".exe" : "";
  return path.join(dir, `cognis${exe}`);
}

/**
 * An explicit prebuilt-binary path override, from ``COGNIS_BINARY_PATH`` (env)
 * or the ``cognis.binaryPath`` setting. Empty when unset. Used by the
 * full-stack host e2e to drive the freshly-built Rust binary, and as a
 * bring-your-own-binary escape hatch.
 */
function binaryPathOverride(): string {
  const env = (process.env.COGNIS_BINARY_PATH ?? "").trim();
  if (env) {
    return env;
  }
  return vscode.workspace
    .getConfiguration("cognis")
    .get<string>("binaryPath", "")
    .trim();
}

/** Sidecar that records the version of the installed binary. */
function versionMarkerPath(): string | undefined {
  const dir = managedBinDir();
  return dir ? path.join(dir, "cognis.binary.version") : undefined;
}

/** True when the managed binary actually exists on disk. */
export function isManagedBinaryInstalled(): boolean {
  const exe = managedBinaryPath();
  return Boolean(exe && fs.existsSync(exe));
}

/** The version of the installed managed binary, if recorded. */
export function installedBinaryVersion(): string | undefined {
  const marker = versionMarkerPath();
  if (!marker || !fs.existsSync(marker)) {
    return undefined;
  }
  try {
    const v = fs.readFileSync(marker, "utf8").trim();
    return v || undefined;
  } catch {
    return undefined;
  }
}

/**
 * True when the managed binary is the active backend. The engine is the single
 * self-contained `cognis` binary, so this is simply "is it installed?".
 */
export function isManagedBinaryActive(): boolean {
  return isManagedBinaryInstalled();
}

/**
 * Wire up the managed binary at activation: remember the storage root + the
 * extension version (the target binary version), the configured release repo,
 * and register the binary path so every backend invocation can prefer it.
 */
export function initManagedBinary(
  context: vscode.ExtensionContext,
  extensionVersion?: string
): void {
  managedRootDir = context.globalStorageUri.fsPath;
  expectedBinaryVersion =
    extensionVersion ??
    (context.extension?.packageJSON?.version as string | undefined);
  const repo = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("binaryRepo", "")
    .trim();
  binaryRepo = repo || DEFAULT_BINARY_REPO;
}

export interface BinaryDriftCheck {
  installed?: string;
  expected?: string;
  outdated: boolean;
}

/**
 * Detect whether the installed binary lags behind the extension after an
 * update. Returns ``outdated: false`` when there is nothing to manage (nothing
 * installed or unknown target).
 */
export function checkManagedBinaryDrift(): BinaryDriftCheck {
  const expected = expectedBinaryVersion;
  if (!expected || !isManagedBinaryInstalled()) {
    return { expected, outdated: false };
  }
  const installed = installedBinaryVersion();
  if (!installed) {
    return { installed, expected, outdated: false };
  }
  return { installed, expected, outdated: compareSemver(installed, expected) < 0 };
}

/** Compare dotted versions; missing parts are 0. Returns -1, 0, or 1. */
function compareSemver(a: string, b: string): number {
  const pa = a.replace(/^v/, "").split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.replace(/^v/, "").split(".").map((n) => parseInt(n, 10) || 0);
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i += 1) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) {
      return diff < 0 ? -1 : 1;
    }
  }
  return 0;
}

export interface DownloadResponse {
  ok: boolean;
  status: number;
  body: Buffer;
}

export interface BinaryFetchDeps {
  /** Download a URL to a buffer. Injected in tests so no network is needed. */
  download: (url: string) => Promise<DownloadResponse>;
}

/** Default downloader: global fetch (follows GitHub's redirect to storage). */
async function defaultDownload(url: string): Promise<DownloadResponse> {
  try {
    const res = await fetch(url, { redirect: "follow" });
    const body = Buffer.from(await res.arrayBuffer());
    return { ok: res.ok, status: res.status, body };
  } catch {
    return { ok: false, status: 0, body: Buffer.alloc(0) };
  }
}

export interface BinaryInstallOutcome {
  triple: string;
  path: string;
  version?: string;
  bytes: number;
  timings: Array<{ phase: string; ms: number }>;
}

/**
 * Install the Cognis engine **binary** with no manual steps: detect the
 * platform, download the matching release artifact + its checksum, verify the
 * SHA-256, and stage it under the extension's global storage. No Python, pip,
 * or compiler involved (Requirement 1.1).
 *
 * Fails loudly and safely: an unsupported platform, an unreachable/absent
 * release, or a checksum mismatch all throw a ``BinaryInstallError`` with a
 * specific message and the binary is never written when verification fails.
 */
export async function installManagedBinary(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken,
  deps?: Partial<BinaryFetchDeps>
): Promise<BinaryInstallOutcome> {
  const download = deps?.download ?? defaultDownload;
  const channel = getOutputChannel();
  const timings: Array<{ phase: string; ms: number }> = [];
  const phase = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const started = Date.now();
    try {
      return await fn();
    } finally {
      const ms = Date.now() - started;
      timings.push({ phase: name, ms });
      channel.appendLine(`[binary] ${name} took ${ms}ms`);
    }
  };

  const version = expectedBinaryVersion;
  if (!version) {
    throw new BinaryInstallError(
      "Cognis could not determine which engine version to install. Reload the window and try again."
    );
  }
  const triple = detectTargetTriple();
  if (!triple) {
    throw new BinaryInstallError(
      `Cognis does not publish an engine binary for your platform (${process.platform}/${process.arch}). ` +
        "Please open an issue so we can add a build for it."
    );
  }
  const binDir = managedBinDir();
  const destPath = managedBinaryPath();
  if (!binDir || !destPath) {
    throw new BinaryInstallError(
      "Cognis storage is not ready yet. Reload the window and try again."
    );
  }

  const baseUrl = releaseAssetBaseUrl(
    binaryRepo,
    version,
    vscode.workspace.getConfiguration("cognis").get<string>("binaryDownloadBaseUrl", "")
  );
  const binaryUrl = assetDownloadUrl(baseUrl, binaryAssetName(triple));
  const checksumUrl = assetDownloadUrl(baseUrl, checksumAssetName(triple));

  if (token.isCancellationRequested) {
    throw new BinaryInstallError("Install cancelled.");
  }

  progress.report({ message: `Downloading the Cognis engine for ${triple}…` });
  const binResp = await phase("download binary", () => download(binaryUrl));
  if (!binResp.ok) {
    throw classifyBinaryFetchFailure(binResp.status, triple, version);
  }

  if (token.isCancellationRequested) {
    throw new BinaryInstallError("Install cancelled.");
  }

  progress.report({ message: "Verifying the download…" });
  const sumResp = await phase("download checksum", () => download(checksumUrl));
  if (!sumResp.ok) {
    throw classifyBinaryFetchFailure(sumResp.status, triple, version);
  }

  const check = verifyDownloadedBinary(binResp.body, sumResp.body.toString("utf8"));
  if (!check.ok) {
    channel.appendLine(
      `[binary] checksum mismatch: expected=${check.expected ?? "<none>"} actual=${check.actual}`
    );
    throw new BinaryInstallError(
      "The downloaded Cognis engine binary failed its checksum verification and was not installed. " +
        "This can mean a corrupted or tampered download — try Install backend again, and if it persists, report it."
    );
  }

  await phase("write binary", async () => {
    fs.mkdirSync(binDir, { recursive: true });
    const tmp = `${destPath}.download`;
    fs.writeFileSync(tmp, binResp.body);
    if (process.platform !== "win32") {
      fs.chmodSync(tmp, 0o755);
    }
    fs.renameSync(tmp, destPath);
    const marker = versionMarkerPath();
    if (marker) {
      fs.writeFileSync(marker, version, "utf8");
    }
  });

  channel.appendLine(`[binary] installed ${triple} v${version} (${binResp.body.length} bytes) → ${destPath}`);
  return {
    triple,
    path: destPath,
    version,
    bytes: binResp.body.length,
    timings,
  };
}

export interface BinaryUninstallOutcome {
  removed: boolean;
  detail: string;
}

/** Delete the managed binary the extension installed (safe: our folder only). */
export async function uninstallManagedBinary(): Promise<BinaryUninstallOutcome> {
  const dir = managedBinDir();
  if (!dir || !fs.existsSync(dir)) {
    return { removed: false, detail: "No Cognis engine binary was installed by the extension." };
  }
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch (err) {
    getOutputChannel().appendLine(
      `[binary] delete failed: ${err instanceof Error ? err.message : String(err)}`
    );
    throw err;
  }
  return { removed: true, detail: `Deleted the managed Cognis engine binary at ${dir}.` };
}

// ---------------------------------------------------------------------------
// Engine invocation resolution (always the managed multi-call binary).
// ---------------------------------------------------------------------------

export interface BackendInvocation {
  command: string;
  args: string[];
}

/**
 * How to invoke the ``cli`` surface for ``repoRoot`` with ``args``:
 * ``<binary> cli --repo-root <root> <args>``. The command is always the managed
 * binary path (honoring the ``COGNIS_BINARY_PATH`` / ``cognis.binaryPath``
 * override).
 */
export function resolveCliInvocation(
  repoRoot: string,
  args: string[]
): BackendInvocation {
  return {
    command: managedBinaryPath()!,
    args: ["cli", "--repo-root", repoRoot, ...args],
  };
}

/** How to invoke the ``mcpd`` surface with ``flags``: ``<binary> mcpd <flags>``. */
export function resolveMcpdInvocation(flags: string[] = []): BackendInvocation {
  return { command: managedBinaryPath()!, args: ["mcpd", ...flags] };
}

/** How to invoke the ``indexd`` surface with ``flags``: ``<binary> indexd <flags>``. */
export function resolveIndexdInvocation(flags: string[] = []): BackendInvocation {
  return { command: managedBinaryPath()!, args: ["indexd", ...flags] };
}
