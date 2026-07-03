# End-to-end testing across apps

Cognis ships as several cooperating surfaces that talk over process boundaries.
The engine is one static Rust binary (busybox-style multi-call) plus the
TypeScript VS Code / Cursor extension:

```
cognis-vscode (TypeScript)
    │  spawns: cognis cli ...     (paths/init/bootstrap/index/health/mcp-config)
    │  spawns: cognis indexd ...  (live indexing daemon)
    ▼
cognis cli / cognis indexd / cognis mcpd  (one Rust binary, multi-call)
    │  share: a UCKG SQLite DB + a JSON status file + the MCP stdio protocol
    ▼
AI agent / MCP host
```

Unit tests inside each crate (and the extension's TS unit tests) exercise one
side of a boundary in isolation. That is fast, but it cannot catch a **cross-app
mismatch** — e.g. the CLI renames a JSON field the extension reads, the daemon
changes its status phases, or the MCP server deadlocks only when driven over
real stdio. The E2E layer closes that gap.

## What the E2E layer covers

There are several complementary pieces.

### 1. Full-flow E2E

Drives the **real** binary surfaces as subprocesses, in the exact order the
extension's "Set Up Workspace" flow uses them:

1. `cognis cli paths` → resolve workspace paths + entrypoints
2. `cognis cli init` → materialize `.cognis/`
3. `cognis cli mcp-config` → emit MCP client config
4. `cognis indexd --full-rebuild` → cold-index, then watch
5. `cognis cli health` → confirm the index is queryable
6. `cognis mcpd` over **stdio** → an AI host calls a tool against the indexed DB

### 2. Cross-language contract parity

The CLI's JSON shapes and the indexd status file are captured into committed
golden skeletons under `tests/e2e/contracts/` (field names + value types, with
machine-specific values stripped). The matching TypeScript test
`apps/cognis-vscode/src/test/contractParity.test.ts` loads those same goldens
and asserts every field the extension's interfaces (`WorkspacePaths`,
`McpConfigPayload`, `HealthReport`, `BootstrapPayload`, `IndexStatusReport`)
actually read is present. A drift on either side fails a test.

### 3. MCP tool output contracts (the AI-facing surface)

A **real** `cognis mcpd` is driven and the JSON each of the 8 tools returns to
the agent is asserted to keep the keys the agent relies on — search/lookup/
trace/resolve hits, hybrid `discover_symbols`, the flagship `diffuse_context`
(`on_path` / `ppr_score` / `match_sources`), the `retrieve_context_capsule`
schema, and the `{error:{code,message,retryable}}` envelope.

### 4. Contract-version handshake + lockstep

`cognis cli handshake` advertises `{contract_version, engine_version,
cli_commands, mcp_tools}`. The extension negotiates it at startup and warns on
version skew. A lockstep check asserts the backend contract version equals the
extension's `EXPECTED_CONTRACT_VERSION` (bump both together); the TS
`contract.test.ts` covers the skew decision matrix.

### 5. Resource-leak / memory guards

The real `cognis mcpd` (all 8 tools, plus sustained lexical/semantic load) and
the real `cognis indexd` watcher are run under repeated edits, asserting bounded
OS-handle and RSS growth — the deterministic fingerprint of a per-call
connection/file leak. A near-linear handle climb fails.

### 6. Full-stack VS Code host e2e

`apps/cognis-vscode` `npm run test:host` (`src/test-host/`) is the only layer
that runs the real `extension.ts` inside a **real VS Code** (via
`@vscode/test-electron`) against the **real `cognis` binary backend**. It runs
`cognis.setupWorkspace` and asserts the real `.cognis/config.yaml` + workspace
`mcp.json` are written and the flow appears in the diagnostics trace. On Linux
run under `xvfb-run -a`. CI job: `vscode-host-e2e`.

## Running

```bash
# Rust workspace tests (unit + property + parity + cross-process e2e)
cargo test --workspace

# TypeScript unit + contract-parity + handshake + diagnostics tests
cd apps/cognis-vscode && npm test

# Panel UI e2e (Playwright over the rendered webview)
cd apps/cognis-vscode && npm run test:e2e

# Full-stack: real VS Code host + real binary backend
cd apps/cognis-vscode && npm run test:host        # Windows
#   Linux: xvfb-run -a npm run test:host
```

## Updating contracts after an intentional change

If you intentionally change a JSON shape (new field, renamed key), regenerate
the goldens under `tests/e2e/contracts/` and update the matching TypeScript
interface (`types.ts`, and the expected key list in `contractParity.test.ts`) so
both sides move together. Reviewers see the golden diff in the PR, which makes a
cross-app contract change explicit and intentional.

## Notes / gotchas these tests already encode

- **The embedder loads before serving.** `cognis mcpd` warms the shared semantic
  layer before it starts serving so the first `semantic_search` call does not
  race model load on a request path.
- **The status file is written atomically with a retry.** It is polled
  concurrently by the extension; on Windows an atomic replace can hit a transient
  sharing violation, so the daemon retries and never crashes over a status update.
- **Lexical vs semantic E2E.** Most stdio round-trips use lexical tools
  (`symbol_search`) with `--skip-embeddings` so they run fast without a model.
  The semantic regression path opts into the real embedder and is skipped
  automatically when the ONNX model assets are absent.
