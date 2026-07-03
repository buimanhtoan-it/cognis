import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import { getOutputChannel } from "./cli";
import {
  assetDownloadUrl,
  parseSha256Sidecar,
  releaseAssetBaseUrl,
  sha256Hex,
} from "./binary";

/**
 * Managed ONNX embedding-model lifecycle (semantic search, Option B).
 *
 * Semantic search needs two things at runtime: the ONNX Runtime (statically
 * linked into the release binary) and the **model weights** — bge-small-en-v1.5
 * as `model.onnx` + `tokenizer.json` + `pooling.json` (~100 MB). The weights are
 * data, not code, so we do not bundle them into the binary. Instead — mirroring
 * how the engine binary itself is fetched — the extension downloads the model
 * assets from the GitHub Release on first use, **verifies each file's SHA-256**
 * against its published `.sha256` sidecar, and stores them under the extension's
 * global storage. The engine is then pointed at that directory via
 * `COGNIS_ONNX_MODEL_DIR` (see `modelEnv`), so `cognis-mcpd` / `cognis-indexd`
 * produce real embeddings. Until the model is present the semantic leg degrades
 * to empty (lexical + structural + diffusion still work).
 *
 * The pure helpers (manifest / dir / env) have no network dependency, so the
 * layout + env wiring is unit-testable offline.
 */

/** The model id whose leaf names the on-disk directory (engine convention). */
export const MODEL_ID = "bge-small-en-v1.5";

/** Files the `onnx-local` backend loads from the model directory. */
const MODEL_FILES = ["model.onnx", "tokenizer.json", "pooling.json"] as const;

/** The release asset name for a model file (namespaced by model id). */
export function modelAssetName(localFile: string): string {
  return `${MODEL_ID}-${localFile}`;
}

let managedRootDir: string | undefined;
let expectedVersion: string | undefined;
let modelRepo = "buimanhtoan-it/cognis";

/**
 * Wire up the managed model at activation: remember the storage root, the
 * target version (the extension's own — the release the assets come from), and
 * the configured release repo.
 */
export function initManagedModel(
  context: vscode.ExtensionContext,
  extensionVersion?: string
): void {
  managedRootDir = context.globalStorageUri.fsPath;
  expectedVersion =
    extensionVersion ??
    (context.extension?.packageJSON?.version as string | undefined);
  const repo = vscode.workspace
    .getConfiguration("cognis")
    .get<string>("binaryRepo", "")
    .trim();
  modelRepo = repo || "buimanhtoan-it/cognis";
}

/** The directory the model files live in (`<globalStorage>/models/<id>`). */
export function managedModelDir(): string | undefined {
  return managedRootDir
    ? path.join(managedRootDir, "models", MODEL_ID)
    : undefined;
}

/** True when every model file is present on disk. */
export function isModelInstalled(): boolean {
  const dir = managedModelDir();
  if (!dir) {
    return false;
  }
  return MODEL_FILES.every((f) => fs.existsSync(path.join(dir, f)));
}

/**
 * Env that points the engine at the managed model, or `{}` when it is not
 * installed (so callers can spread it unconditionally — semantic simply stays
 * off until the model is present).
 */
export function modelEnv(): Record<string, string> {
  const dir = managedModelDir();
  if (dir && isModelInstalled()) {
    return { COGNIS_ONNX_MODEL_DIR: dir };
  }
  return {};
}

export interface ModelDownloadResponse {
  ok: boolean;
  status: number;
  body: Buffer;
}

export interface ModelFetchDeps {
  download: (url: string) => Promise<ModelDownloadResponse>;
}

async function defaultDownload(url: string): Promise<ModelDownloadResponse> {
  try {
    const res = await fetch(url, { redirect: "follow" });
    const body = Buffer.from(await res.arrayBuffer());
    return { ok: res.ok, status: res.status, body };
  } catch {
    return { ok: false, status: 0, body: Buffer.alloc(0) };
  }
}

export class ModelInstallError extends Error {
  readonly userMessage: string;
  constructor(userMessage: string) {
    super(userMessage);
    this.name = "ModelInstallError";
    this.userMessage = userMessage;
  }
}

export interface ModelInstallOutcome {
  dir: string;
  files: string[];
  bytes: number;
}

/**
 * Download + verify the model files and stage them under the managed model
 * directory. Each file is checksum-verified against its `.sha256` sidecar and
 * written atomically (temp + rename); a verification failure aborts without
 * leaving a partial file. Idempotent: already-present, still-verifiable files
 * are re-fetched (cheap correctness over cleverness — this runs once).
 */
export async function installManagedModel(
  progress: vscode.Progress<{ message?: string }>,
  token: vscode.CancellationToken,
  deps?: Partial<ModelFetchDeps>
): Promise<ModelInstallOutcome> {
  const download = deps?.download ?? defaultDownload;
  const channel = getOutputChannel();
  const version = expectedVersion;
  if (!version) {
    throw new ModelInstallError(
      "Cognis could not determine which model version to download. Reload the window and try again."
    );
  }
  const dir = managedModelDir();
  if (!dir) {
    throw new ModelInstallError(
      "Cognis storage is not ready yet. Reload the window and try again."
    );
  }

  const baseUrl = releaseAssetBaseUrl(
    modelRepo,
    version,
    vscode.workspace
      .getConfiguration("cognis")
      .get<string>("modelDownloadBaseUrl", "")
  );

  fs.mkdirSync(dir, { recursive: true });
  let totalBytes = 0;

  for (const localFile of MODEL_FILES) {
    if (token.isCancellationRequested) {
      throw new ModelInstallError("Install cancelled.");
    }
    const asset = modelAssetName(localFile);
    progress.report({ message: `Downloading semantic model — ${localFile}…` });

    const fileResp = await download(assetDownloadUrl(baseUrl, asset));
    if (!fileResp.ok) {
      throw classifyModelFetchFailure(fileResp.status, asset, version);
    }
    const sumResp = await download(assetDownloadUrl(baseUrl, `${asset}.sha256`));
    if (!sumResp.ok) {
      throw classifyModelFetchFailure(sumResp.status, `${asset}.sha256`, version);
    }
    const expected = parseSha256Sidecar(sumResp.body.toString("utf8"));
    const actual = sha256Hex(fileResp.body);
    if (!expected || actual !== expected) {
      channel.appendLine(
        `[model] checksum mismatch for ${asset}: expected=${expected ?? "<none>"} actual=${actual}`
      );
      throw new ModelInstallError(
        `The downloaded semantic-model file ${localFile} failed its checksum verification and was not installed. ` +
          "Try again; if it persists, report it."
      );
    }

    const dest = path.join(dir, localFile);
    const tmp = `${dest}.download`;
    fs.writeFileSync(tmp, fileResp.body);
    fs.renameSync(tmp, dest);
    totalBytes += fileResp.body.length;
    channel.appendLine(`[model] installed ${localFile} (${fileResp.body.length} bytes)`);
  }

  channel.appendLine(`[model] semantic model ready at ${dir} (${totalBytes} bytes)`);
  return { dir, files: [...MODEL_FILES], bytes: totalBytes };
}

/** Turn a failed model fetch into a specific, actionable message. */
export function classifyModelFetchFailure(
  status: number,
  asset: string,
  version: string
): ModelInstallError {
  if (status === 0) {
    return new ModelInstallError(
      "Couldn't reach the Cognis release server to download the semantic model. " +
        "Check your internet connection (or proxy/VPN) and try again. Semantic search stays off until the model is installed."
    );
  }
  if (status === 404) {
    return new ModelInstallError(
      `The semantic model asset ${asset} was not found in release v${version}. ` +
        "This build may not publish the model yet — semantic search will stay off (lexical + structural search still work)."
    );
  }
  return new ModelInstallError(
    `Downloading the semantic model failed (HTTP ${status}). Open the Cognis output log for details and try again.`
  );
}

/** Remove the managed model directory (safe: our folder only). */
export function uninstallManagedModel(): boolean {
  const dir = managedModelDir();
  if (!dir || !fs.existsSync(dir)) {
    return false;
  }
  try {
    fs.rmSync(dir, { recursive: true, force: true });
    return true;
  } catch (err) {
    getOutputChannel().appendLine(
      `[model] delete failed: ${err instanceof Error ? err.message : String(err)}`
    );
    return false;
  }
}
