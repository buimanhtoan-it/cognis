# Getting Started

This guide takes you from a fresh machine to a working `cognis` setup. It is
written for first-time users who want a copy/paste-friendly path and a clear
definition of "done".

The recommended path is:

1. install the `cognis` backend from source
2. install the VS Code / Cursor extension
3. open the repository you want to index
4. run **Cognis: Set Up for AI**
5. verify that MCP tools are available

## Before You Start

You need:

- Python 3.11 or newer
- Git
- VS Code or Cursor
- Node.js 18 or newer, only because the extension is packaged locally as a
  `.vsix`
- about 1 GB of free disk space for dependencies and the local embedding model

`cognis` is not published to PyPI yet. Install it from the source repository.

## Two Folders Matter

Keep these two folders separate:

- The `cognis` source folder is where you clone and install this project.
- The target repository is the codebase you want `cognis` to index.

For example:

```text
D:\PROGRAMING\cognis          # the cognis source folder
D:\work\my-app                # your target repository
```

Run install and extension packaging commands in the `cognis` source folder.
Run setup, health checks, and indexing against the target repository.

## Path A: Cursor or VS Code

Use this path if you want the editor to handle MCP configuration and live
indexing for you.

### 1. Install the Backend

On Windows PowerShell:

```powershell
git clone https://github.com/buimanhtoan-it/cognis;
cd cognis;
python -m venv .venv;
.\.venv\Scripts\Activate.ps1;
python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]";
python -m cognis.cli.main --version;
```

On macOS or Linux:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
python -m venv .venv
source .venv/bin/activate
python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
python -m cognis.cli.main --version
```

If the version command prints a version, the backend is installed.

### 2. Package the Extension

From the `cognis` source folder:

```powershell
python scripts/setup_extension.py --package;
```

This creates a file like:

```text
apps/cognis-vscode/cognis-vscode-<version>.vsix
```

### 3. Install the Extension

In Cursor or VS Code:

1. Open the Extensions view.
2. Open the `...` menu.
3. Choose **Install from VSIX...**.
4. Select `apps/cognis-vscode/cognis-vscode-<version>.vsix`.
5. Reload the editor if prompted.

### 4. Select the Python Interpreter

Select the same Python interpreter used for the backend install.

On Windows, this is usually:

```text
D:\PROGRAMING\cognis\.venv\Scripts\python.exe
```

If the editor does not pick it up automatically, set `cognis.pythonPath` to that
absolute path.

### 5. Open the Target Repository

Open the repository you want to index, not necessarily the `cognis` source
folder.

Then run:

```text
Cognis: Set Up for AI
```

You can run it from the Command Palette or from the Cognis sidebar. This command
creates `.cognis/` in the target repository, writes MCP configuration for the
editor, starts managed indexing, and runs a health check.

### 6. Verify the Setup

The setup is ready when all of these are true:

- `.cognis/uckg.db` exists in the target repository.
- **Cognis: Show Health** reports `overall: ok`.
- The Cognis sidebar shows the workspace as ready or healthy.
- MCP tools appear after reloading the editor or MCP host.

If tools do not appear immediately, reload the editor and run **Cognis: Repair
Setup**.

## Path B: CLI Only

Use this path if you do not want the extension to manage setup.

First, complete the backend install from Path A. Then move into the target
repository.

On Windows PowerShell:

```powershell
cd D:\work\my-app;
python -m cognis.cli.main bootstrap .;
python -m cognis.cli.main health;
python -m cognis.cli.main mcp-config --host cursor --repo-root .;
```

On macOS or Linux:

```bash
cd /path/to/my-app
python -m cognis.cli.main bootstrap .
python -m cognis.cli.main health
python -m cognis.cli.main mcp-config --host cursor --repo-root .
```

The `bootstrap` command initializes `.cognis/`, runs a full index, and checks
health. Copy the generated MCP configuration into your client configuration if
you are wiring the client manually.

Start the MCP server with:

```powershell
python -m cognis_mcpd.main;
```

For long editing sessions outside the extension, start live indexing with:

```powershell
python -m cognis_indexd.main --repo-root D:\work\my-app;
```

## Faster First Run

The first run can take longer because local embedding dependencies and models
may be downloaded. If you want a faster setup with lexical and structural
search only, skip embeddings on the first index:

```powershell
python -m cognis.cli.main bootstrap . --skip-embeddings;
```

Later, re-run indexing without `--skip-embeddings` to enable semantic search.

## Common Problems

### `cognis-cli` Is Not Recognized

Use the module form instead:

```powershell
python -m cognis.cli.main health;
```

The extension also uses module form internally, so it does not require
`cognis-cli` to be on `PATH`.

### The Extension Cannot Find Python

Set `cognis.pythonPath` to the exact Python executable in the environment where
you installed `cognis`.

On Windows, that usually looks like:

```text
D:\PROGRAMING\cognis\.venv\Scripts\python.exe
```

Then run **Cognis: Repair Setup**.

### MCP Tools Are Missing

Run **Cognis: Repair Setup**, then reload the editor or MCP host. If you
configured MCP manually, check that `COGNIS_DB_PATH`, `COGNIS_AUDIT_LOG`, and
`COGNIS_REPO_ROOT` point to the target repository.

### Health Is Degraded

Open **Cognis: Show Health** and fix the first reported error. It is normal to
see warnings before the target repository has been initialized and indexed.

## What Good Looks Like

A plug-and-play setup is complete when:

- the backend version command works
- the extension is installed
- the target repository has `.cognis/config.yaml` and `.cognis/uckg.db`
- health reports `overall: ok`
- MCP tools are visible in the editor
- `retrieve_context_capsule` or `discover_symbols` returns results for the
  target repository

After that, use the editor flow for day-to-day work. Run **Cognis: Repair
Setup** any time Python, MCP configuration, or indexing state drifts.

## Next Steps

- [install.md](install.md) for installation details and optional dependencies
- [quickstart.md](quickstart.md) for the CLI indexing flow
- [mcp-client-config.md](mcp-client-config.md) for manual MCP configuration
- [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) for
  extension settings and troubleshooting
