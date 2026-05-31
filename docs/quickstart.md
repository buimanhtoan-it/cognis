# Quickstart

This guide gets one repository indexed and ready for use from an MCP client.

## Before you start

Complete the backend installation described in [install.md](install.md). At the
end of that process, `cognis-cli` and `cognis-mcpd` should be available.

## Fastest path

From the repository you want to index:

```bash
cognis-cli bootstrap .
```

This command runs:

1. `init`
2. a full index
3. a health check

If `cognis-cli` is not on `PATH`, use the module form:

```powershell
python -m cognis.cli.main bootstrap .
```

If you want a faster first run without semantic embeddings:

```bash
cognis-cli bootstrap . --skip-embeddings
```

## Step 1: prepare the target repository

Move into the repository you want to understand:

```bash
cd /path/to/your/repo
```

If you prefer the explicit steps instead of `bootstrap`, run:

```bash
cognis-cli init
cognis-cli index --full .
cognis-cli health
```

This creates a `.cognis/` directory containing the local configuration, index
database, cache, and audit log.

## Step 2: verify health

Run:

```bash
cognis-cli health
```

You want to see:

- the configuration file loading successfully
- the database present and writable
- the embedder check passing, unless you intentionally skipped embeddings
- `overall: ok`

If the health check fails, resolve that before moving on to MCP client
configuration.

## Step 3: start the MCP server

Start the server in the same environment where you installed `cognis`:

```bash
cognis-mcpd
```

This starts the MCP server on stdio. Your client configuration should launch
this process.

## Step 4: connect a client

Configure the MCP client you want to use:

- Claude Code or Claude Desktop
- Cursor
- VS Code
- Cline / Roo Cline

See [mcp-client-config.md](mcp-client-config.md) for exact configuration
examples.

## Step 5: run the first queries

Once the client is connected, prefer discovery-first queries that minimize agent
round trips:

```text
cognis: discover_symbols("validate jwt", k=8)
cognis: resolve_symbols(["ts:src/auth/jwt.ts:validate@...", "..."])
cognis: dependency_trace("ts:src/auth/jwt.ts:validate@...", direction="in", depth=3)
```

For a task-oriented request, start with the capsule tool instead of chaining
individual lookups:

```text
cognis: retrieve_context_capsule(
    "Why is the /login endpoint timing out under load?",
    max_tokens=8000
)
```

**Tool choice quick guide**

| When you… | Use |
| --- | --- |
| Need candidates when name or intent is unclear | `discover_symbols` |
| Need lexical matches for a known name fragment | `symbol_search` |
| Already have an id or qualified name | `symbol_lookup` |
| Need full records for several discovered ids | `resolve_symbols` |
| Need meaning-based matches only | `semantic_search` |
| Need callers/callees from a known symbol | `dependency_trace` |
| Want task context in one response | `retrieve_context_capsule` |

## Optional: use the VS Code / Cursor extension

If you want editor integration, package the extension from the `cognis`
repository root:

1. package the extension:
   ```bash
   python scripts/setup_extension.py --package
   ```
2. install the generated `.vsix`
3. open the target repository in the editor
4. select the same Python interpreter used for the `cognis` install
5. run **Cognis: Set Up for AI**

See [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) for the
extension workflow.

## Optional: keep the index current

For long editing sessions outside the extension, run the watcher:

```bash
cognis-indexd --repo-root /path/to/your/repo
```

This keeps the index up to date as files change.

## Quick reference

| Command | Purpose |
| --- | --- |
| `cognis-cli bootstrap .` | Initialize, index, and check health in one command |
| `cognis-cli init` | Create `.cognis/` in the current repository |
| `cognis-cli index --full .` | Run a full index |
| `cognis-cli health` | Report configuration and runtime status |
| `cognis-mcpd` | Start the MCP server |
| `cognis-indexd --repo-root .` | Start live indexing |

## Troubleshooting shortcuts

- On Windows, use `python -m cognis.cli.main ...` if console scripts are not on `PATH`.
- If embeddings were skipped, semantic search will remain unavailable until you re-index without `--skip-embeddings`.
- If the client cannot connect, re-check `COGNIS_DB_PATH`, the MCP command, and the selected Python interpreter.

## Next steps

- [install.md](install.md) for installation details and troubleshooting
- [mcp-client-config.md](mcp-client-config.md) for client-specific configuration
- [operations.md](operations.md) for Docker-based deployment
