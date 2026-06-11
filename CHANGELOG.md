# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.2] — 2026-06-11

Patch release. Fixes a first-run panel state regression in the VS Code/Cursor
extension (no engine code change; the engine version is bumped only to keep the
bundle's pinned install in lockstep).

### Fixed

- **Fresh-install cold start no longer regresses or loops.** While the initial
  embedding backfill runs, the engine WAL-locks the DB and the vector table is
  briefly incomplete, so a health poll could momentarily read a failing
  `vector`/`index` check or fail to open the DB. The panel misread this as a
  failure and (a) reverted from "Generating embeddings…" back to "Set Up for
  AI", and (b) looped on "Troubleshoot" / "repair semantic index". The panel now
  keeps showing progress whenever the daemon reports an active index operation,
  and an already-configured workspace shows a non-destructive "Finishing setup…"
  state on a transient health gap instead of a first-run setup/repair verdict.
- Added panel state-machine regression tests and simulator fixtures for the
  embedding-backfill and transient-health-gap states — the cross-process e2e
  never sampled the panel during embedding, so these races slipped through.

## [0.5.1] — 2026-06-11

Patch release. Fixes the CI unit/PBT suite (no functional engine change).

### Fixed

- `test_stage_semantic_when_available` no longer attempts a Hugging Face model
  **download** during the unit/integration run. Having `sentence-transformers`
  installed does not mean the model weights are cached, so on a fresh CI runner
  (package present, no model cache, no network) the test reached out to the Hub
  and errored. It now forces an offline load and **skips cleanly** when the
  weights aren't already cached, honoring the module's "no network" contract;
  where the model is cached it still runs the full semantic+fusion path.

## [0.5.0] — 2026-06-11

Positioning + measurement-infrastructure release. Ships the live-indexing
UX improvements, RRF fusion, and the full development-criteria / regression-gate
loop, and corrects public copy to claim only what the benchmark supports.

### Added

- **Live embedding progress on cold index.** The initial index now publishes a
  moving "Generating semantic embeddings… X/N symbols (search already works)"
  status (70→100%) instead of sitting at a static 70% for minutes. Lexical and
  structural search remain available within seconds while embeddings backfill in
  the background (the two-phase cold index is now progress-reported end to end).
- **RRF fusion as the cross-layer ranker.** Lexical (BM25) and semantic (cosine)
  hits are now fused with parameter-free Reciprocal Rank Fusion
  (`cognis_retrieval.fusion`) instead of a scale-incoherent max-score merge —
  the strongest fusion on the reproducible objective benchmark.
- **Observability for first-use latency.** Structured timing logs for embedder
  model load (cache-hit vs online-fallback), MCP semantic-layer warm, per-call
  `semantic_search` latency, and a per-phase cold-index breakdown
  (parse/resolve/embed/write) — the basis for UX/perf decisions.
- **Full-flow coverage harness** (`make coverage`) that measures in-process and
  spawned-subprocess (CLI/indexd/mcpd) coverage together.
- **Cross-app e2e report** (`make e2e-report`) capturing per-stage latency,
  throughput, retrieval correctness, embedding-progress trajectory, and semantic
  warm-vs-hot latency split — runnable against the bundled sample repo or any
  repo via `--repo`.
- **VS Code panel simulator + Playwright UI tests** (`npm run test:e2e`) that
  render the real webview markup per state and assert every button posts a valid
  command intent, with no VS Code instance required.
- **Offline per-version licensing.** The prebuilt build verifies an Ed25519
  license key fully offline (no license server), with a **version band** —
  a `0.5` key unlocks every `0.5.x` patch but not `0.6` (free patches, next
  minor is a separate purchase). The "Buy" action is configurable via the
  `cognis.buyUrl` setting. The open-source build ships no embedded key, so its
  gate is fully open.

### Fixed

- **Engine imports without the `embed-local` extra.** `numpy` is now an optional
  runtime dependency across the indexer/retrieval import chain, so a
  lexical+structural-only install imports and runs (degrading gracefully to
  no-semantic) instead of failing at import. The full install (`embed-local`)
  is unchanged.

- First-time `semantic_search` no longer needs the embedder loaded on a worker
  thread (warm-on-startup), and the slow online model-revalidation path is now
  surfaced instead of hanging silently.
- e2e contract snapshot for `mcp-config` no longer bakes the machine-specific
  hashed server name into the golden (was non-portable across workspaces).

### Changed

- Type-checking is now strict over `cognis_indexer` in addition to
  `packages/core` (`make typecheck`), with the embedder-pipeline Optional
  narrowing fixed — the planned per-cycle ratchet toward full strict coverage.


## [0.4.0] — 2026-06-06

First commercial release of the VS Code / Cursor extension.

### Added

- **Offline license gate scaffold.** `apps/cognis-vscode/src/licenseCore.ts`
  (pure Ed25519 verification) + `license.ts` (editor plumbing) + the
  `cognis.enterLicense` command. Paid features call `requireLicense(...)`, which
  is a no-op in the open-source/source build (no embedded key) and enforces in
  the prebuilt commercial build. `Set Up for AI` is gated as the first example.
  Verification is fully offline — no license server, zero ops after a sale.
- **Real MCP concurrency cap.** `cognis-mcpd` now bounds concurrent tool
  execution with a process-wide semaphore (`COGNIS_MCP_MAX_CONCURRENCY`,
  default 16) via a `_bounded_tool` decorator; a saturated server returns a
  retryable envelope instead of piling up work. This makes the documented
  "concurrent requests" limit code-backed.
- **Version is now single-source.** `cognis.__version__` derives from
  `pyproject.toml` (PEP 621) at runtime instead of a duplicated literal, and the
  extension test harness reads its version from `package.json`. Engine and
  extension are both **0.4.0**; the Docker image tag is parameterized
  (`COGNIS_VERSION`).
- **Stronger math tests.** Added a unit test verifying the CSAR `α→0 ⇒
  stationary distribution` endpoint (previously docs-only) and concurrency-cap
  tests for the MCP server.
- **Version badge in the panel.** The Cognis sidebar header now shows the
  installed extension version (e.g. `v0.4.0`).
- **Standalone sellable installer build.** `scripts/build_installer.py` packages
  the compiled extension `.vsix` plus an `INSTALL.md` and the commercial license
  into `dist/cognis-pro-<version>.zip` — the artifact distributed via a
  Merchant-of-Record. The `dist/` output is git-ignored (never committed).
- **Architecture + audit section in the README.** A diagram
  (`assets/architecture.svg`) and a plain-language "how it works" walkthrough,
  plus an independent capability/security audit summary (ratings backed by code
  and tests), so users understand the system well enough to self-set-up — while
  most still choose the one-click installer.
- **"Connect to AI" MCP setup guide.** New command `cognis.connectToAi` (and the
  panel's "Connect to AI" primary action) writes/refreshes the workspace MCP
  config, then opens a copy-paste-ready guide: the collected environment
  variables, the exact `mcpServers` JSON for this workspace, the on-disk config
  path, and per-host reload steps for Cursor, VS Code, and Claude Desktop.
- **Pause / resume index sync.** New commands `cognis.pauseSync` /
  `cognis.resumeSync` and panel buttons. Sync is auto-on by default; pausing
  stops the live-indexing daemon and prevents auto-restart on reload or file
  save, while keeping the built index and MCP wiring intact. The paused state is
  persisted per workspace and reflected in the panel's Index Status section.

### Changed

- **Security docs corrected.** The "concurrent requests" hard-cap claim was
  reworded to match the code: isolation comes from per-call wall-time limits and
  a single-flight lock + cooldown on the semantic stage, not a global request
  semaphore.
- **License: the extension is now commercial/proprietary** (`SEE LICENSE IN
  LICENSE.txt`), separate from the open-source engine. The packaged `.vsix` is
  the paid product distributed via a Merchant-of-Record.


## [0.3.2] — 2026-06-05

### Fixed

- **MCP server keys are now identical across operating systems.** The
  human-readable slug part of the key was derived with a platform-specific path
  helper, so a Windows-style path processed on a non-Windows host (e.g. CI, or a
  remote/WSL backend) produced a different key than the same repo on Windows —
  which could create a duplicate MCP entry. The slug now extracts the final path
  segment in a separator-agnostic way, matching the already-normalized path
  hash, so the extension and `cognis-cli` always agree regardless of platform.
- **Backend auto-upgrade after an extension update.** When the managed Python
  backend is older than the installed extension, Cognis now offers a one-click
  upgrade on activation (managed environments only; a bring-your-own Python is
  never touched), with a "skip this version" option so it doesn't nag.

## [0.3.1] — 2026-06-05

### Fixed

- **MCP server entries no longer collide for repos that share a folder name.**
  The per-repo MCP key was derived from the folder basename only, so two repos
  named the same (e.g. `work/api` and `personal/api`) both became `cognis-api`
  and overwrote each other in the shared global MCP config — breaking semantic
  search for whichever was wired first. Keys now include a short, stable hash of
  the full repo path (`cognis-api-3f9a2c`), so any number of repos — including
  same-named ones — can be connected at once. The extension and the
  `cognis-cli mcp-config` command derive identical keys, and existing entries
  are migrated automatically on the next connect (matched by `COGNIS_DB_PATH`,
  not by name, so nothing is left orphaned).

## [0.3.0] — 2026-06-05

### Added

- **One-click backend install/uninstall.** The VS Code / Cursor panel now
  installs the Cognis Python backend for you — it creates a private environment
  it manages (no terminal, no `pip`, no choosing a Python) and offers to set up
  the workspace right after. The **Danger zone → Remove everything** action
  reverses it, deleting that managed environment cleanly. If you bring your own
  Python via `cognis.pythonPath`, install/uninstall operate on the `cognis`
  package there and never touch your environment. New `cognis.installBackend`
  command and `cognis.backendPackageSpec` setting.
- **Lifecycle removal commands.** `cognis.removeFromWorkspace` stops indexing,
  disconnects this repo's MCP entry, and deletes the local `.cognis/`.
  `cognis.prepareUninstall` additionally strips every `cognis-*` server from the
  shared MCP config and uninstalls the managed backend, so nothing is orphaned
  after the extension is removed.
- **Onboarding stepper.** The panel shows a fixed 4-step path
  (Backend → Components → Index synced → AI connected) so a first-time user
  always sees where they are and the single next action. A dedicated "Install
  the Cognis backend" state guides fresh machines instead of failing setup with
  a raw import error.

### Changed

- **`.cognis/` is added to `.gitignore` automatically** after setup when the
  workspace is a git repo and the entry is missing (idempotent), with a
  non-blocking notice — instead of a prompt with a "Don't ask again" choice.
- **Plainer, behavior-based wording.** Removed the term "interpreter" from
  user-facing copy; renamed **Repair Setup → Troubleshoot & Repair** and
  **Clear Index & Re-index → Rebuild Index** (command IDs unchanged). The status
  bar now uses a short, stable vocabulary (Indexing / Ready / Action needed /
  Not set up).
- The VS Code / Cursor panel's **Prerequisites checklist now collapses** once
  every required component is installed, showing a one-line "Ready" summary
  instead of the full list. It auto-expands when a required component is missing
  so the install action stays obvious, and can be expanded manually any time to
  re-check or install optional extras.

### Fixed

- **Live indexing from the extension now makes the workspace searchable in
  seconds instead of appearing to fail.** The `cognis-indexd` cold rebuild
  (spawned by the extension's "Set Up for AI" / live indexing) embedded *every*
  symbol before writing any of them, so on a real repository the index DB stayed
  empty — and the health panel reported `index: fail` ("0 files … excluded by
  .gitignore") — for the entire multi-minute embed. The daemon now cold-indexes
  in two phases: lexical + structural data first (fast, commits immediately so
  search works and health flips to `ok`), then backfills semantic embeddings in
  the background. Manual `cognis-cli index` was unaffected because operators
  waited for it (or used `--skip-embeddings`); the failure only surfaced through
  the daemon path.
- `cognis-cli health` and the `index --clear` diagnosis no longer blame
  `.gitignore` / `repo.ignore` when the real cause is an unfinished index run.
  When indexable source exists but the DB is empty, they now point to running
  `index --full` instead of asserting the source was excluded.

## [0.2.1] — 2026-06-02

### Added

- **Prerequisite checklist in the VS Code / Cursor panel.** Before setup, the
  panel now lists each installable backend component (parsers, local embeddings,
  vector search, MCP server, tokenizers) with an installed/missing marker and a
  per-item **Install** button (plus **Install all**). Backed by a new
  `cognis-cli doctor --json` command. **Set Up for AI** is blocked until the
  required components are installed, so a fresh user is never left with a
  half-provisioned workspace.
- **`.gitignore` reminder.** After setup, in a git repository, the extension
  offers to add `.cognis/` to `.gitignore` (with a "Don't ask again" option) so
  the local index database, caches, and audit log are never committed.

### Changed

- **The extension no longer auto-creates `.cognis/` on activation.** Opening a
  folder leaves the repository untouched; `.cognis/` is created only when the
  user explicitly runs **Set Up for AI**. Activation still auto-manages
  workspaces that are already configured.

## [0.2.0] — 2026-06-02

### Fixed

- **`semantic_search` (and the semantic stage of `retrieve_context_capsule`)
  no longer hang / time out on first use over MCP stdio.** The embedder
  (`sentence-transformers`/`torch`) was being loaded for the first time on a
  spawned worker thread inside the server; first-time torch initialization off
  the main thread hangs, so the tool blocked until the MCP deadline fired and
  returned a `TIMEOUT`. `cognis-mcpd` now warms the shared semantic layer
  **synchronously on the main thread** before serving, so every tool call
  reuses the cached singleton. Disable with
  `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0` (re-exposes the off-main-thread
  first-load hang for semantic tools).
- `cognis-mcpd` warms the UCKG `Database` (and the optional `sqlite-vec`/`numpy`
  import) on the main thread before serving, avoiding an import-lock deadlock
  when the first tool call would otherwise trigger that import on a FastMCP
  worker thread (observed on Python 3.14 / Windows).
- `cognis-indexd` releases its SQLite connections on shutdown (dedicated
  single-thread writer executor) and writes its status file atomically with a
  retry, fixing a Windows `os.replace` sharing-violation crash when an IDE polls
  the status file concurrently.

### Added

- Cross-app **end-to-end test suite** (`tests/e2e/`, marker `e2e`) that drives
  the real `cognis-cli`, `cognis-indexd`, and `cognis-mcpd` over process
  boundaries (CLI JSON, the live status file, and MCP stdio), plus committed
  JSON **contract snapshots** verified against the VS Code extension's
  TypeScript interfaces (`apps/cognis-vscode/src/test/contractParity.test.ts`).
  Includes a regression test that indexes with embeddings and asserts
  `semantic_search` returns over stdio instead of hanging. See
  `docs/e2e-testing.md`.

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

[Unreleased]: https://github.com/buimanhtoan-it/cognis/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/buimanhtoan-it/cognis/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/buimanhtoan-it/cognis/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/buimanhtoan-it/cognis/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/buimanhtoan-it/cognis/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/buimanhtoan-it/cognis/compare/v0.1.17...v0.2.0
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
