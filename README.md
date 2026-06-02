<div align="center">

![Cognis logo](assets/logo.png)

# cognis

</div>

> **Software Cognition Engine** for MCP clients and coding agents.
>
> **Status: v0.2.0 beta** — see [CHANGELOG.md](CHANGELOG.md) and [docs/release-notes-v0.2.0.md](docs/release-notes-v0.2.0.md).

`cognis` is a local indexing and retrieval system for source code. It builds a
workspace database from your repository and exposes structured queries such as
symbol lookup, semantic search, dependency tracing, and task-oriented context
retrieval to MCP-compatible tools.

## What makes it different: CSAR

Most AI code tools (Cursor, Cody, and earlier versions of cognis) rank code by
**embedding KNN + BM25**, scoring each symbol independently. That misses the
*flow* of code: a function on the call path between two relevant symbols is
invisible if it has no direct keyword or embedding match.

cognis is built around **CSAR — Code Spreading-Activation Retrieval**. CSAR
seeds a relevance distribution from cheap lexical + semantic matches, then
**diffuses** it across the code knowledge graph using Personalized PageRank
(random walk with restart). Results:

- **Recovers full flow.** On-path callers/callees surface even with zero direct
  match — solving the "missing structure" failure of pure embedding search.
- **Repo-size-independent cost.** The forward-push solver has provable work
  bound `1/(α·ε)`, *independent of repository size* — so improving recall does
  not mean more greping/embedding as the codebase grows.
- **One tunable operator.** A single parameter `α` provably interpolates between
  pure semantic (`α→1`) and pure structural (`α→0`) retrieval.

CSAR is grounded in five theorems (existence/uniqueness, geometric convergence,
mass conservation, endpoint limits, and the forward-push cost bound), each
verified in code by unit and property-based tests. See
[docs/csar.md](docs/csar.md) for the math and proofs.

The flagship MCP tool is **`diffuse_context`**, which returns the unified ranked
shortlist in a single round trip — replacing separate `discover_symbols` +
`dependency_trace` calls.

## What it does

`cognis` is useful when file-level search is not enough. Instead of returning
only raw files, it stores code structure and retrieval metadata so clients can
request focused context about symbols, relationships, and likely problem areas.

## Repo layout

```
cognis/
├── apps/
│   ├── cognis-cli/        # Click-based CLI: init, index, eval, health, up/down
│   ├── cognis-mcpd/       # FastMCP server (stdio at MVP, SSE in Phase 2)
│   ├── cognis-indexd/     # Indexer daemon: watcher → parser → enricher → embedder → writer
│   └── cognis-vscode/     # VS Code / Cursor extension
├── packages/
│   ├── core/              # data model, planner, capsule composer, schemas
│   ├── retrieval/         # CSAR diffusion + lexical/semantic/structural layers
│   ├── indexer/           # parsers, resolvers, enrichers, embedders, writer
│   ├── adapters/          # git, lsp, otel (phase 3)
│   └── eval/              # golden-set runner, metrics, reports
├── tests/
│   ├── unit/              # fast, in-process
│   ├── integration/       # cross-process, fixture-repo
│   ├── pbt/               # hypothesis property-based tests (CP-1..CP-12)
│   ├── eval/              # slow nightly eval
│   └── fixtures/repos/    # mini-ts-app, mini-py-svc, mini-go-svc
├── docs/
└── .cognis/               # gitignored runtime dir (created by `cognis-cli init`)
```

## Current scope

| Area | Status |
| --- | --- |
| Indexer (TS / Python / Go) | Implemented |
| **CSAR diffusion retrieval (Personalized PageRank)** | **Implemented — primary engine** |
| Retrieval (lexical, semantic, structural) | Implemented (CSAR seed/fallback layers) |
| MCP server (8 tools, stdio) | Implemented |
| CLI: `init`, `bootstrap`, `paths`, `mcp-config`, `index`, `eval`, `health`, `mcp-conformance` | Implemented |
| VS Code / Cursor extension | Implemented (`apps/cognis-vscode`) |
| CLI: `up`, `down` | Docker Compose wrappers (`deploy/compose.yaml`) |
| CLI: `profile` | Stub — use `make bench` for latency tests |
| LSP resolver | Detection only; heuristic fallback for edges |
| PyPI publish | Not yet — install from source |

Full release notes: [docs/release-notes-v0.2.0.md](docs/release-notes-v0.2.0.md).

## Quick start

**Requirements:** Python ≥ 3.11 and Git. That's it.

### Step 1 — Install (all platforms)

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
python -m venv .venv
```

Activate the virtual environment:

| Platform | Command |
| --- | --- |
| macOS / Linux | `source .venv/bin/activate` |
| Windows PowerShell | `.\.venv\Scripts\Activate.ps1` |

Install the backend (one command):

```bash
python -m pip install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
```

### Step 2 — Pick how you want to use it

**Option A · Editor (VS Code / Cursor) — recommended**

```bash
python scripts/setup_extension.py --package
```

Then in your editor: install the generated `.vsix`, select the **same Python
interpreter** you used above, open your project, and run the command
**Cognis: Set Up for AI**. The extension writes the MCP config and starts
indexing for you. Reload the editor if the tools don't appear right away.

**Option B · CLI / terminal**

```bash
cd /path/to/your/project
cognis-cli bootstrap .      # init + index + health, in one command
cognis-mcpd                 # start the MCP server (stdio)
```

That's it — your repo is indexed and the MCP tools are live. Point any
MCP-compatible client at `cognis-mcpd` (see
[docs/mcp-client-config.md](docs/mcp-client-config.md)).

> If `cognis-cli` / `cognis-mcpd` aren't on your `PATH`, use the module form:
> `python -m cognis.cli.main bootstrap .` and `python -m cognis_mcpd.main`.

### Re-index from scratch

Wiped state or stale index? Reset and rebuild while keeping your config:

```bash
cognis-cli index --clear .
```

In the editor, use the **Clear & Re-index** button in the Cognis panel.

### Next steps

- [Getting started](docs/getting-started.md) — fresh machine → working editor setup
- [Quickstart](docs/quickstart.md) — CLI-focused walkthrough and first query
- Contributor setup: `make install-dev` (or `.\scripts\setup-dev.ps1` /
  `./scripts/setup-dev.sh` / `invoke install-dev`)

## Development workflow

| Command | What it runs |
| --- | --- |
| `make lint` | `ruff format --check` + `ruff check` |
| `make typecheck` | `mypy` (strict on `packages/core`) |
| `make test` | `pytest` unit + property tests |
| `make bench` | `pytest --benchmark-only` |
| `make eval` | golden-set runner (`cognis-cli eval`) |

`tasks.py` exposes the same recipes for environments without `make` (Windows in particular):

```bash
invoke lint typecheck test
```

## Platform notes

- Python >= 3.11.
- Tree-sitter grammars are vendored or downloaded as part of the dev bootstrap; CI caches them.

### `sqlite-vec` extension

cognis uses [`sqlite-vec`](https://github.com/asg017/sqlite-vec) for the
semantic retrieval layer (KNN over `symbol_vec`). The Python wheel pulls
in a prebuilt native extension; installation is one command on all three
supported platforms:

```bash
pip install cognis[vector]
# or directly:
pip install sqlite-vec
```

| Platform | Notes |
| --- | --- |
| **Linux** (x86_64, aarch64) | Prebuilt wheel ships with the extension `.so`. Requires `glibc >= 2.17`. No additional steps. |
| **macOS** (x86_64, arm64) | Prebuilt wheel ships with the extension `.dylib`. Requires macOS 11+. No additional steps. |
| **Windows** (x86_64) | Prebuilt wheel ships with `vec0.dll`. Python must be built with extension-loading enabled (the official python.org installer is). If you use a stripped Python build (some corporate distributions), install a stock CPython and retry. |

When the extension cannot be loaded for any reason, cognis falls back to a
plain `symbol_vec(symbol_id PK, embedding BLOB)` table. The indexer still
writes embeddings; only KNN queries are unavailable until the extension is
restored. `cognis-cli health` reports the active backend.

To verify the extension is loaded:

```bash
python -c "import sqlite3, sqlite_vec; c=sqlite3.connect(':memory:'); c.enable_load_extension(True); sqlite_vec.load(c); print(c.execute('select vec_version()').fetchone())"
```

A successful run prints something like `('v0.1.6',)`.

## Self-hosted deployment

For a Docker Compose deployment:

```bash
export WORKSPACE_HOST_PATH=/path/to/your/codebase
docker compose -f deploy/compose.yaml up -d
```

See [docs/operations.md](docs/operations.md) for init, indexing, health, and upgrades.

## Security model in one screen

- Every comment / docstring / PR body is treated as **untrusted** and tagged before reaching the LLM.
- Secret-shaped strings (API keys, JWTs, PEM headers, `password=`) are scrubbed *before* indexing — originals are never persisted.
- MCP tools have hard caps on depth, k, wall time, and concurrent requests; every call is logged to `.cognis/audit.log` with hashed args.

Full threat model: [docs/security.md](docs/security.md).

## License

Apache-2.0. See [`LICENSE`](LICENSE).

## Project links

- Repository: [github.com/buimanhtoan-it/cognis](https://github.com/buimanhtoan-it/cognis)
- CSAR method (math + proofs): [docs/csar.md](docs/csar.md)
- Operations: [docs/operations.md](docs/operations.md)
- Architecture: [docs/architecture.md](docs/architecture.md)
- Getting started: [docs/getting-started.md](docs/getting-started.md)
- Quickstart: [docs/quickstart.md](docs/quickstart.md)
- Install guide: [docs/install.md](docs/install.md)
- MCP client setup: [docs/mcp-client-config.md](docs/mcp-client-config.md)
- Changelog: [CHANGELOG.md](CHANGELOG.md)
