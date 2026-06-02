# Release Notes — cognis v0.2.0

> **Reliability release.** Fixes the cross-app failures that made semantic
> search and live indexing appear broken in real editor sessions, and adds a
> cross-app end-to-end test layer so those classes of bug can't regress
> silently.

## Highlights

### Semantic search no longer hangs on first use (the big one)

On a fresh workspace indexed with embeddings, the first `semantic_search` call
(and the semantic stage of `retrieve_context_capsule`) over MCP stdio would
hang until the MCP deadline fired and return a `TIMEOUT` — so an AI agent saw
"indexing doesn't work" even though the index was fully populated.

**Root cause:** the embedder (`sentence-transformers` / `torch`) was loaded for
the first time on a spawned worker thread inside the server. First-time `torch`
initialization off the main thread hangs in the MCP server process. The
previous background warm-up thread did not help because it, too, was not the
main thread.

**Fix:** `cognis-mcpd` now warms the shared semantic layer **synchronously on
the main thread** before serving. Every subsequent tool call reuses the cached
singleton and returns immediately. This adds a one-time startup cost (model
load) before the server accepts connections; disable with
`COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0` if you only use lexical/structural
tools (note: doing so re-exposes the off-main-thread first-load hang for
semantic tools).

### MCP tool calls no longer deadlock on first invocation

The first MCP tool call could deadlock on the CPython import lock when it
triggered the `sqlite_vec`/`numpy` import (via the DB probe) on a FastMCP anyio
worker thread while the main thread sat in the stdio serve loop (observed on
Python 3.14 / Windows). `cognis-mcpd` now warms the UCKG `Database` on the main
thread before serving, so the heavy import happens once, up front.

### `cognis-indexd` is robust on Windows and on shutdown

- The status file (`.cognis/indexd-status.json`), which IDE integrations poll on
  a timer, is now written atomically with a retry. This fixes a Windows
  `os.replace` sharing-violation crash (`WinError 5/32`) when a reader holds the
  destination open during the swap. A status update can never take down the
  daemon.
- The daemon runs all pipeline DB work on a dedicated single-thread writer
  executor and releases its SQLite connections deterministically on shutdown,
  fixing a connection leak on long-lived hosts.

## Added — cross-app end-to-end testing

cognis ships as several cooperating apps (`cognis-vscode`, `cognis-cli`,
`cognis-indexd`, `cognis-mcpd`) that talk over process boundaries. Unit and
integration tests mock one side of each boundary, so a drift in the JSON shapes
or a stdio-only failure slips through. v0.2.0 adds:

- **`tests/e2e/` (marker `e2e`)** — drives the real `cognis-cli`,
  `cognis-indexd`, and `cognis-mcpd` as subprocesses in the exact order the
  extension's "Set Up for AI" flow uses them (paths → init → mcp-config →
  indexd `--full-rebuild` → health → MCP stdio query). Includes a regression
  test that indexes with embeddings and asserts `semantic_search` returns over
  stdio instead of hanging.
- **Contract snapshots** — committed golden skeletons of the real CLI / status
  JSON, verified on the TypeScript side by
  `apps/cognis-vscode/src/test/contractParity.test.ts` against the extension's
  interfaces (`WorkspacePaths`, `McpConfigPayload`, `HealthReport`,
  `BootstrapPayload`, `IndexStatusReport`). A field rename on either side fails
  a test.

Run with `make e2e` (or `invoke e2e`) and `npm test` in `apps/cognis-vscode`.
See [e2e-testing.md](e2e-testing.md).

## Carried forward from v0.1.x

CSAR (Code Spreading-Activation Retrieval) remains the primary retrieval engine,
with the flagship `diffuse_context` MCP tool. The full 8-tool MCP surface,
three-language indexing (TypeScript / Python / Go), the Context Capsule v1
schema, secret redaction, and single-file SQLite storage are unchanged. See
[release-notes-v0.1.17.md](release-notes-v0.1.17.md) for the feature baseline
and [docs/csar.md](csar.md) for the CSAR math and proofs.

## Getting started

```bash
pip install cognis[indexer,embed-local,tokenizers,mcp]
cd /your/repo
cognis-cli init
cognis-cli index --full .
cognis-cli health
cognis-mcpd  # or configure via docs/mcp-client-config.md
```

## Changelog

See [CHANGELOG.md](../CHANGELOG.md) for the full change history.

## License

Apache-2.0. See `LICENSE`.
