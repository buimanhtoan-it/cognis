# Installation Guide

Cognis ships as an Apache-2.0 Rust engine (`cognis`) paired with a VS Code / Cursor extension (`cognis-mcp`). You can install prebuilt release artifacts or build everything from source.

## System Requirements

- **Supported OS / Architecture (managed prebuilt binaries):**
  - Windows x64 (`x86_64-pc-windows-msvc`)
  - macOS Apple Silicon (`aarch64-apple-darwin`)
  - Linux x64 (`x86_64-unknown-linux-gnu`)
- **Editors / MCP Hosts:** VS Code 1.85+, Cursor, Antigravity, Claude Code / Claude Desktop, Kiro
- **Build-from-source prerequisites:** Git, stable [Rust toolchain](https://rustup.rs), and Node.js 20+ (for the VS Code / Cursor extension)

SQLite (with FTS5) is compiled directly into the `cognis` binary — no external database or runtime is required.

---

## Option A — Install via VS Code / Cursor Extension (`.vsix`)

1. Download `cognis-mcp-<version>.vsix` from [GitHub Releases](https://github.com/buimanhtoan-it/cognis/releases) (or package it locally via Option B).
2. In VS Code or Cursor, open **Extensions** (`Ctrl+Shift+X` / `Cmd+Shift+X`) -> `...` menu -> **Install from VSIX...** and select the `.vsix` file.
3. Open the **Cognis** view in the Activity Bar and click **Install engine**. The extension downloads the version-matched `cognis` binary and `bge-small-en-v1.5` ONNX semantic model assets, verifies their `.sha256` checksums, and stores them in editor global storage.
4. Open any repository and run **Cognis: Set Up Workspace**.

---

## Option B — Build from Source

### 1. Build the `cognis` Rust binary

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis

# Standard build (lexical + structural + AST complexity + CSIA/CSAR; offline-friendly)
cargo build --release -p cognis --bin cognis

# Full release build with statically-linked ONNX Runtime for local semantic search
cargo build --release -p cognis --bin cognis --features onnx-download
```

The resulting single multi-call binary is located at `target/release/cognis` (`target\release\cognis.exe` on Windows):

- `cognis bootstrap .` — initializes `.cognis/`, runs a full index, and verifies health
- `cognis mcpd` — starts the 12-tool MCP server on `stdio`
- `cognis indexd --repo-root .` — watches files and updates the index incrementally

To stage a release-ready stripped binary + `.sha256` sidecar under `dist/`:

```bash
cargo xtask dist --features onnx-download
```

### 2. Build and package the VS Code / Cursor extension

```bash
cd apps/cognis-mcp
npm install
npm run package
```

Install the generated `apps/cognis-mcp/cognis-mcp-0.1.0.vsix` in VS Code or Cursor. If using a custom source-built binary, set `cognis.binaryPath` in editor settings to the absolute path of `cognis` (`cognis.exe`). For local semantic embeddings on source builds, see [`assets/models/README.md`](../assets/models/README.md).

---

## Verify Installation

From any repository root:

```bash
cognis --version
cognis bootstrap .
cognis health
```

A healthy workspace creates `.cognis/uckg.db`, prints `overall: ok`, and is immediately ready for MCP queries and real-time editor guardrails.

---

## Uninstalling

- **In VS Code / Cursor:** Run **Cognis: Remove Everything (Prepare for Uninstall)** from the Command Palette to stop background daemons, clean `.cognis/`, remove Cognis MCP entries without touching unrelated MCP servers, and remove managed binaries — then uninstall the extension.
- **CLI-only:** Delete the `cognis` binary and remove `.cognis/` inside indexed repositories. Your source code is never modified or deleted.
