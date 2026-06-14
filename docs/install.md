# Installation Guide

This guide covers the local installation flow for `cognis`, optional editor
integration, and the checks you should run before using it on a real codebase.

## Requirements

- Python 3.11 or newer
- Linux, macOS, or Windows
- Node.js 18 or newer only if you plan to build the VS Code / Cursor extension
- roughly 1 GB of free disk space for the backend and local embedding model

`cognis` is not published to PyPI yet. Install it from source.

## Recommended local installation

Clone the repository and create a virtual environment:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
python -m venv .venv
```

Activate the virtual environment:

- macOS / Linux: `source .venv/bin/activate`
- Windows PowerShell: `.\.venv\Scripts\Activate.ps1`

Install the Python backend:

```bash
python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
```

This installs the command-line tools:

- `cognis-cli`
- `cognis-mcpd`
- `cognis-indexd`

There is no top-level `cognis` wrapper command yet. In terminals, use
`cognis-cli` or the module form shown below.

If you are setting up a contributor environment, you can use one of the helper
paths below instead of running the commands manually:

- Windows PowerShell: `.\scripts\setup-dev.ps1`
- macOS / Linux: `./scripts/setup-dev.sh`
- Make: `make install-dev`
- Invoke: `invoke install-dev`

## Verify the backend installation

Run:

```bash
cognis-cli --version
cognis-cli health
```

If `cognis-cli` is not available on `PATH`, use the module form:

```bash
python -m cognis.cli.main --version
python -m cognis.cli.main health
```

The same fallback works for the daemon entry points:

- MCP server: `python -m cognis_mcpd.main`
- Indexer daemon: `python -m cognis_indexd.main`

It is normal for `cognis-cli health` to report warnings before you initialize a
repository. The first successful `init` or `bootstrap` run creates `.cognis/`
and the local database.

## Optional: build the VS Code / Cursor extension

If you want editor integration, package the extension from the repository root
after the Python backend is installed:

```bash
python scripts/setup_extension.py --package
```

This creates `apps/cognis-vscode/cognis-vscode-<version>.vsix`.

Install the package in VS Code or Cursor:

1. Open the Extensions view.
2. Open the `...` menu.
3. Select **Install from VSIX...**
4. Choose `cognis-vscode-<version>.vsix`.
5. Select the same Python interpreter you used for the `cognis` install, or set `cognis.pythonPath`.
6. Open the target repository and run **Cognis: Set Up Workspace**.

For VS Code / Cursor users, this is the recommended starting flow. The
extension uses the selected Python interpreter directly, so it does not require
`cognis-cli` to be on `PATH`.

For extension-specific details, see [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md).

## Optional dependencies

The editable install above uses the full supported feature set. If you need to
understand the extras individually, use this table:

| Extra | Purpose |
| --- | --- |
| `indexer` | Tree-sitter parsers and the file watcher |
| `embed-local` | Local embeddings with `sentence-transformers` |
| `vector` | Vector search with `sqlite-vec` |
| `tokenizers` | Token counting for capsule budgeting |
| `mcp` | MCP server runtime |

## sqlite-vec

`cognis` uses `sqlite-vec` for vector search. The dependency is already covered
by the `vector` extra, but you can test it directly:

```python
import sqlite3
import sqlite_vec

conn = sqlite3.connect(":memory:")
conn.enable_load_extension(True)
sqlite_vec.load(conn)
print("sqlite-vec loaded OK")
```

Platform notes:

- Linux: prebuilt wheels work on supported architectures
- macOS: prebuilt wheels work on supported architectures
- Windows: use the official CPython build when possible

If the extension does not load, lexical and structural retrieval still work, but
vector search remains unavailable until `sqlite-vec` loads correctly.

## Docker deployment

For a persistent self-hosted deployment, use Docker Compose:

```bash
export WORKSPACE_HOST_PATH=/path/to/your/codebase
docker compose -f deploy/compose.yaml up -d
```

Published image tags use `ghcr.io/buimanhtoan-it/cognis-engine:<version>`.
Operational steps are documented in [operations.md](operations.md).

## Windows notes

On Windows, the most common problem is that Python installs console scripts to a
directory that is not on `PATH`. The module form avoids that problem entirely:

```powershell
python -m cognis.cli.main bootstrap .
python -m cognis.cli.main health
python -m cognis_mcpd.main
```

If you want the console scripts for the current terminal session, add the Python
Scripts directory to `PATH`:

```powershell
$scripts = python -c "import sysconfig; print(sysconfig.get_path('scripts'))"
$env:Path = "$scripts;$env:Path"
cognis-cli --version
```

If you mainly use the VS Code / Cursor extension, selecting the correct Python
interpreter is usually enough. The extension runs `python -m ...` internally and
does not depend on the console scripts being visible on `PATH`.

## Upgrading

After updating the checkout, reinstall and re-run the health check:

```bash
python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
cognis-cli health
```

If `health` reports a version mismatch, run a full re-index:

```bash
cognis-cli index --full .
```

## Uninstalling

```bash
pip uninstall cognis
```

Optional cleanup:

- remove the virtual environment
- remove `.cognis/` from repositories you indexed
- uninstall the `.vsix` from the editor if you no longer need it

## Troubleshooting

| Problem | What to check |
| --- | --- |
| `cognis-cli` is not recognized | Activate the virtual environment or use `python -m cognis.cli.main` |
| `fastmcp` is missing | Reinstall with the `mcp` extra |
| `sentence_transformers` is missing | Reinstall with the `embed-local` extra |
| `sqlite-vec` does not load | Confirm the Python build supports extension loading |
| Permission errors under `.cognis/` | Verify the current user can create and write files in the repository |
| `COGNIS_DB_PATH` is incorrect | Re-run `cognis-cli init` or inspect `.cognis/config.yaml` and your environment |
