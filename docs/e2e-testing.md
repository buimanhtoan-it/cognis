# End-to-End & Contract Testing

Cognis ships as cooperating Rust and TypeScript surfaces communicating across process boundaries:

```text
cognis-mcp (TypeScript: Panel, Guardrail CodeLens & Diagnostics)
    │  spawns: cognis cli ...     (paths / init / index / health / mcp-config / handshake)
    │  spawns: cognis indexd ...  (incremental file watcher)
    │  calls:  cognis mcpd ...    (analyze_patch_impact, Try MCP tools, HTTP/stdio)
    ▼
cognis (Single Multi-Call Rust Binary: cli · indexd · mcpd · owner)
    │  shares: .cognis/uckg.db (SQLite WAL) + indexd-status.json + MCP JSON-RPC 2.0
    ▼
AI Coding Agent / MCP Host (Antigravity, Cursor, VS Code, Claude Code, Kiro)
```

Our test architecture verifies both isolated crate invariants and cross-process contracts.

## Test Layers

### 1. Rust Unit, Property (`proptest`) & Live Guardrail E2E Tests

- **Complexity Semiring & `OPT-001..007` (`cognis-core`, `cognis-indexer`):** Verifies lattice algebra, multi-language tree-sitter complexity extraction, Shadow AST diffing, and optimization detection.
- **CSAR & CSIA Mathematical Properties (`cognis-csar`):** Machine-checks PPR mass conservation, geometric convergence, forward-push work bounds $O(1/(\alpha\varepsilon))$, and loop-amplified ($\beta^\ell = 4^\ell$) reverse-push blast radius.
- **Live Agent Guardrail E2E (`crates/cognis-mcp/tests/live_agent_guardrail_e2e.rs`):** Drives real indexing + `McpServer` JSON-RPC over realistic multi-file repositories, verifying `analyze_patch_impact`, `inspect_symbol_complexity`, `trace_blast_radius`, `suggest_optimal_refactor`, and `cognis://live/guardrails` end-to-end.

### 2. Cross-Language JSON Contract Parity (`tests/e2e/contracts/`)

Committed golden skeletons in `tests/e2e/contracts/` pin the exact JSON shapes emitted by `cognis cli` (`paths`, `health`, `bootstrap`, `mcp-config`, `handshake`) and `indexd-status.json`. `apps/cognis-mcp/src/test/contractParity.test.ts` asserts that every field read by the TypeScript extension matches the Rust contract, and `mcpContractRecovery.test.ts` verifies lockstep across all **12 MCP tools**.

### 3. Extension Guardrail & Lifecycle Suite (`apps/cognis-mcp`)

460+ Mocha tests verify:

- `GuardrailController` parsing of canonical `GuardrailCapsule` payloads, live Big-O CodeLens rendering, `OPT-001..007` & `CSIA` `vscode.Diagnostic` mapping, and `cognis.guardrails.*` configuration toggles (`guardrails.test.ts`).
- Fresh workspace setup ordering (`init` -> `index --full .` -> `indexd` -> `mcp.json`), MCP migration/rollback safety, binary checksum verification, and panel state machines.

### 4. Full-Stack VS Code Host E2E

`npm run test:host` (and `npm run test:host:large`) launches a real VS Code instance via `@vscode/test-electron` against the compiled `cognis` binary to validate end-to-end workspace bootstrap and MCP querying without mocks.

## Running the Test Suites

```bash
# Rust formatting, strict Clippy lints, and full workspace tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# VS Code / Cursor extension lint + unit + contract-parity suite
cd apps/cognis-mcp
npm run lint
npm test

# Optional: Playwright webview UI e2e & real VS Code Electron host e2e
npm run test:e2e
npm run test:host
```

## Updating Cross-App Contracts

Whenever a CLI or MCP JSON schema changes intentionally:

1. Update the Rust struct and regenerate or update `tests/e2e/contracts/*.json`.
2. Bump `CONTRACT_VERSION` (`crates/cognis-core/src/contract.rs`) and `EXPECTED_CONTRACT_VERSION` (`apps/cognis-mcp/src/contract.ts`) together.
3. Update the matching TypeScript parser/interface and unit tests under `apps/cognis-mcp/src/test/`.
