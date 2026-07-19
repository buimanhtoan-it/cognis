# Getting Started

This guide takes you from a fresh machine to a working `cognis` setup. It is
written for first-time users who want a copy/paste-friendly path and a clear
definition of "done".

`cognis` is a single static binary — no Python, no `pip`, no virtual environment.

Choose one distribution path:

1. buy the single ready-to-install ZIP from Polar, or
2. clone the public repository and build the same software from source for free.

The Polar ZIP is the only supported end-user prebuilt download. It contains the
editor `.vsix`, `INSTALL.md`, and Apache-2.0 license; it does not require a
license key or activation. GitHub Releases may provide checksum-verified engine
and model assets used by the managed installer, but they are not a separate
supported product download.

## Before You Start

You need:

- VS Code or Cursor for the editor path
- network access for the managed install, or local engine/model assets
- Rust stable, Git, and Node.js 18+ only when building from source
- about 200 MB of free disk space for the binary and local embedding model

## Two Folders Matter

Keep these two folders separate:

- The folder where the `cognis` binary lives (anywhere on your `PATH`).
- The target repository — the codebase you want `cognis` to index.

For example:

```text
C:\tools\cognis.exe           # the cognis binary (on PATH)
D:\work\my-app                # your target repository
```

Run setup, health checks, and indexing against the target repository.

## Path A: Cursor or VS Code

Use this path if you want the editor to handle MCP configuration and live
indexing for you. The extension can download and manage the backend binary, so
this is the simplest route.

### 1. Install the Extension

For the prebuilt path, extract the ZIP downloaded from Polar and install the
`.vsix` inside it. Polar does not offer a separate VSIX or license-key benefit.

For the free source path:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis/apps/cognis-vscode
npm install
npm run package
```

In Cursor or VS Code, choose **Install from VSIX...**, select the generated or
Polar-bundled `cognis-vscode-<version>.vsix`, and reload if prompted.

### 2. Install the Engine

For the Polar-bundled extension, open the Cognis panel and click **Install
engine**. The extension detects your platform, downloads the matching `cognis`
binary and semantic model, verifies their checksums, and stages them under
editor global storage. No terminal or Python is required.

For a source build, run
`cargo build --release -p cognis --bin cognis --features onnx-download` and set
`cognis.binaryPath` to
`target/release/cognis` (`cognis.exe` on Windows). Provide semantic model assets
through `COGNIS_ONNX_MODEL_DIR` when semantic search is required.

### 3. Open the Target Repository

Open the repository you want to index, then run:

```text
Cognis: Set Up Workspace
```

You can run it from the Command Palette or from the Cognis sidebar. This command
creates `.cognis/` in the target repository, writes **workspace-scoped** MCP
configuration for the editor by default (so only the open repo is started — not
every repo listed in a global host file), starts managed indexing, and runs a
health check. Prefer keeping `cognis.mcpConfigScope` at `workspace` and
`cognis.mcpStdioMode` at `proxy` unless you deliberately need global multi-repo
fan-out or legacy heavy-per-connection stdio. Migration and multi-host lifecycle
details: [mcp-client-config.md](mcp-client-config.md).

### 4. Verify the Setup

The setup is ready when all of these are true:

- `.cognis/uckg.db` exists in the target repository.
- **Cognis: Show Health** reports `overall: ok`.
- The Cognis sidebar shows the workspace as ready or healthy.
- MCP tools appear after reloading the editor or MCP host.

If tools do not appear immediately, reload the editor and run **Cognis: Repair
Setup**.

## Path B: CLI Only

Use this path if you do not want the extension to manage setup.

### 1. Get the Binary

Build the CLI from source. Standalone prebuilt binaries are not an end-user
distribution channel:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --release -p cognis --bin cognis --features onnx-download
# the binary is at target/release/cognis (cognis.exe on Windows)
```

Verify it runs:

```bash
cognis --version
```

### 2. Bootstrap the Target Repository

Move into the target repository and run:

```bash
cd /path/to/my-app
cognis bootstrap .
cognis health
cognis cli mcp-config --host cursor --repo-root .
```

The `bootstrap` command initializes `.cognis/`, runs a full index, and checks
health. Copy the generated MCP configuration into your client configuration if
you are wiring the client manually (see
[mcp-client-config.md](mcp-client-config.md)).

### 3. Start the Server / Daemon

Start the MCP server (stdio):

```bash
cognis mcpd
```

For long editing sessions outside the extension, start live indexing:

```bash
cognis indexd --repo-root /path/to/my-app
```

## Faster First Run

The managed extension provisions the local embedding model during engine setup;
source builds must supply it through `COGNIS_ONNX_MODEL_DIR`. If you want a
faster setup with lexical and structural search only, skip embeddings on the
first index:

```bash
cognis bootstrap . --skip-embeddings
```

Later, re-run indexing without `--skip-embeddings` to enable semantic search.

## Common Problems

### `cognis` Is Not Recognized

Confirm the binary is on your `PATH`, or invoke it by its full path
(e.g. `./target/release/cognis health`).

### MCP Tools Are Missing

Run **Cognis: Troubleshoot & Repair**, then reload the editor or MCP host. If you
configured MCP manually, check that `COGNIS_DB_PATH`, `COGNIS_AUDIT_LOG`, and
`COGNIS_REPO_ROOT` point to the target repository, and that the server command
points at your `cognis` binary's `mcpd` surface.

### Many Idle Cognis Processes / High RAM

Prefer workspace MCP scope and thin-proxy stdio. If older global
`cognis-*` entries remain under `~/.cursor/mcp.json` or `~/.vscode/mcp.json`,
migrate them to the workspace file (repair/connect flows) or remove only the
Cognis entries for closed repos — never wipe unrelated MCP servers. See
[mcp-client-config.md](mcp-client-config.md) and the private-bytes procedure in
[e2e-testing.md](e2e-testing.md).

### Health Is Degraded

Open **Cognis: Show Health** and fix the first reported error. It is normal to
see warnings before the target repository has been initialized and indexed.

## What Good Looks Like

A plug-and-play setup is complete when:

- `cognis --version` works
- the extension is installed (editor path)
- the target repository has `.cognis/config.yaml` and `.cognis/uckg.db`
- health reports `overall: ok`
- MCP tools are visible in the editor
- `retrieve_context_capsule` or `discover_symbols` returns results for the
  target repository

After that, use the editor flow for day-to-day work. Run **Cognis: Repair
Setup** any time MCP configuration or indexing state drifts.

## Next Steps

- [install.md](install.md) for installation details and the binary distribution
- [quickstart.md](quickstart.md) for the CLI indexing flow
- [mcp-client-config.md](mcp-client-config.md) for manual MCP configuration
- [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) for
  extension settings and troubleshooting
