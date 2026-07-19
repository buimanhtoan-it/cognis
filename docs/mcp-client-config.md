# MCP Client Configuration

This guide explains how to connect `cognis` to common MCP clients after a
repository has already been initialized and indexed. It also covers the
workspace-scope default, safe global→workspace migration and rollback, eager
vs lazy semantic startup, multi-root / multi-host lifecycle, and the thin-proxy
topology used to avoid host × repository process fan-out.

## Before configuring a client

Confirm these checks first:

- the repository contains `.cognis/uckg.db`
- `cognis health` reports `overall: ok`
- you know the absolute path to `.cognis/uckg.db`
- you know the absolute path to your `cognis` binary (the MCP server is the
  binary's `mcpd` surface — invoked as `cognis mcpd`, or by the binary path with
  an `mcpd` argument)

## Recommended path: VS Code or Cursor extension

If you use VS Code or Cursor, the extension can write the MCP configuration for
you:

1. install the `.vsix` from the Polar ZIP, or build it from source
2. click **Install engine** in the Cognis panel, or set `cognis.binaryPath` to a source-built binary
3. open the target repository
4. run **Cognis: Set Up Workspace** (or **Troubleshoot & Repair** if the workspace was already configured)

The extension resolves absolute `COGNIS_DB_PATH`, `COGNIS_AUDIT_LOG`, and
`COGNIS_REPO_ROOT` values and writes the configuration for the selected host,
pointing the server command at the managed `cognis` binary's `mcpd` surface.
On Windows it also writes a safer default semantic timeout budget unless you
override those env vars explicitly.

### Config scope (workspace default)

| Setting | Values | Default |
| --- | --- | --- |
| `cognis.mcpConfigScope` | `workspace`, `global` | **`workspace`** |

- **`workspace` (default):** writes the host's MCP file *inside the repository*
  (for example `.cursor/mcp.json`, `.vscode/mcp.json`, or
  `.kiro/settings/mcp.json`). Each host starts only the open repo's Cognis
  server — this is the path that prevents host × repository idle `mcpd` fan-out.
- **`global` (explicit opt-in):** merges a `cognis-<slug>` entry into the
  user-level host config (`~/.cursor/mcp.json`, `~/.vscode/mcp.json`, …). Every
  MCP host that loads that global file then starts a daemon for **every**
  indexed repo still listed there. Global scope remains supported with clear
  fan-out warnings and is **never** silently migrated or deleted.

Prefer workspace scope unless you deliberately need the same host to keep many
repos connected without opening them.

### Stdio topology and sharing gate

| Setting / env | Default | Meaning |
| --- | --- | --- |
| `cognis.mcpStdioMode` / `COGNIS_MCP_STDIO_MODE` | **`proxy`** | Editor-facing stdio process is a **model-free thin proxy** that forwards JSON-RPC to one heavy repository daemon. Host × repository connections cost a thin process, not a full ONNX/DB process. Set `heavy` (or env `heavy` / `legacy`) to restore one heavy process per editor connection. |
| `cognis.mcpSharedHttpEnabled` / `COGNIS_MCP_SHARED_HTTP` | **OFF (`false`)** | Reversible gate for shared loopback HTTP. Shared HTTP is still withheld until every required evidence check passes; a failed gate keeps the thin-proxy / per-repository stdio path with **no data loss**. |

Default production topology: **workspace scope + thin stdio proxy + sharing gate
OFF**. Do not enable shared HTTP in user configs unless you have gate evidence
and understand the isolation model in [security.md](security.md).


## Manual configuration

### Generate the JSON block

You can ask `cognis` to generate the MCP configuration:

```bash
cognis cli mcp-config --host cursor --repo-root /path/to/repo
```

Use `--host vscode`, `--host cursor`, or `--host claude` as needed.

### Typical configuration paths

| Host | Typical configuration file |
| --- | --- |
| Cursor | `~/.cursor/mcp.json` |
| VS Code (generic) | `~/.vscode/mcp.json` |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` on Windows |

### Example: binary path + `mcpd` surface (portable)

```json
{
  "mcpServers": {
    "cognis": {
      "command": "C:\\tools\\cognis.exe",
      "args": ["mcpd"],
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

If `cognis` is on your `PATH`, you can use `"command": "cognis"` with
`"args": ["mcpd"]`. A binary installed under the legacy name `cognis-mcpd` can be
launched directly with no `args`.

## Claude Code and Claude Desktop

Claude Code and Claude Desktop both use the same JSON structure.

### macOS / Linux example

```json
{
  "mcpServers": {
    "cognis": {
      "command": "cognis",
      "args": ["mcpd"],
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
      "command": "C:\\tools\\cognis.exe",
      "args": ["mcpd"],
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

Use the absolute path to the `cognis` binary if it is not on the client's
`PATH`.

## Cursor and generic VS Code MCP support

Use the same JSON block structure shown above. The main difference is where the
configuration file lives. If you already use the extension, prefer the editor
command instead of editing the file manually.

## Cline / Roo Cline

Create a new MCP server entry with:

- **Name**: `cognis`
- **Command**: `cognis` (with argument `mcpd`), or the absolute binary path
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
      "command": "cognis",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "/repos/frontend/.cognis/uckg.db"
      }
    },
    "cognis-backend": {
      "command": "cognis",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "/repos/backend/.cognis/uckg.db"
      }
    }
  }
}
```

With the extension and **workspace** scope, prefer one workspace `mcp.json` per
repository rather than listing every repo in a single global file. Listing many
`cognis-*` entries in a global host config is the historical source of idle
`mcpd` multiplication (host × repository).

## Eager vs lazy semantic startup

Semantic tools (`semantic_search`, hybrid legs of `discover_symbols` /
`diffuse_context` / capsules) need a local ONNX embedder. Cognis now honors a
single warm policy env that the extension writes into generated MCP config.

| Source | Value | Engine behavior |
| --- | --- | --- |
| `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=1` | Eager | Build/map the embedder at process open (before semantic readiness) |
| `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0` | Lazy | **Zero** ONNX session resident until first semantic demand; concurrent first demand is single-flight |
| Variable **absent** | Eager | Legacy / direct-launch compatibility (`cognis mcpd` with no extension env) |
| Invalid / empty / whitespace | Eager (+ warning on stderr) | Safe fallback; never leave tools waiting on an unexpected deferral |

**Extension setting:** `cognis.mcpWarmSemanticOnStartup` (boolean, default
`false`). Extension-generated config always sets the policy explicitly: `false`
emits `=0` (lazy), while `true` emits `=1` (eager). An absent variable remains
eager only for legacy/direct engine launches. The Rust engine
(`SemanticWarmPolicy` in `cognis-core`) is the sole consumer of the env var —
lexical, structural, lookup, trace, resolve, and other non-semantic tools do
**not** wait on ONNX initialization.

**Operator guidance:**

- Prefer **lazy (`0`)** when you want lower idle private bytes and can tolerate a
  slower first semantic call (single-flight load + optional failure cooldown).
- Prefer **eager (`1`)** when first-query latency must stay within editor
  timeouts (especially Windows cold model load).
- Do not set free-text values (`true`/`false`/`yes`); only `"1"` and `"0"` are
  accepted.

Idle eviction (primarily `indexd`) may release a mapped session after a
configurable idle interval with no in-flight work; reload reuses the same
single-flight path. Semantic results for the same repo/DB/model fingerprint and
query stay equivalent once the model is loaded, independent of eager vs lazy.

## Multi-root and multi-host lifecycle

Cognis treats ownership by **canonical repository identity** (symlink- and
case-resolved absolute repo root + `COGNIS_DB_PATH`), not by window title or raw
path string.

| Scenario | Expected behavior |
| --- | --- |
| Multi-root workspace | Each root plans MCP/index ownership independently; heavy owners are deduped per canonical identity |
| Same repo open in Cursor + VS Code (or two windows) | At most one heavy repository daemon per canonical repo; additional hosts attach via thin stdio proxy (default) or a gate-verified shared loopback HTTP route |
| Extension reload / crash | Cross-process lease files under `.cognis/` (`mcpd.lease`, `indexd.lease`) carry owner nonce, PID + process-start identity, and heartbeat; the next owner attaches or reclaims only when the previous owner is confirmed gone |
| Stop / disconnect | Reference-aware graceful shutdown: last client release stops the heavy daemon; cleanup is idempotent and never kills an unrelated or PID-reused process |

Cardinality targets used in measurement (see
[private-bytes measurement](../tests/e2e/private-bytes/README.md)):

- `A` = active canonical repositories
- `H` = active MCP client connections
- `I` = actively indexing repositories
- heavy daemons ≤ `A`, indexd ≤ 1 per repo and ≤ `I`, thin proxies ≤ `H` and model-free

Do not put every indexed repository into a **global** MCP config unless you
explicitly accept host × repository process cost.

## Migrating global Cognis entries to workspace scope

If an older setup wrote Cognis into a **global** host file
(`~/.cursor/mcp.json`, `~/.vscode/mcp.json`, …), move each repository's entry to
the matching **workspace** file so hosts no longer start every listed repo.

### What the extension does

`migrateGlobalEntryToWorkspace` (extension module `mcpConfigMigrate.ts`):

1. **Plans** the move (source path, destination path, Cognis server names that
   match the repository env) — dry-run returns this plan without writing.
2. **Locks** the source + destination pair in-process so concurrent runs serialize.
3. Writes **timestamped, byte-preserving backups** of every touched file.
4. **Parses and validates** both configs; merges **only** the matched Cognis
   entry into the workspace file.
5. Writes the destination **atomically** (temp + fsync + rename).
6. **Verifies** the destination is host-visible and `COGNIS_DB_PATH` /
   repository env match the target repo.
7. **Removes the source entry only after verification.**
8. Preserves every **non-Cognis** key and server byte-for-byte (or semantically
   exact).
9. On any failure, **restores all touched files from backup** and retains the
   backups so an interrupted move stays recoverable. Successful runs drop
   backups unless retention is requested.

The operation is **restartable and idempotent**: re-running after a clean
success is a no-op; re-running after rollback retries the same plan.

### Operator / admin steps

1. Prefer the extension: open the repo, set `cognis.mcpConfigScope` to
   `workspace`, run **Cognis: Connect MCP** or **Troubleshoot & Repair**. The
   migration path moves matching global Cognis entries when present.
2. Confirm the workspace MCP file contains the `cognis-<slug>` (or env-matched)
   entry and that the global file no longer lists that entry.
3. Reload the MCP host / editor so it re-reads config.
4. Run a small tool call (`symbol_lookup`) and `cognis health` in the repo.

### Manual recovery / rollback

If migration was interrupted or health/compatibility checks fail:

1. **Do not** delete unrelated user servers or whole `mcp.json` files.
2. Locate timestamped backups next to the touched paths (same directory,
   timestamp suffix from the migration audit trail in the extension log).
3. Restore both source and destination from those backups (byte copy over the
   current files), or re-run migration after the tree is consistent — the
   routine restores from its retained backups when a step fails.
4. Prefer **safe non-destruction**: if ownership of a live PID is ambiguous,
   leave the process alone and use **Cognis: Troubleshoot & Repair** / lease
   reclaim rather than a blind `taskkill`.
5. Non-Cognis MCP servers must remain intact through enable, migrate, disable,
   and rollback. If they do not, restore from backup and file a bug with the
   diagnostics log.

Explicit **global** scope is never force-migrated. Users who keep
`cognis.mcpConfigScope = global` retain that mode with fan-out warnings.

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
after the first successful load (eager at open, or lazy on first demand — see
[Eager vs lazy semantic startup](#eager-vs-lazy-semantic-startup)). Identical search
queries are cached in-process for 60 seconds (configurable via
`COGNIS_MCP_CACHE_TTL_S`) to avoid repeated embedder and SQL work.


## Verify the connection

After saving the MCP configuration:

1. reload the MCP host or restart the client
2. run a small query such as `symbol_lookup("main")`
3. if available, run:

```bash
cognis cli mcp-conformance
```

## Security notes

- MCP tools are read-only by default.
- `cognis` writes an audit log to `.cognis/audit.log`.
- comments, docstrings, and other untrusted text are marked in returned capsules.
- Shared HTTP (when the gate is eventually enabled) binds **loopback-only** by
  default, uses scoped credentials, verifies repository/DB identity per
  attachment, and refuses model-session reuse across differing model fingerprints
  — see [security.md](security.md).
- Prefer workspace-scoped config and the thin-proxy stdio path; do not expose
  `mcpd --transport http` on non-loopback interfaces.

For the full security model, see [security.md](security.md).

## Process / private-byte measurement

Resource claims (process counts, idle private bytes, active peak) are
**empirical** for a named machine, build, model, and topology. The acceptance
procedure and script live under
[`tests/e2e/private-bytes/`](../tests/e2e/private-bytes/README.md). The recorded
defect snapshot (~1.23 GiB idle aggregate on one Windows multi-process topology)
and the median target (≤ 0.615 GiB on an equivalent stabilized-idle reproduction)
are **targets and baselines**, not universal or automatically achieved results.
Always label published numbers with hardware, OS, build, model fingerprint,
topology (`A`/`H`/`I`), and run count.

## Troubleshooting

| Problem | What to check |
| --- | --- |
| `cognis` cannot be found | Use the full path to the binary, or put it on `PATH` |
| `INDEX_NOT_READY` or missing symbols | Re-run `cognis index --full .` in the target repository |
| Wrong repository answers | Confirm `COGNIS_DB_PATH` and `COGNIS_REPO_ROOT` point at the intended repository |
| Semantic search unavailable | Confirm the index was built with embeddings (not `--skip-embeddings`) |
| Slow first query | Under lazy policy the embedder loads on first demand; under eager it loads at start. On Windows prefer generated config or set the three `COGNIS_MCP_*TIMEOUT*` env vars shown above |
| Many idle `cognis` / `mcpd` processes | Prefer `cognis.mcpConfigScope=workspace` and `cognis.mcpStdioMode=proxy`; migrate global Cognis entries out of `~/.cursor/mcp.json` / `~/.vscode/mcp.json`; avoid listing every repo in a global host file |
| Shared HTTP did not switch on | Sharing gate defaults OFF; even when `cognis.mcpSharedHttpEnabled=true`, every evidence check must pass or Cognis keeps thin-proxy stdio (no data loss) |
| Migration left config inconsistent | Restore from timestamped migration backups or re-run repair; never wipe unrelated MCP servers |
