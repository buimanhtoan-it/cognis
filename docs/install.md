# Installation Guide

Cognis has one source tree and two supported ways to install the same software:

1. buy one ready-to-install ZIP from Polar, or
2. build from this Apache-2.0 repository for free.

The Polar ZIP is the only supported end-user prebuilt download. It contains the
VS Code / Cursor `.vsix`, `INSTALL.md`, and license notice. Polar does not offer
a separate VSIX, license key, activation, subscription, or feature unlock.

## Requirements

For the Polar path:

- VS Code 1.85+ or Cursor
- Windows x64, macOS Apple Silicon, or Linux x64
- network access on first managed install for the checksum-verified engine and
  semantic model

For the source path:

- Git and the stable Rust toolchain
- Node.js 18+ to build the editor extension
- local semantic model assets when semantic search is required; see
  [`assets/models/README.md`](../assets/models/README.md)

No path requires Python, `pip`, or a virtual environment.

## Option A - Polar ZIP

1. Purchase and download `cognis-prebuilt-<version>.zip` from Polar.
2. Extract the ZIP. Treat the files inside as one product bundle; do not look
   for a separate Polar VSIX or license-key benefit.
3. In VS Code or Cursor, open Extensions, choose **Install from VSIX...**, and
   select `cognis-vscode-<version>.vsix` from the extracted folder.
4. Open the Cognis panel and click **Install engine**. The extension downloads
   the engine and semantic model matching its version, verifies SHA-256
   sidecars, and stores the assets under editor global storage.
5. Open a repository and run **Cognis: Set Up Workspace**.

There is no key to enter and no activation step. The prebuilt package has the
same features as a source build.

## Option B - build from source

Clone the repository and build the Rust engine:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --release -p cognis --bin cognis --features onnx-download
```

The binary is `target/release/cognis` (`cognis.exe` on Windows). It is a
busybox-style multi-call binary:

- `cognis bootstrap .` initializes, indexes, and checks health
- `cognis mcpd` starts the MCP server on stdio
- `cognis indexd --repo-root .` keeps the index current

Package the editor extension from the same checkout:

```bash
cd apps/cognis-vscode
npm install
npm run package
```

Install the generated `cognis-vscode-<version>.vsix`, then set the advanced
`cognis.binaryPath` setting to the absolute source-built binary path. Set
`COGNIS_ONNX_MODEL_DIR` to a directory containing `model.onnx`,
`tokenizer.json`, and `pooling.json` when semantic search is required.

To stage a stripped binary and SHA-256 sidecar for your own use:

```bash
cargo xtask dist
cargo xtask dist --features onnx-download
```

The second command links ONNX Runtime into the binary; model weights remain
separate local assets.

## Verify

From a target repository:

```bash
cognis --version
cognis bootstrap .
cognis health
```

A ready setup has `.cognis/uckg.db`, reports `overall: ok`, and exposes the MCP
tools after the editor or MCP host reloads.

## Docker from source

The repository does not advertise a separate public container as an end-user
prebuilt product. Build any container deployment from your source checkout and
follow [operations.md](operations.md).

## Upgrading

- Polar path: download the replacement versioned ZIP from the Polar File
  Download benefit, install its bundled VSIX, then use **Reinstall engine** if
  prompted.
- Source path: update the checkout, rebuild the engine and extension, and
  reinstall the generated VSIX.

Run `cognis health` after either path. Re-index when health reports an index or
version mismatch.

## Uninstalling

Use **Cognis: Remove Everything (Prepare for Uninstall)** before uninstalling
the extension, or manually remove the source-built binary and each repository's
`.cognis/` directory. Source files are never deleted.