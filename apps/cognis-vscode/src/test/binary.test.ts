// Harness first: installs the vscode stub before binary.ts (which imports
// vscode) is required.
import "./testHarness";

import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import {
  assetDownloadUrl,
  binaryAssetName,
  binarySubcommand,
  checksumAssetName,
  classifyBinaryFetchFailure,
  detectTargetTriple,
  initManagedBinary,
  installManagedBinary,
  installedBinaryVersion,
  isManagedBinaryInstalled,
  managedBinaryPath,
  parseSha256Sidecar,
  releaseAssetBaseUrl,
  releaseTag,
  resolveCliInvocation,
  resolveMcpdInvocation,
  sha256Hex,
  uninstallManagedBinary,
  verifyDownloadedBinary,
  type DownloadResponse,
} from "../binary";
import {
  buildBinaryStdioServerBlock,
  rewriteServerBlockToBinary,
} from "../mcpServer";

// ---------------------------------------------------------------------------
// Platform detection: the five published targets, and "no binary here".
// ---------------------------------------------------------------------------

test("detectTargetTriple maps every published platform to its triple", () => {
  assert.equal(detectTargetTriple("win32", "x64"), "x86_64-pc-windows-msvc");
  assert.equal(detectTargetTriple("darwin", "arm64"), "aarch64-apple-darwin");
  assert.equal(detectTargetTriple("darwin", "x64"), "x86_64-apple-darwin");
  assert.equal(detectTargetTriple("linux", "x64"), "x86_64-unknown-linux-gnu");
  assert.equal(detectTargetTriple("linux", "arm64"), "aarch64-unknown-linux-gnu");
});

test("detectTargetTriple returns undefined for an unsupported platform", () => {
  assert.equal(detectTargetTriple("win32", "arm64"), undefined);
  assert.equal(detectTargetTriple("linux", "ia32"), undefined);
  assert.equal(detectTargetTriple("aix", "ppc64"), undefined);
});

// ---------------------------------------------------------------------------
// Asset naming + URLs.
// ---------------------------------------------------------------------------

test("binaryAssetName adds .exe only on the Windows triple", () => {
  assert.equal(binaryAssetName("x86_64-pc-windows-msvc"), "cognis-x86_64-pc-windows-msvc.exe");
  assert.equal(binaryAssetName("x86_64-unknown-linux-gnu"), "cognis-x86_64-unknown-linux-gnu");
  assert.equal(binaryAssetName("aarch64-apple-darwin"), "cognis-aarch64-apple-darwin");
});

test("checksumAssetName appends .sha256 to the binary asset", () => {
  assert.equal(
    checksumAssetName("x86_64-pc-windows-msvc"),
    "cognis-x86_64-pc-windows-msvc.exe.sha256"
  );
  assert.equal(
    checksumAssetName("x86_64-unknown-linux-gnu"),
    "cognis-x86_64-unknown-linux-gnu.sha256"
  );
});

test("releaseTag prefixes a bare version and leaves a v-tag intact", () => {
  assert.equal(releaseTag("0.7.3"), "v0.7.3");
  assert.equal(releaseTag("v0.7.3"), "v0.7.3");
});

test("releaseAssetBaseUrl builds the GitHub release path by default", () => {
  assert.equal(
    releaseAssetBaseUrl("owner/repo", "0.7.3"),
    "https://github.com/owner/repo/releases/download/v0.7.3"
  );
});

test("releaseAssetBaseUrl honors an override and trims trailing slashes", () => {
  assert.equal(
    releaseAssetBaseUrl("owner/repo", "0.7.3", "https://mirror.example.com/cognis/0.7.3/"),
    "https://mirror.example.com/cognis/0.7.3"
  );
});

test("assetDownloadUrl joins base and asset name", () => {
  assert.equal(
    assetDownloadUrl("https://github.com/o/r/releases/download/v1", "cognis-x.exe"),
    "https://github.com/o/r/releases/download/v1/cognis-x.exe"
  );
});

// ---------------------------------------------------------------------------
// Checksum: parse + verify (the trust-on-download decision).
// ---------------------------------------------------------------------------

test("parseSha256Sidecar reads the sha256sum -c format and bare hex", () => {
  const hex = "a".repeat(64);
  assert.equal(parseSha256Sidecar(`${hex}  cognis-x86_64-unknown-linux-gnu`), hex);
  assert.equal(parseSha256Sidecar(`${hex} *cognis-x86_64-pc-windows-msvc.exe`), hex);
  assert.equal(parseSha256Sidecar(`${hex}\n`), hex);
});

test("parseSha256Sidecar lowercases and returns undefined when absent", () => {
  assert.equal(parseSha256Sidecar("A".repeat(64)), "a".repeat(64));
  assert.equal(parseSha256Sidecar("not a checksum"), undefined);
  assert.equal(parseSha256Sidecar(""), undefined);
});

test("verifyDownloadedBinary passes only when the digest matches the sidecar", () => {
  const body = Buffer.from("the cognis engine binary bytes");
  const hex = sha256Hex(body);
  const ok = verifyDownloadedBinary(body, `${hex}  cognis-bin`);
  assert.equal(ok.ok, true);
  assert.equal(ok.expected, hex);
  assert.equal(ok.actual, hex);

  const bad = verifyDownloadedBinary(body, `${"0".repeat(64)}  cognis-bin`);
  assert.equal(bad.ok, false);

  // No parseable digest → never trusted.
  const missing = verifyDownloadedBinary(body, "garbage sidecar");
  assert.equal(missing.ok, false);
});

test("sha256Hex matches Node's crypto for a known input", () => {
  const data = Buffer.from("hello cognis");
  const expected = crypto.createHash("sha256").update(data).digest("hex");
  assert.equal(sha256Hex(data), expected);
});

test("binarySubcommand maps each surface to its multi-call subcommand", () => {
  assert.equal(binarySubcommand("cli"), "cli");
  assert.equal(binarySubcommand("mcpd"), "mcpd");
  assert.equal(binarySubcommand("indexd"), "indexd");
});

// ---------------------------------------------------------------------------
// Fetch-failure classification.
// ---------------------------------------------------------------------------

test("classifyBinaryFetchFailure maps network/404/other to specific guidance", () => {
  const offline = classifyBinaryFetchFailure(0, "x86_64-unknown-linux-gnu", "0.7.3");
  assert.match(offline.userMessage, /internet connection/i);

  const notFound = classifyBinaryFetchFailure(404, "x86_64-unknown-linux-gnu", "0.7.3");
  assert.match(notFound.userMessage, /no cognis engine binary for your platform/i);

  const other = classifyBinaryFetchFailure(503, "x86_64-unknown-linux-gnu", "0.7.3");
  assert.match(other.userMessage, /HTTP 503/);
});

// ---------------------------------------------------------------------------
// mcp.json: the server block now launches the binary's mcpd surface.
// ---------------------------------------------------------------------------

test("buildBinaryStdioServerBlock launches <binary> mcpd with env preserved", () => {
  const env = { COGNIS_DB_PATH: "/repo/.cognis/uckg.db" };
  const block = buildBinaryStdioServerBlock("/bin/cognis", env);
  assert.equal(block.command, "/bin/cognis");
  assert.deepEqual(block.args, ["mcpd"]);
  assert.deepEqual(block.env, env);
});

test("rewriteServerBlockToBinary drops a legacy -m <module> prefix to <binary> mcpd", () => {
  const legacyBlock = {
    command: "some-interpreter",
    args: ["-m", "legacy_module", "--transport", "stdio"],
    env: { COGNIS_DB_PATH: "/repo/.cognis/uckg.db" },
  };
  const rewritten = rewriteServerBlockToBinary(legacyBlock, "/bin/cognis");
  assert.equal(rewritten.command, "/bin/cognis");
  // The -m <module> prefix is dropped; trailing flags are preserved.
  assert.deepEqual(rewritten.args, ["mcpd", "--transport", "stdio"]);
  assert.deepEqual(rewritten.env, legacyBlock.env);
});

test("rewriteServerBlockToBinary tolerates a block with no module prefix", () => {
  const block = { command: "python", args: [], env: {} };
  const rewritten = rewriteServerBlockToBinary(block, "/bin/cognis");
  assert.deepEqual(rewritten.args, ["mcpd"]);
});

// ---------------------------------------------------------------------------
// End-to-end install (no network): download injected, checksum verified.
// ---------------------------------------------------------------------------

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

test("installManagedBinary downloads, verifies the checksum, and stages the binary", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-ok-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  const triple = detectTargetTriple();
  assert.ok(triple, "test host must be a supported platform");
  const body = Buffer.from("FAKE COGNIS ENGINE BINARY");
  const hex = sha256Hex(body);
  const requested: string[] = [];
  const download = async (url: string): Promise<DownloadResponse> => {
    requested.push(url);
    if (url.endsWith(".sha256")) {
      return { ok: true, status: 200, body: Buffer.from(`${hex}  ${binaryAssetName(triple!)}\n`) };
    }
    return { ok: true, status: 200, body };
  };

  const outcome = await installManagedBinary(silentProgress() as never, noCancelToken() as never, {
    download,
  });

  assert.equal(outcome.triple, triple);
  assert.equal(outcome.version, "9.9.9");
  assert.equal(outcome.bytes, body.length);
  assert.equal(isManagedBinaryInstalled(), true);
  assert.equal(installedBinaryVersion(), "9.9.9");
  // The file on disk is exactly the verified bytes.
  assert.deepEqual(fs.readFileSync(managedBinaryPath()!), body);
  // Both the binary and its checksum sidecar were fetched.
  assert.equal(requested.length, 2);
  assert.ok(requested.some((u) => u.endsWith(".sha256")));

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("installManagedBinary refuses to install on a checksum mismatch", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-bad-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  const body = Buffer.from("FAKE COGNIS ENGINE BINARY");
  const download = async (url: string): Promise<DownloadResponse> => {
    if (url.endsWith(".sha256")) {
      // A digest that does NOT match the body → must be rejected.
      return { ok: true, status: 200, body: Buffer.from(`${"0".repeat(64)}  cognis\n`) };
    }
    return { ok: true, status: 200, body };
  };

  await assert.rejects(
    () => installManagedBinary(silentProgress() as never, noCancelToken() as never, { download }),
    /checksum verification/i
  );
  // Nothing was staged.
  assert.equal(isManagedBinaryInstalled(), false);

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("installManagedBinary surfaces a clear error when the release is missing (404)", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-404-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  const download = async (): Promise<DownloadResponse> => ({
    ok: false,
    status: 404,
    body: Buffer.alloc(0),
  });

  await assert.rejects(
    () => installManagedBinary(silentProgress() as never, noCancelToken() as never, { download }),
    /no cognis engine binary for your platform/i
  );
  assert.equal(isManagedBinaryInstalled(), false);

  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("uninstallManagedBinary removes a staged binary and reports it", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-rm-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  const body = Buffer.from("bin");
  const hex = sha256Hex(body);
  const triple = detectTargetTriple()!;
  await installManagedBinary(silentProgress() as never, noCancelToken() as never, {
    download: async (url) =>
      url.endsWith(".sha256")
        ? { ok: true, status: 200, body: Buffer.from(`${hex}  ${binaryAssetName(triple)}`) }
        : { ok: true, status: 200, body },
  });
  assert.equal(isManagedBinaryInstalled(), true);

  const result = await uninstallManagedBinary();
  assert.equal(result.removed, true);
  assert.equal(isManagedBinaryInstalled(), false);

  fs.rmSync(storageDir, { recursive: true, force: true });
});

// ---------------------------------------------------------------------------
// Invocation resolution: binary preferred once installed, Python otherwise.
// ---------------------------------------------------------------------------

test("resolveCliInvocation always uses the managed binary's cli surface", async () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-inv-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  // Always the binary path + cli subcommand, even before the file is staged.
  const before = resolveCliInvocation("/repo", ["health"]);
  assert.equal(before.command, managedBinaryPath());
  assert.deepEqual(before.args, ["cli", "--repo-root", "/repo", "health"]);

  const body = Buffer.from("bin");
  const hex = sha256Hex(body);
  const triple = detectTargetTriple()!;
  await installManagedBinary(silentProgress() as never, noCancelToken() as never, {
    download: async (url) =>
      url.endsWith(".sha256")
        ? { ok: true, status: 200, body: Buffer.from(`${hex}  ${binaryAssetName(triple)}`) }
        : { ok: true, status: 200, body },
  });

  const after = resolveCliInvocation("/repo", ["health"]);
  assert.equal(after.command, managedBinaryPath());
  assert.deepEqual(after.args, ["cli", "--repo-root", "/repo", "health"]);

  const mcpd = resolveMcpdInvocation(["--transport", "stdio"]);
  assert.equal(mcpd.command, managedBinaryPath());
  assert.deepEqual(mcpd.args, ["mcpd", "--transport", "stdio"]);

  await uninstallManagedBinary();
  fs.rmSync(storageDir, { recursive: true, force: true });
});

test("COGNIS_BINARY_PATH override drives the binary without an install", () => {
  const storageDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-bin-ovr-"));
  initManagedBinary(fakeContext(storageDir, "9.9.9") as never, "9.9.9");

  // A real file on disk stands in for a prebuilt engine binary.
  const exe = path.join(storageDir, process.platform === "win32" ? "cognis.exe" : "cognis");
  fs.writeFileSync(exe, "binary");

  const prev = process.env.COGNIS_BINARY_PATH;
  process.env.COGNIS_BINARY_PATH = exe;
  try {
    assert.equal(managedBinaryPath(), exe, "override wins over managed path");
    assert.equal(isManagedBinaryInstalled(), true, "override file exists → installed");

    const cli = resolveCliInvocation("/repo", ["health"]);
    assert.equal(cli.command, exe);
    assert.deepEqual(cli.args, ["cli", "--repo-root", "/repo", "health"]);

    const mcpd = resolveMcpdInvocation(["--transport", "http", "--port", "1"]);
    assert.equal(mcpd.command, exe);
    assert.deepEqual(mcpd.args, ["mcpd", "--transport", "http", "--port", "1"]);
  } finally {
    if (prev === undefined) {
      delete process.env.COGNIS_BINARY_PATH;
    } else {
      process.env.COGNIS_BINARY_PATH = prev;
    }
    fs.rmSync(storageDir, { recursive: true, force: true });
  }
});
