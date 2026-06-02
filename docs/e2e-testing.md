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

There are two complementary pieces.

### 1. Full-flow E2E (`tests/e2e/`, marker `e2e`)

Drives the **real** entrypoints as subprocesses, in the exact order the
extension's "Set Up for AI" flow uses them:

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

## Running

```bash
# Python full-flow + contract snapshots (needs the indexer + mcp extras)
make e2e                 # or: pytest -m e2e
# or via invoke
invoke e2e

# TypeScript unit + contract-parity tests
cd apps/cognis-vscode && npm test
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
