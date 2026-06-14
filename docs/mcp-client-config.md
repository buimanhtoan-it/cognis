# MCP Client Configuration

This guide explains how to connect `cognis` to common MCP clients after a
repository has already been initialized and indexed.

## Before configuring a client

Confirm these checks first:

- the repository contains `.cognis/uckg.db`
- `cognis-cli health` reports `overall: ok`
- you know the absolute path to `.cognis/uckg.db`
- you know whether your client should launch `cognis-mcpd` directly or through `python -m cognis_mcpd.main`

## Recommended path: VS Code or Cursor extension

If you use VS Code or Cursor, the extension can write the MCP configuration for
you:

1. install `cognis`
2. install the `cognis-vscode` extension
3. select the same Python interpreter used for the `cognis` install
4. open the target repository
5. run **Cognis: Set Up Workspace** (or **Troubleshoot & Repair** if the workspace was already configured)

The extension resolves absolute `COGNIS_DB_PATH`, `COGNIS_AUDIT_LOG`, and
`COGNIS_REPO_ROOT` values and writes the configuration for the selected host.
On Windows it also writes a safer default semantic timeout budget unless you
override those env vars explicitly.

## Manual configuration

### Generate the JSON block

You can ask `cognis` to generate the MCP configuration:

```bash
python -m cognis.cli.main mcp-config --host cursor --repo-root /path/to/repo
```

Use `--host vscode`, `--host cursor`, or `--host claude` as needed.

### Typical configuration paths

| Host | Typical configuration file |
| --- | --- |
| Cursor | `~/.cursor/mcp.json` |
| VS Code (generic) | `~/.vscode/mcp.json` |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` on Windows |

### Example: module form (Windows-friendly)

```json
{
  "mcpServers": {
    "cognis": {
      "command": "python",
      "args": ["-m", "cognis_mcpd.main"],
      "env": {
        "COGNIS_DB_PATH": "C:\\path\\to\\repo\\.cognis\\uckg.db",
        "COGNIS_AUDIT_LOG": "C:\\path\\to\\repo\\.cognis\\audit.log",
        "COGNIS_REPO_ROOT": "C:\\path\\to\\repo",
        "COGNIS_MCP_SOFT_TIMEOUT_S": "10",
        "COGNIS_MCP_HARD_TIMEOUT_S": "25",
        "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S": "12"
      }
    }
  }
}
```

## Claude Code and Claude Desktop

Claude Code and Claude Desktop both use the same JSON structure.

### macOS / Linux example

```json
{
  "mcpServers": {
    "cognis": {
      "command": "cognis-mcpd",
      "env": {
        "COGNIS_DB_PATH": "/absolute/path/to/your/repo/.cognis/uckg.db",
        "COGNIS_AUDIT_LOG": "/absolute/path/to/your/repo/.cognis/audit.log",
        "COGNIS_REPO_ROOT": "/absolute/path/to/your/repo"
      }
    }
  }
}
```

### Windows example

```json
{
  "mcpServers": {
    "cognis": {
      "command": "python",
      "args": ["-m", "cognis_mcpd.main"],
      "env": {
        "COGNIS_DB_PATH": "C:\\path\\to\\your\\repo\\.cognis\\uckg.db",
        "COGNIS_AUDIT_LOG": "C:\\path\\to\\your\\repo\\.cognis\\audit.log",
        "COGNIS_REPO_ROOT": "C:\\path\\to\\your\\repo",
        "COGNIS_MCP_SOFT_TIMEOUT_S": "10",
        "COGNIS_MCP_HARD_TIMEOUT_S": "25",
        "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S": "12"
      }
    }
  }
}
```

If `cognis` is installed inside a virtual environment, you can also point
`command` directly at that environment's `cognis-mcpd` executable.

## Cursor and generic VS Code MCP support

Use the same JSON block structure shown above. The main difference is where the
configuration file lives. If you already use the extension, prefer the editor
command instead of editing the file manually.

## Cline / Roo Cline

Create a new MCP server entry with:

- **Name**: `cognis`
- **Command**: `cognis-mcpd` or `python -m cognis_mcpd.main`
- **Environment variables**:
  - `COGNIS_DB_PATH`
  - `COGNIS_AUDIT_LOG` (recommended)
  - `COGNIS_REPO_ROOT` (recommended when you switch repos)

## Multiple repositories

If you need more than one indexed repository, add one server entry per
repository and give each entry a distinct name:

```json
{
  "mcpServers": {
    "cognis-frontend": {
      "command": "cognis-mcpd",
      "env": {
        "COGNIS_DB_PATH": "/repos/frontend/.cognis/uckg.db"
      }
    },
    "cognis-backend": {
      "command": "cognis-mcpd",
      "env": {
        "COGNIS_DB_PATH": "/repos/backend/.cognis/uckg.db"
      }
    }
  }
}
```

## Available tools

| Tool | Purpose |
| --- | --- |
| `diffuse_context(query, k?, alpha?, eps?, kind?, file_path?)` | **Flagship** CSAR spreading-activation retrieval; recovers full code flow in one call |
| `discover_symbols(query, k?, kind?, file_path?)` | Hybrid lexical + semantic discovery (RRF) in one call |
| `symbol_search(query, k?, kind?, file_path?)` | Lexical symbol discovery (names, qualified names, signatures) |
| `symbol_lookup(name_or_id, kind?)` | Resolve one symbol by id, qualified name, or exact/fuzzy match |
| `resolve_symbols(symbol_ids, include_body?)` | Batch hydrate up to 50 symbols after discovery |
| `semantic_search(query, k?, kind?, file_path?)` | Concept/intent search with enriched location + snippet payloads |
| `dependency_trace(symbol_id, direction, depth)` | Traverse callers or callees; hits include symbol metadata |
| `retrieve_context_capsule(task, max_tokens?, include_runtime?)` | Build a task-oriented context package in one call (CSAR-powered) |

## Choosing the right tool (for agents and humans)

Use the narrowest tool that answers the question. Fewer round trips and smaller
payloads keep agent sessions faster and cheaper.

| Goal | Preferred tool | Why |
| --- | --- | --- |
| Understand or trace a flow / find everything around a region | `diffuse_context` | CSAR diffuses over the call graph; recovers on-path symbols a single lookup misses, in one round trip |
| Find candidates when name or intent is unclear | `discover_symbols` | One call merges lexical + semantic evidence with fused ranking |
| Find candidates by partial name or keyword only | `symbol_search` | Fast lexical shortlist with ids, locations, and snippets |
| Resolve a known id or qualified name to one symbol | `symbol_lookup` | Exact resolution; avoid when you only have a vague name |
| Hydrate several discovered symbol ids at once | `resolve_symbols` | One batch call instead of repeated `symbol_lookup` |
| Explore by meaning when lexical discovery is insufficient | `semantic_search` | Embedding similarity with enriched symbol payloads |
| Walk callers or callees from a known symbol id | `dependency_trace` | Graph traversal; enriched hits reduce follow-up lookups |
| Start a bugfix, feature, or explain task with one call | `retrieve_context_capsule` | Planner composes symbols, call chain, and evidence (CSAR structural stage) |

Recommended agent workflow:

1. **Diffuse** with `diffuse_context("why does login time out", k=10)` to get the
   relevant region *and its call flow* in one call. Lower `alpha` (e.g. `0.1`)
   spreads farther along the graph; raise it (e.g. `0.4`) to stay near direct matches.
2. **Discover** with `discover_symbols("validate jwt", k=8)` when you just need a
   quick lexical/semantic shortlist (or `symbol_search` when the name is known).
3. **Hydrate** several finalists with `resolve_symbols([id1, id2])` when full records are needed.
4. **Skip straight to task context** with `retrieve_context_capsule(task=...)` when the task is already clear.

Avoid chaining `discover_symbols` + `dependency_trace` manually — `diffuse_context`
fuses discovery and flow traversal in one round trip. Avoid repeated `symbol_lookup`
for multiple ids — use `resolve_symbols` instead.

Semantic tools (`diffuse_context`, `discover_symbols`, `semantic_search`, and semantic
retrieval inside `retrieve_context_capsule`) share one process-wide embedder instance
after the first call. Identical search queries are cached in-process for 60 seconds
(configurable via `COGNIS_MCP_CACHE_TTL_S`) to avoid repeated embedder and SQL work.

## Verify the connection

After saving the MCP configuration:

1. reload the MCP host or restart the client
2. run a small query such as `symbol_lookup("main")`
3. if available, run:

```bash
cognis-cli mcp-conformance
```

## Security notes

- MCP tools are read-only by default.
- `cognis` writes an audit log to `.cognis/audit.log`.
- comments, docstrings, and other untrusted text are marked in returned capsules.

For the full security model, see [security.md](security.md).

## Troubleshooting

| Problem | What to check |
| --- | --- |
| `cognis-mcpd` cannot be found | Use the full executable path or `python -m cognis_mcpd.main` |
| `INDEX_NOT_READY` or missing symbols | Re-run `cognis-cli index --full .` in the target repository |
| Wrong repository answers | Confirm `COGNIS_DB_PATH` and `COGNIS_REPO_ROOT` point at the intended repository |
| Semantic search unavailable | Confirm the install included the embedding dependencies |
| Slow first query | The local embedder may still be loading into memory; on Windows prefer generated config or set the three `COGNIS_MCP_*TIMEOUT*` env vars shown above |
