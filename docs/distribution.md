# Single-binary distribution (Rust engine, G2)

> Spec: `rust-engine-migration` Task 10.1 — Requirements 8.1, 8.2.
> Evidence discipline (`docs/development-criteria.md`): each claim is labelled
> **proven** / **empirically-supported (n=…)** / **conjectured**.

The shipped product is **one static `cognis` binary per platform**. The binary
is busybox-style multi-call (`bins/cognis`): dispatched by `argv[0]` or a leading
subcommand it behaves as `cognis-cli`, `cognis-mcpd`, or `cognis-indexd`, so a
single artifact covers every surface. This closes the largest real gap with the
C competitor (CBM) — distribution, not raw speed.

## What is linked in

| Component | Strategy | Requirement |
| --- | --- | --- |
| SQLite + FTS5 | **Bundled** — `rusqlite` `bundled` feature compiles SQLite (with FTS5) from C source into the binary. No system SQLite. | 8.1, 8.2 |
| sqlite-vec (`vec0` KNN) | Optional. Default builds ship the in-Rust **BLOB + linear-scan fallback** (Requirement 2.4), so the binary is self-contained with no extension. A `vec0` loadable extension is loaded at runtime when `COGNIS_SQLITE_VEC_PATH` is set (store `sqlite-vec` feature). Static embedding of the extension is the documented next step. | 2.4, 8.2 |
| ONNX embedding model | **Fetch-on-first-run**, checksum-verified. The default binary builds *without* the `onnx` feature (stub/zero-vec embedder, fully offline). The production embedder (`--features onnx`) resolves `bge-small-en-v1.5` from `assets/models/<leaf>/` or `COGNIS_ONNX_MODEL_DIR`; the ONNX export ships as checked-in assets. `--features onnx-download` links ONNX Runtime statically for a self-contained binary. | 7.2, 8.2 |
| Python runtime | **None.** No Python or PyTorch is required at runtime. | 8.2 |

## Build profile

`[profile.release]` (root `Cargo.toml`): `opt-level = 3`, `lto = true`,
`codegen-units = 1`, `strip = "symbols"`. `panic` stays the default (unwind) at
the workspace level so proptest / `should_panic` tests keep working; the CSAR
`cdylib` keeps its own `panic = "abort"` in `native/csar-rs`.

## Local build / staging

```sh
cargo xtask dist                          # host target → dist/
cargo xtask dist --target <triple>        # a specific target
cargo xtask dist --use-cross --target aarch64-unknown-linux-gnu
cargo xtask dist --features onnx-download # self-contained ONNX runtime
```

`xtask dist` builds `--release -p cognis --bin cognis`, copies the artifact to
`dist/cognis-<triple>[.exe]`, and writes a `dist/cognis-<triple>[.exe].sha256`
sidecar in `sha256sum -c` format (public checksum — the trust posture the design
calls for alongside CI signing/provenance).

## Release matrix (CI)

`.github/workflows/release.yml` job `dist-binaries` builds the five target
platforms on tag push. Native runners cover host-architecture targets; the Linux
aarch64 target cross-compiles with `cross` (Docker-backed, `Cross.toml`). Each
target uploads its binary + checksum; `github-release` attaches them to the
GitHub Release.

| Platform | Target triple | Build path | Status |
| --- | --- | --- | --- |
| Linux amd64 | `x86_64-unknown-linux-gnu` | native (ubuntu) | CI-configured |
| Linux arm64 | `aarch64-unknown-linux-gnu` | `cross` (ubuntu) | CI-configured |
| macOS arm64 | `aarch64-apple-darwin` | native (macos) | CI-configured |
| macOS amd64 | `x86_64-apple-darwin` | native (macos) | CI-configured |
| Windows amd64 | `x86_64-pc-windows-msvc` | native (windows) | CI-configured |
| **Host (dev machine)** | resolved by `rustc -vV` | `cargo xtask dist` | **verified** |

**Verified vs CI-configured (honest status):** only the **host** target was
built and staged in this environment (`cargo build --release -p cognis` +
`cargo xtask dist`) — *empirically-supported, n=1, single machine*. The other
four targets are **configured** (matrix + `cross` + `xtask`) but not executed
here: cross-compiling all five requires the platform runners / Docker images the
CI matrix provides. Cross-target success is **conjectured** until the matrix runs
on a tagged release.

## Extension "Install backend" (fetch binary, no pip) — Task 10.2

The VS Code / Cursor extension's **Install backend** flow downloads the prebuilt
single `cognis` binary instead of a package-manager install (Requirement 1.1 — a
user gets a working backend with no Python, pip, or compiler):

1. **Platform detection** maps `process.platform`/`process.arch` to one of the
   five published target triples (`apps/cognis-vscode/src/binary.ts`
   `detectTargetTriple`). An unsupported platform fails with a clear message
   asking the user to open an issue for a build.
2. **Fetch** the release asset `cognis-<triple>[.exe]` and its `.sha256` sidecar
   from the GitHub Release matching the extension version
   (`https://github.com/<repo>/releases/download/v<version>/…`). Override the
   source with `cognis.binaryRepo` / `cognis.binaryDownloadBaseUrl` for offline
   mirrors or pre-release testing.
3. **Checksum verification** — the SHA-256 of the downloaded bytes is compared to
   the sidecar (`sha256sum -c` format) before anything is written. A mismatch (or
   an unverifiable sidecar) aborts the install; the binary is never staged.
4. **Stage** the verified binary under the extension's global storage
   (`<globalStorage>/bin/cognis[.exe]`, `chmod +x` on POSIX) with a version
   marker for drift detection after an extension update.

Once the binary is installed it is the active backend: the extension invokes it
multi-call (`cognis cli …`, `cognis mcpd …`, `cognis indexd …`) and the generated
`mcp.json` server block points at **`<binary> mcpd`**, preserving the env
(`COGNIS_DB_PATH`, timeouts) and server name. The engine is pure Rust — there is
no Python runtime, pip, or PyPI package involved anywhere in this flow.

> **Verified vs network-gated (honest status):** the platform-detection,
> URL-building, checksum-parse/verify, mcp.json rewrite, and invocation-resolution
> logic are covered by offline unit tests (`src/test/binary.test.ts`, download
> injected — no network) — *empirically-supported, n=1 host*. Downloading a real
> release asset is **not** exercised here (no published binary in this
> environment); that path is **configured** and runs against the release matrix.

## Signing / provenance

SLSA provenance + signed artifacts (cosign/sigstore) are the target trust posture
(design "Build / Distribution / Packaging") and layer onto the `dist-binaries`
job + published `.sha256` checksums. Implementing the signing step is tracked
separately from the build matrix.
