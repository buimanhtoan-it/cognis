// Harness first: installs the vscode stub before model.ts (imports vscode).
import "./testHarness";

import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import {
  initManagedModel,
  installManagedModel,
  isModelInstalled,
  managedModelDir,
  modelAssetName,
  modelEnv,
  uninstallManagedModel,
  MODEL_ID,
  classifyModelFetchFailure,
  type ModelDownloadResponse,
} from "../model";

function fakeContext(storageDir: string, version: string): {
  globalStorageUri: { fsPath: string };
  extension: { packageJSON: { version: string } };
} {
  return {
    globalStorageUri: { fsPath: storageDir },
    extension: { packageJSON: { version } },
  };
}

function silentProgress(): { report: (value: { message?: string }) => void } {
  return { report() {} };
}

function noCancelToken(): {
  isCancellationRequested: boolean;
  onCancellationRequested: () => { dispose: () => void };
} {
  return {
    isCancellationRequested: false,
    onCancellationRequested: () => ({ dispose() {} }),
  };
}

function sha256(buf: Buffer): string {
  return crypto.createHash("sha256").update(buf).digest("hex");
}

test("modelAssetName namespaces each file by the model id", () => {
  assert.equal(modelAssetName("model.onnx"), `${MODEL_ID}-model.onnx`);
  assert.equal(modelAssetName("tokenizer.json"), `${MODEL_ID}-tokenizer.json`);
});

test("modelEnv is empty until the model is installed, then points at the dir", () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-model-env-"));
  initManagedModel(fakeContext(storageDir, "0.8.0") as never, "0.8.0");
  assert.deepEqual(modelEnv(), {}, "no env before install");
  assert.equal(isModelInstalled(), false);

  // Stage the three files so it reads as installed.
  const dir = managedModelDir()!;
  fs.mkdirSync(dir, { recursive: true });
  for (const f of ["model.onnx", "tokenizer.json", "pooling.json"]) {
    fs.writeFileSync(path.join(dir, f), "x");
  }
  assert.equal(isModelInstalled(), true);
  assert.deepEqual(modelEnv(), { COGNIS_ONNX_MODEL_DIR: dir });

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("installManagedModel downloads + checksum-verifies every model file", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-model-ok-"));
  initManagedModel(fakeContext(storageDir, "0.8.0") as never, "0.8.0");

  // Deterministic per-asset bytes + matching sidecars.
  const bodies: Record<string, Buffer> = {
    [`${MODEL_ID}-model.onnx`]: Buffer.from("ONNX-WEIGHTS"),
    [`${MODEL_ID}-tokenizer.json`]: Buffer.from("{tok}"),
    [`${MODEL_ID}-pooling.json`]: Buffer.from("{cls}"),
  };
  const download = async (url: string): Promise<ModelDownloadResponse> => {
    const name = url.split("/").pop()!;
    if (name.endsWith(".sha256")) {
      const asset = name.slice(0, -".sha256".length);
      return { ok: true, status: 200, body: Buffer.from(`${sha256(bodies[asset])}  ${asset}`) };
    }
    return { ok: true, status: 200, body: bodies[name] };
  };

  const outcome = await installManagedModel(
    silentProgress() as never,
    noCancelToken() as never,
    { download }
  );
  assert.equal(outcome.files.length, 3);
  assert.equal(isModelInstalled(), true);
  // The three files landed with the engine-expected local names.
  const dir = managedModelDir()!;
  for (const f of ["model.onnx", "tokenizer.json", "pooling.json"]) {
    assert.ok(fs.existsSync(path.join(dir, f)), `missing ${f}`);
  }

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("installManagedModel refuses a file whose checksum does not match", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-model-bad-"));
  initManagedModel(fakeContext(storageDir, "0.8.0") as never, "0.8.0");

  const download = async (url: string): Promise<ModelDownloadResponse> => {
    const name = url.split("/").pop()!;
    if (name.endsWith(".sha256")) {
      // A wrong digest for every asset.
      return { ok: true, status: 200, body: Buffer.from(`${"0".repeat(64)}  x`) };
    }
    return { ok: true, status: 200, body: Buffer.from("data") };
  };

  await assert.rejects(
    installManagedModel(silentProgress() as never, noCancelToken() as never, { download }),
    /checksum verification/
  );
  assert.equal(isModelInstalled(), false, "no file staged on verification failure");

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("uninstallManagedModel removes a staged model", () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-model-rm-"));
  initManagedModel(fakeContext(storageDir, "0.8.0") as never, "0.8.0");
  const dir = managedModelDir()!;
  fs.mkdirSync(dir, { recursive: true });
  for (const f of ["model.onnx", "tokenizer.json", "pooling.json"]) {
    fs.writeFileSync(path.join(dir, f), "x");
  }
  assert.equal(isModelInstalled(), true);
  assert.equal(uninstallManagedModel(), true);
  assert.equal(isModelInstalled(), false);
  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("classifyModelFetchFailure gives specific guidance per status", () => {
  assert.match(classifyModelFetchFailure(0, "a", "0.8.0").userMessage, /internet/i);
  assert.match(classifyModelFetchFailure(404, "a", "0.8.0").userMessage, /not found|stay off/i);
  assert.match(classifyModelFetchFailure(500, "a", "0.8.0").userMessage, /HTTP 500/);
});
