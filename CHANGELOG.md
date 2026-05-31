# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.17] — 2026-05-31

### Added

- **CSAR — Code Spreading-Activation Retrieval**, the new primary retrieval
  engine. Seeds a relevance distribution from cheap lexical + semantic matches
  and diffuses it across the Unified Code Knowledge Graph via Personalized
  PageRank (random walk with restart), recovering on-path symbols that
  independent embedding/lexical ranking misses. Includes exact, power-iteration,
  and Andersen–Chung–Lang forward-push solvers. The forward-push solver has a
  provable work bound `1/(alpha*eps)` independent of repository size. Math and
  proofs in `docs/csar.md`; verified by `tests/unit/test_csar.py` and
  `tests/pbt/test_csar_pbt.py` (CP-CSAR-1..5).
- MCP tool `diffuse_context` — flagship CSAR retrieval; returns a unified ranked
  shortlist (with `on_path` flags) in one round trip, replacing separate
  `discover_symbols` + `dependency_trace` calls. Tunable via `COGNIS_MCP_CSAR_*`.
- `retrieve_context_capsule` structural stage is now CSAR-powered: lexical +
  semantic hits seed a graph diffusion whose on-path symbols feed the bug /
  root-cause sections, replacing the previous single-hop BFS.
- **Clear & Re-index**: a managed reset that deletes the stored index (UCKG
  database, WAL/SHM sidecars, capsule cache) and rebuilds from scratch while
  preserving `config.yaml` and MCP wiring. Available as the VS Code / Cursor
  command `Cognis: Clear Index & Re-index` (and a button in the panel's Index
  Status section, with a confirmation prompt) and as the CLI flag
  `cognis-cli index --clear`.
- MCP tool `discover_symbols` — hybrid lexical + semantic discovery with
  reciprocal-rank fusion in one call.
- MCP tool `resolve_symbols` — batch hydrate up to 50 symbol ids without repeated
  `symbol_lookup` round trips.
- Enriched `semantic_search` payloads (file location, signature, docstring) plus
  optional `kind` / path filters; batch SQL hydration replaces per-hit lookups.
- Short-lived in-process result cache for search tools (`COGNIS_MCP_CACHE_TTL_S`,
  default 60s).
- MCP tool `symbol_search` for top-k symbol discovery with optional `kind` and
  `file_path` filters.
- `dependency_trace` hit payloads enriched with symbol metadata (qualified name,
  kind, file path, line range when available).
- Process-wide embedder reuse for semantic MCP tools to reduce repeated model
  load latency.

### Changed

- Default MCP tool allowlist and `McpToolName` now include `diffuse_context` as
  the first (flagship) tool.
- Documentation and tests steer agents toward `diffuse_context` for
  flow-oriented retrieval, `discover_symbols` for quick discovery,
  `resolve_symbols` for batch hydration, `symbol_lookup` for exact resolution,
  and `retrieve_context_capsule` for low-round-trip task context.
- `cognis-cli init` now additively migrates stale `.cognis/config.yaml`
  defaults, writes config revision metadata, and keeps runtime loading aligned
  with newer ignore/tool defaults without clobbering user overrides.
- Generated MCP config now always includes `COGNIS_REPO_ROOT`; on Windows it
  also applies safer semantic timeout defaults unless the operator overrides the
  corresponding `COGNIS_MCP_*` env vars.
- Windows MCP defaults raised to soft/hard/discover timeouts of 30/60/30 seconds
  and `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=1` to reduce first-query timeouts.
- VS Code / Cursor extension writes MCP config to workspace `.cursor/mcp.json` or
  `.vscode/mcp.json` by default (`cognis.mcpConfigScope`), with global config as
  an opt-in fallback.
- MCP servers are registered as `cognis-<repo-slug>` (for example
  `cognis-cognis`, `cognis-edittruyentranh`) and merged additively into global
  MCP config so multiple indexed repositories can stay connected at once.
- Generated MCP config defaults to minimal env (`COGNIS_DB_PATH` plus Windows
  timeout defaults); repo root and audit log are inferred by `cognis-mcpd`.
- Successful semantic retrieval is now checked against the hard timeout only, so
  cold-start model loads no longer fail with a follow-up soft-timeout after the
  semantic stage already completed.

[Unreleased]: https://github.com/buimanhtoan-it/cognis/compare/v0.1.17...HEAD
[0.1.17]: https://github.com/buimanhtoan-it/cognis/compare/v0.1.0...v0.1.17

---

## [0.1.0] — Phase 1 MVP

### Added

**Phase 0 — Foundations**

- Repo scaffold per design "Build and Release" layout (`apps/`, `packages/`, `tests/`).
- `pyproject.toml` pinning Python ≥ 3.11 and Phase 0/1 dev dependencies (ruff,
  mypy, pytest, pytest-asyncio, pytest-benchmark, hypothesis).
- `Makefile` and `tasks.py` recipes for `lint`, `typecheck`, `test`, `bench`, `eval`.
- GitHub Actions workflows: lint+unit on push, integration on PR, nightly eval.
- `pre-commit` config: ruff format + lint + mypy.
- Apache-2.0 `LICENSE`, README skeleton, `.gitignore` covering `.cognis/` and build artifacts.
- `packages/core/cognis/config.py`: Pydantic-validated config loader for `.cognis/config.yaml`.
- `cognis-cli` (Click) with commands: `init`, `index`, `eval`, `health`, `up`, `down`,
  `mcp-conformance`, `profile`.
- SQLite schema bootstrap in WAL mode with FTS5 and sqlite-vec virtual tables.
- Migration runner with `meta.index_version` tracking.
- Eval harness skeleton (`packages/eval/runner.py`) with Recall@k, MRR metrics.
- Test fixture repos: `mini-ts-app`, `mini-py-svc`, `mini-go-svc` with planted bugs.

**Phase 1 — MVP Cognition**

- Tree-sitter parsers for TypeScript, Python, and Go.
- File watcher with 200ms debounce and `.gitignore` awareness (`watchdog`).
- Edge resolver: LSP-first with heuristic fallback, confidence-scored edges.
- Enricher: `db_table`, `http_route`, `env_var`, `external_call` detection.
- Secret redactor: Shannon entropy + pattern matching for AWS/GCP/Azure/GitHub/OpenAI/JWT/PEM.
- Embedder with local `bge-small-en-v1.5` (384d) backend; optional Voyage API.
- Writer with single-writer thread, per-file transactions, cascade deletion.
- Retrieval layers: Lexical (FTS5), Semantic (sqlite-vec KNN), Structural (recursive CTE).
- Cognitive Context Planner: rule-based classify + layer_plan + allocate_budget, < 30ms.
- Capsule composer v1 with Pydantic schema, tiktoken budget, untrusted content wrapping.
- MCP server (`cognis-mcpd`) with 4 tools: `symbol_lookup`, `semantic_search`,
  `dependency_trace`, `retrieve_context_capsule`.
- Hard limits enforcement: depth ≤ 8, k ≤ 50, max_tokens ≤ 32000, 10s hard timeout.
- Audit log (append-only JSONL, args hash only).

**Phase 1 — Conformance, Eval, Performance, Release (Tasks 16–18)**

- `cognis-cli mcp-conformance`: built-in conformance check for all 4 tools; optional
  upstream harness integration when `mcp_conformance` package is installed.
- Integration tests (`tests/integration/test_mcp_integration.py`) for all 4 tools,
  planted auth-timeout bug detection, and incremental write API verification.
- Cross-platform CI: integration tests run on ubuntu-latest, macos-latest, windows-latest.
- Golden query set expanded to 110 queries (20 each for bugfix/feature/refactor/explain/review,
  10 for migrate) across all 3 fixture repos.
- `scripts/run_eval.py`: standalone eval runner script.
- `docs/eval/phase1-baseline.md`: methodology, baseline placeholder, tuning approach,
  Phase 1 gate criteria.
- `docs/eval/swe-bench-methodology.md`: SWE-bench Lite mini-run methodology.
- `tests/benchmark/test_latency.py`: `@pytest.mark.benchmark` tests for all 4 hot-path
  latency budgets.
- `apps/cognis-mcpd/cognis_mcpd/metrics.py`: in-memory Counter/Histogram/Gauge with stdlib only.
- `docs/install.md`, `docs/quickstart.md`, `docs/mcp-client-config.md`: user-facing docs.
- `docs/performance.md`: performance guide, known gaps, profiling instructions.
- `docs/observability.md`: metrics, logging, audit log, Phase 2 Prometheus migration plan.
- `docs/release.md`: release procedure, Docker, cibuildwheel notes.
- `docs/release-notes-v0.1.17.md`: release notes for the v0.1.x line.
- `Dockerfile`: multi-stage build with bge-small-en-v1.5 pre-cached, non-root user.

### Fixed

- `mcp-conformance` CLI command: upgraded from stub to functional implementation.

### Notes on Phase 2+

- Behavioral (runtime) layer: no-op at MVP; OTel adapter planned for Phase 3.
- Temporal (git history) layer: Phase 2.
- Reranker (bge-reranker-v2-m3): Phase 2.
- SSE transport: Phase 2.
- Prometheus `/metrics` HTTP endpoint: Phase 2.

[0.1.0]: https://github.com/buimanhtoan-it/cognis/releases/tag/v0.1.0
