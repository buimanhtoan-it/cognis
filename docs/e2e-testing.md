# End-to-end testing across apps

Cognis ships as several cooperating apps that talk over process boundaries:

```
cognis-vscode (TypeScript)
    │  spawns: python -m cognis.cli.main ...   (paths/init/bootstrap/index/health/mcp-config)
    │  spawns: python -m cognis_indexd.main ... (live indexing daemon)
    ▼
cognis-cli / cognis-indexd / cognis-mcpd (Python)
    │  share: a UCKG SQLite DB + a JSON status file + the MCP stdio protocol
    ▼
AI agent / MCP host
```

Unit and integration tests mock one side of each boundary. That is fast, but it
cannot catch a **cross-app mismatch** — e.g. the CLI renames a JSON field the
extension reads, the daemon changes its status phases, or the MCP server
deadlocks only when driven over real stdio. The E2E layer closes that gap.

## What the E2E layer covers

There are several complementary pieces.

### 1. Full-flow E2E (`tests/e2e/`, marker `e2e`)

Drives the **real** entrypoints as subprocesses, in the exact order the
extension's "Set Up Workspace" flow uses them:

1. `cognis-cli paths` → resolve workspace paths + entrypoints
2. `cognis-cli init` → materialize `.cognis/`
3. `cognis-cli mcp-config` → emit MCP client config
4. `cognis-indexd --full-rebuild` → cold-index, then watch
5. `cognis-cli health` → confirm the index is queryable
6. `cognis-mcpd` over **stdio** → an AI host calls a tool against the indexed DB

`tests/e2e/harness.py` provides `run_cli`, `run_cli_json`, and an
`IndexdProcess` context manager that launches the daemon, drains its stdout,
waits for a status phase, and tears it down cleanly.

### 2. Cross-language contract parity

`tests/e2e/test_contract_snapshots.py` captures the *real* JSON shapes emitted
by the CLI and the indexd status file into committed golden skeletons under
`tests/e2e/contracts/` (field names + value types, with machine-specific values
stripped). The matching TypeScript test
`apps/cognis-vscode/src/test/contractParity.test.ts` loads those same goldens
and asserts every field the extension's interfaces (`WorkspacePaths`,
`McpConfigPayload`, `HealthReport`, `BootstrapPayload`, `IndexStatusReport`)
actually read is present. A drift on either side fails a test.

### 3. MCP tool output contracts (the AI-facing surface)

`tests/e2e/test_mcp_tool_contracts.py` drives a **real** `cognis-mcpd` over HTTP
and asserts the JSON each of the 8 tools returns to the agent keeps the keys the
agent relies on — search/lookup/trace/resolve hits, hybrid `discover_symbols`,
the flagship `diffuse_context` (`on_path` / `ppr_score` / `match_sources`), the
`retrieve_context_capsule` schema, and the `{error:{code,message,retryable}}`
envelope. The live tool set is asserted against `cognis.contract.MCP_TOOLS`.

### 4. Contract-version handshake + lockstep

`cognis-cli handshake` advertises `{contract_version, engine_version,
cli_commands, mcp_tools}` from `cognis/contract.py`. The extension negotiates it
at startup and warns on version skew. A lockstep test asserts the backend
`CONTRACT_VERSION` equals the extension's `EXPECTED_CONTRACT_VERSION` (bump both
together); the TS `contract.test.ts` covers the skew decision matrix.

### 5. Resource-leak / memory guards

`tests/e2e/test_memory.py` runs the real `cognis-mcpd` (all 8 tools, plus
sustained lexical/semantic load) and the real `cognis-indexd` watcher under
repeated edits, asserting bounded OS-handle and RSS growth — the deterministic
fingerprint of a per-call connection/file leak. A near-linear handle climb fails.

### 6. Full-stack VS Code host e2e

`apps/cognis-vscode` `npm run test:host` (`src/test-host/`) is the only layer
that runs the real `extension.ts` inside a **real VS Code** (via
`@vscode/test-electron`) against the **real Python backend**. It runs
`cognis.setupWorkspace` and asserts the real `.cognis/config.yaml` + workspace
`mcp.json` are written and the flow appears in the diagnostics trace. Point it at
a backend python with `COGNIS_TEST_PYTHON`; on Linux run under `xvfb-run -a`. CI
job: `vscode-host-e2e`.

## Running

```bash
# Python full-flow + contract snapshots + MCP tool contracts + memory guards
make e2e                 # or: pytest -m e2e
# or via invoke
invoke e2e

# TypeScript unit + contract-parity + handshake + diagnostics tests
cd apps/cognis-vscode && npm test

# Full-stack: real VS Code host + real backend (needs COGNIS_TEST_PYTHON)
cd apps/cognis-vscode && npm run test:host        # Windows
#   Linux: COGNIS_TEST_PYTHON=python xvfb-run -a npm run test:host
```

The `e2e` marker is excluded from the default `make test` so push CI stays fast.
It runs cross-platform (Linux/macOS/Windows) in `pr-integration.yml`.

## Updating contracts after an intentional change

If you intentionally change a JSON shape (new field, renamed key), regenerate
the goldens and update the matching TypeScript interface:

```bash
COGNIS_UPDATE_CONTRACTS=1 pytest -m e2e -k contract_snapshots
```

Then update `types.ts` (and the expected key list in `contractParity.test.ts`)
so both sides move together. Reviewers see the golden diff in the PR, which
makes a cross-app contract change explicit and intentional.

## Notes / gotchas these tests already encode

- **The embedder must load on the main thread.** `cognis-mcpd` warms the shared
  semantic layer synchronously on the main thread before serving. Loading
  `sentence-transformers`/`torch` for the first time on a FastMCP worker thread
  hangs the server, so `semantic_search` would time out on first use. The E2E
  `test_semantic_search_over_stdio_does_not_hang` indexes with embeddings and
  asserts the tool returns over stdio (with a `fail_after` guard so a
  regression surfaces as a clear failure, not a stalled suite).
- **MCP stdio dispatch must not import heavy deps on a worker thread.**
  `cognis-mcpd` warms the `Database` (which imports `sqlite_vec`/`numpy`) on the
  main thread before serving; otherwise the first tool call's import on a
  FastMCP anyio worker thread can deadlock against the serve loop's import lock
  (Python 3.14 / Windows). See `cognis_mcpd.main._warm_db_on_startup`.
- **The status file is written atomically with a retry.** It is polled
  concurrently by the extension; on Windows `os.replace` can hit a transient
  sharing violation. `cognis_indexd.main._write_status_file` retries and never
  crashes the daemon over a status update.
- **Lexical vs semantic E2E.** Most stdio round-trips use lexical tools
  (`symbol_search`) with `--skip-embeddings` so they run fast without a model.
  The dedicated semantic regression test opts into the real embedder path and is
  skipped automatically when `sentence-transformers` is not installed.
