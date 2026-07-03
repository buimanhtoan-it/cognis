# Getting Started

This guide takes you from a fresh machine to a working `cognis` setup. It is
written for first-time users who want a copy/paste-friendly path and a clear
definition of "done".

`cognis` is a single static binary — no Python, no `pip`, no virtual environment.

The recommended path is:

1. get the `cognis` binary (download a prebuilt release or build from source)
2. install the VS Code / Cursor extension
3. open the repository you want to index
4. run **Cognis: Set Up Workspace**
5. verify that MCP tools are available

## Before You Start

You need:

- The `cognis` binary for your platform (prebuilt download) **or** the
  [Rust toolchain](https://rustup.rs) + Git to build it from source
- VS Code or Cursor (for the editor path)
- Node.js 18 or newer only if you build the extension `.vsix` locally
- about 200 MB of free disk space for the binary and the local embedding model

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

Build the `.vsix` from `apps/cognis-vscode` (or use the prebuilt Pro build):

```bash
cd apps/cognis-vscode
npm install
npm run package
```

In Cursor or VS Code:

1. Open the Extensions view.
2. Open the `...` menu.
3. Choose **Install from VSIX...**.
4. Select `apps/cognis-vscode/cognis-vscode-<version>.vsix`.
5. Reload the editor if prompted.

### 2. Install the Backend

Open the Cognis panel and click **Install backend**. The extension detects your
platform, downloads the matching `cognis` binary, verifies its checksum, and
stages it under the extension's storage. No terminal, no Python.

If you prefer to manage the binary yourself, put a `cognis` binary on your
`PATH` (see Path B) — the extension will use it.

### 3. Open the Target Repository

Open the repository you want to index, then run:

```text
Cognis: Set Up Workspace
```

You can run it from the Command Palette or from the Cognis sidebar. This command
creates `.cognis/` in the target repository, writes MCP configuration for the
editor (pointing at the `cognis` binary's `mcpd` surface), starts managed
indexing, and runs a health check.

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

Either download a prebuilt release and put it on your `PATH`, or build from
source:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --release
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

The first run can take longer because the local embedding model may be fetched
on first use. If you want a faster setup with lexical and structural search
only, skip embeddings on the first index:

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
