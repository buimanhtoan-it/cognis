/**
 * Property 33: Downloads are used only after checksum verification (task 16.13).
 *
 * For any Engine_Binary or Model_Assets payload and any published checksum,
 * production install uses the payload only when its SHA-256 equals the sidecar
 * digest. A mismatched or unparseable sidecar is never executed, loaded, or
 * staged.
 *
 * Validates: Requirements 21.4, 3.1, 3.2
 * Properties: P33
 */
import "./testHarness";

import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { downloadedPayloadIsUsable } from "../activation";
import {
  detectTargetTriple,
  initManagedBinary,
  installManagedBinary,
  isManagedBinaryInstalled,
  managedBinaryPath,
  parseSha256Sidecar,
  sha256Hex,
  verifyDownloadedBinary,
  type DownloadResponse,
} from "../binary";
import {
  initManagedModel,
  installManagedModel,
  isModelInstalled,
  managedModelDir,
  modelAssetName,
  type ModelDownloadResponse,
} from "../model";
import { PROPERTY_RUNS } from "./generators";

const MODEL_FILES = ["model.onnx", "tokenizer.json", "pooling.json"] as const;

type SidecarKind = "match" | "mismatch" | "garbage";
type SidecarStyle = "spaced" | "star" | "bare" | "upper";

interface ChecksumCase {
  payload: Uint8Array;
  kind: SidecarKind;
  style: SidecarStyle;
  garbage: string;
}

const arbPayload = fc.uint8Array({ minLength: 0, maxLength: 48 });

const arbChecksumCase: fc.Arbitrary<ChecksumCase> = fc.record({
  payload: arbPayload,
  kind: fc.oneof(
    { weight: 2, arbitrary: fc.constant("match" as const) },
    { weight: 2, arbitrary: fc.constant("mismatch" as const) },
    { weight: 1, arbitrary: fc.constant("garbage" as const) }
  ),
  style: fc.constantFrom<SidecarStyle>("spaced", "star", "bare", "upper"),
  garbage: fc.constantFrom("", "not a checksum", "deadbeef", "????"),
});

function independentDigest(payload: Uint8Array): string {
  return crypto.createHash("sha256").update(payload).digest("hex");
}

function mismatchDigest(actual: string): string {
  return `${actual[0] === "0" ? "1" : "0"}${actual.slice(1)}`;
}

function formatSidecar(hex: string, style: SidecarStyle): string {
  switch (style) {
    case "spaced":
      return `${hex}  cognis-bin`;
    case "star":
      return `${hex} *cognis.exe`;
    case "bare":
      return `${hex}\n`;
    case "upper":
      return hex.toUpperCase();
  }
}

function publishedSidecar(case_: ChecksumCase): string {
  const actual = independentDigest(case_.payload);
  switch (case_.kind) {
    case "match":
      return formatSidecar(actual, case_.style);
    case "mismatch":
      return formatSidecar(mismatchDigest(actual), case_.style);
    case "garbage":
      return case_.garbage;
  }
}

function expectedUsable(payload: Uint8Array, sidecar: string): boolean {
  const parsed = parseSha256Sidecar(sidecar);
  return parsed !== undefined && parsed === independentDigest(payload);
}

function fakeContext(storageDir: string, version: string) {
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

test("Property 33: a payload is used only when its digest matches the published checksum", async () => {
  assert.ok(detectTargetTriple(), "host must be a published engine triple");

  await fc.assert(
    fc.asyncProperty(
      arbChecksumCase,
      fc.tuple(arbChecksumCase, arbChecksumCase, arbChecksumCase),
      async (binaryCase, modelCases) => {
        const binarySidecar = publishedSidecar(binaryCase);
        const usable = expectedUsable(binaryCase.payload, binarySidecar);
        const payloadBuf = Buffer.from(binaryCase.payload);

        assert.equal(downloadedPayloadIsUsable(payloadBuf, binarySidecar), usable);
        assert.equal(verifyDownloadedBinary(payloadBuf, binarySidecar).ok, usable);
        assert.equal(verifyDownloadedBinary(payloadBuf, binarySidecar).actual, sha256Hex(payloadBuf));
        assert.equal(sha256Hex(payloadBuf), independentDigest(binaryCase.payload));

        const binDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p33-bin-"));
        try {
          initManagedBinary(fakeContext(binDir, "9.9.9") as never, "9.9.9");
          const download = async (url: string): Promise<DownloadResponse> => {
            if (url.endsWith(".sha256")) {
              return { ok: true, status: 200, body: Buffer.from(binarySidecar) };
            }
            return { ok: true, status: 200, body: payloadBuf };
          };
          if (usable) {
            const outcome = await installManagedBinary(
              silentProgress() as never,
              noCancelToken() as never,
              { download }
            );
            assert.equal(isManagedBinaryInstalled(), true);
            const dest = managedBinaryPath();
            assert.ok(dest);
            assert.deepEqual(fs.readFileSync(dest), payloadBuf);
            assert.equal(outcome.bytes, payloadBuf.length);
          } else {
            await assert.rejects(
              () =>
                installManagedBinary(
                  silentProgress() as never,
                  noCancelToken() as never,
                  { download }
                ),
              /checksum verification/i
            );
            assert.equal(isManagedBinaryInstalled(), false);
            const dest = managedBinaryPath();
            if (dest) {
              assert.equal(fs.existsSync(dest), false);
            }
          }
        } finally {
          fs.rmSync(binDir, { recursive: true, force: true });
        }

        const modelDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-p33-model-"));
        try {
          initManagedModel(fakeContext(modelDir, "9.9.9") as never, "9.9.9");
          const sidecars = modelCases.map((case_) => publishedSidecar(case_));
          const usableFlags = modelCases.map((case_, index) =>
            expectedUsable(case_.payload, sidecars[index])
          );
          const download = async (url: string): Promise<ModelDownloadResponse> => {
            const name = url.split("/").pop() ?? "";
            for (const [index, local] of MODEL_FILES.entries()) {
              const asset = modelAssetName(local);
              if (name === `${asset}.sha256`) {
                return { ok: true, status: 200, body: Buffer.from(sidecars[index]) };
              }
              if (name === asset) {
                return {
                  ok: true,
                  status: 200,
                  body: Buffer.from(modelCases[index].payload),
                };
              }
            }
            return { ok: false, status: 404, body: Buffer.alloc(0) };
          };

          const allUsable = usableFlags.every(Boolean);
          if (allUsable) {
            await installManagedModel(
              silentProgress() as never,
              noCancelToken() as never,
              { download }
            );
            assert.equal(isModelInstalled(), true);
            const staged = managedModelDir();
            assert.ok(staged);
            for (const [index, local] of MODEL_FILES.entries()) {
              assert.deepEqual(
                fs.readFileSync(path.join(staged, local)),
                Buffer.from(modelCases[index].payload)
              );
            }
          } else {
            await assert.rejects(
              () =>
                installManagedModel(
                  silentProgress() as never,
                  noCancelToken() as never,
                  { download }
                ),
              /checksum verification/i
            );
            assert.equal(isModelInstalled(), false);
            const staged = managedModelDir();
            const firstMismatch = usableFlags.indexOf(false);
            if (staged && firstMismatch >= 0) {
              assert.equal(
                fs.existsSync(path.join(staged, MODEL_FILES[firstMismatch])),
                false,
                "a mismatched model payload must not be installed"
              );
            }
          }
        } finally {
          fs.rmSync(modelDir, { recursive: true, force: true });
        }
      }
    ),
    { numRuns: PROPERTY_RUNS }
  );
});
