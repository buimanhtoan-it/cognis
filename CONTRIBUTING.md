# Contributing to Cognis

Thank you for contributing to `cognis`. The engine is a Cargo workspace written in Rust, paired with a TypeScript VS Code / Cursor extension under `apps/cognis-mcp`.

## Development setup

You need the [Rust toolchain](https://rustup.rs) (stable) and Git. Node.js 20+ is required when working on the VS Code / Cursor extension.

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
cargo build --workspace
```

On Windows or Linux/macOS, the one-step helper builds the workspace and packages the extension:

```powershell
.\scripts\setup-dev.ps1   # Windows
./scripts/setup-dev.sh    # macOS / Linux
```

### Workspace layout

- `crates/*` — engine libraries:
  - `cognis-core` — domain models (`Symbol`, `Edge`), complexity semiring lattice, guardrail schemas, configuration, and contract constants
  - `cognis-store` — bundled SQLite (`rusqlite`), FTS5, `sqlite-vec` / BLOB vector store, and CSR graph builder
  - `cognis-embed` — `Embedder` / `Reranker` traits and local ONNX backend (`bge-small-en-v1.5`)
  - `cognis-indexer` — 10-language tree-sitter parser, artifact extractors, Speculative Shadow AST diffing, static complexity & `OPT-001..007` analyzer, resolver, enricher, and writer
  - `cognis-csar` — CSAR forward-push retrieval solver and CSIA reverse-push loop-amplified blast-radius solver
  - `cognis-retrieval` — lexical/semantic/structural retrieval layers, RRF fusion, and token-budgeted capsule composer
  - `cognis-mcp` — 12 MCP tools, `cognis://live/guardrails` resource, JSON-RPC 2.0 stdio/HTTP server, hard caps, and audit log
  - `cognis-owner` — canonical repository lease & thin-proxy process ownership coordinator
  - `cognis-eval` — benchmark and evaluation harness
  - `cognis-testgen` — shared `proptest` generators (dev-only)
- `bins/*` — runtime surfaces (`cognis-cli`, `cognis-indexd`, `cognis-mcpd`) unified into the single multi-call `cognis` binary (`bins/cognis`)
- `xtask` — cross-platform release staging and verification (`cargo xtask dist` / `verify-release`)
- `apps/cognis-mcp` — TypeScript VS Code / Cursor extension (Live Big-O CodeLens, Guardrail Diagnostics, and one-click MCP setup)

## Day-to-day workflow

Run the Rust checks from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For the extension, work from `apps/cognis-mcp`:

```bash
cd apps/cognis-mcp
npm install
npm run lint
npm test
```

## Pull request expectations

- Keep each pull request focused on a single problem or feature.
- Update documentation whenever CLI flags, MCP contracts, or extension settings change.
- Add or update unit / property / contract-parity tests when runtime behavior changes.
- Never commit secrets, `.cognis/` runtime state, or local scratch files.

## Release process

Maintainers follow [docs/release.md](docs/release.md). Releases are tag-driven via `.github/workflows/release.yml`.

## Code of conduct

Be respectful, specific, and constructive in issues, reviews, and commits.
