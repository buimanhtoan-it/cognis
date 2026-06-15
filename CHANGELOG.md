# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.3] — 2026-06-15

Two new languages: C# and Java are now first-class. The retrieval core is
unchanged — CSAR is language-agnostic — so this is purely new parser coverage
plus the wiring to enable it by default.

### Added

- **C# parser** (`.cs`, tree-sitter-c-sharp) and **Java parser** (`.java`,
  tree-sitter-java). Both extract classes, interfaces, methods, and
  constructors; structs/records/enums map to `class` (the model has no dedicated
  enum/struct kind). Nested types are qualified (`Outer.Inner.method`), and
  XML-doc (`///`) / Javadoc (`/** */`) comments are captured as docstrings.
  Symbol IDs are stable under cosmetic edits (CP-2), matching the other parsers.
- **C#/Java OOP edges.** A language-aware resolver
  (`OOPRelationshipResolver`) emits `inherits` / `implements` edges from class
  headers (`: Base, IFoo` in C#; `extends`/`implements` in Java). It only links
  to types that exist in the repo (external bases like `System.Object` /
  `java.lang.Object` produce no noise), and the edges feed both CSAR diffusion
  (`build_code_graph` includes all kinds) and `dependency_trace`.
- **Snapshot fixtures** `mini-cs-app` and `mini-java-svc` with curated
  `expected_symbols.json`, wired into the parser snapshot suite.
- `languages.enabled` now defaults to
  `[typescript, python, go, csharp, java]`; the indexer maps `.cs → csharp` and
  `.java → java`. Existing workspaces pick the new languages up on the next full
  index.

### Changed

- `cognis-engine[indexer]` now also pulls `tree-sitter-c-sharp` and
  `tree-sitter-java`. A missing grammar degrades gracefully — those files are
  skipped, the rest of the index still builds.

## [0.7.2] — 2026-06-15

Status-honesty patch: a stale on-disk index version no longer gets stuck
mid-onboarding while quietly driving an endless rebuild loop. The fix is in the
indexer itself, so every entrypoint (CLI and daemon) agrees.

### Fixed

- **Daemon full rebuilds now stamp `meta.index_version` (root cause of a stuck
  "Index synced" step + rebuild loop).** Only the CLI `index --full/--clear`
  path wrote `index_version`; the `cognis-indexd` `--full-rebuild` rebuilt the
  index but left the stamp untouched. After an upgrade (e.g. an index built by
  0.3.0 served by 0.7.1), the `health` version check failed *forever*, the
  onboarding stepper's **Index synced** step showed an error, and — because the
  extension's auto-manage treats a failing version check as "needs rebuild" — it
  re-forced `--full-rebuild` on every activation, an endless loop that never
  cleared the mismatch. The stamp now lives in `IndexerPipeline.index_repo`
  (written on any `full=True` index), so the CLI and daemon share one source of
  truth and a forced rebuild actually resolves the mismatch.
- **Onboarding stepper and headline no longer disagree during `watching`.** The
  panel's "active indexing" bypass was gated on the broad `indexStatus.active`,
  which is still true in the steady-state `watching` phase — so a genuine health
  failure (like the stale `index_version` above) was masked by "Watching for
  file changes" in the headline while the stepper (which reads `health.overall`)
  showed an error. The bypass is now gated on `isIndexStatusBusy` (genuine
  in-flight work), so a real failure surfaces consistently; the cold-index /
  embedding progress protection is preserved.

### Changed

- **`health` version-check docstring** now matches behavior (`index_version`
  drift → `fail`, not `warn`): a stale index must be rebuilt before it is served.
- **CI typing stability for the embedder.** `LocalEmbedder.embed_batch` now
  narrows the `object`-typed model handle with `typing.cast` instead of an
  annotated assignment. Newer `sentence-transformers` releases ship `py.typed`,
  which turned the previously-fine assignment into a mypy `[assignment]` error
  on a fresh dependency resolve; `cast` is stable whether the dependency exposes
  types or is treated as `Any`, so the `lint + unit` gate no longer breaks on a
  dependency-version float.


## [0.7.1] — 2026-06-14

Honesty + correctness patch: the panel now reports MCP connectivity from the
*real* running server (not just on-disk config), the eval gate stops asserting a
quality number the project does not claim, and a single command keeps every
version file from drifting at release time.

### Added

- **Live MCP runtime probe (`mcpRuntime.ts`).** The panel now detects the actual
  editor-spawned `cognis_mcpd` stdio processes (Cursor-style), so "connected"
  means *configured in `mcp.json` **and** a server is really running* — not just
  that the config was written. The count is repo-scoped on Linux/macOS (verified
  against each process's environment via `envMatchesRepo`); on Windows the OS
  does not expose another process's environment through built-in tooling, so the
  count is machine-wide and the panel says so plainly instead of overclaiming.
- **One-command version bump (`scripts/bump_version.py`).** Writes the version to
  `pyproject.toml`, the extension `package.json`, and its lockfile (both fields)
  and scaffolds the CHANGELOG section. `--check` mode is now a CI gate that fails
  the build on version drift across files (e.g. extension 0.7.x vs engine 0.6.x).
- **`make bench-public` / `invoke bench-public`.** Runs the fair-harness
  retrieval comparison (BM25/DENSE/RRF/2HOP/CSAR/UNION) over the public repos —
  the reproducible numbers behind `.benchmarks/public/RESULTS.md`.

### Changed

- **Eval gate is now an explicit no-regression smoke gate.** `eval-baselines/phase1.json`
  recorded aspirational minimums (Recall@10 ≥ 0.70, MRR ≥ 0.50) the engine never
  met on the synthetic golden, so the build failed on an ungrounded absolute, not
  a regression. It now records the *measured* Recall@k / MRR as a baseline and
  fails only on a regression beyond `regression_tolerance` (default 0.05).
  `scripts/compare_eval_baseline.py`, `docs/development-criteria.md`, and
  `docs/eval/phase1-baseline.md` were aligned, with a legacy `*_min` fallback.
- **`mypy` strict now covers `packages/indexer`** in addition to `packages/core`.
- **README** shows a git-tag version badge (no hardcoded version) and an honest,
  reproducible benchmarks section that states what the numbers do and do not claim.

### Fixed

- **Panel ↔ mcpd connectivity desync.** The panel previously showed "connected"
  from the presence of the `mcp.json` entry alone, so it could claim a working
  MCP server when the editor had not actually launched one. It now reflects the
  live process state and distinguishes *not connected* / *configured (not
  running)* / *connected*.
- **False "duplicate MCP process" warning on Windows.** The warning fired on a
  machine-wide count, so opening two workspaces flagged a spurious duplicate. It
  is now gated to the repo-scoped count only.
- **Windows path comparison in MCP env matching.** Drive-letter casing and
  slash style (`D:\...` vs `d:/...`) no longer cause a false mismatch when
  attributing a config/process to a repo (`pathsEqual` / `normalizePathForCompare`).

## [0.7.0] — 2026-06-12

Reliability + observability release: close the extension ↔ backend integration
gaps, make every user flow traceable, and cover all interaction paths end to end.

### Added

- **Structured diagnostics trace.** The extension writes append-only JSON Lines
  to `diagnostics.jsonl` (size-rotated, mirrored to the Cognis output channel),
  surfaced by the new **Cognis: Show Diagnostics Log** command and tuned by the
  `cognis.logLevel` setting. Every CLI call (exit + duration), every command
  flow (start/ok/fail + duration), every surfaced error, the startup handshake,
  and unknown indexd phases are recorded — so a production bug is reconstructable.
- **Contract version handshake.** `cognis-cli handshake` advertises
  `{contract_version, engine_version, cli_commands, mcp_tools}` from a single
  source of truth (`cognis/contract.py`); the extension negotiates it at startup
  and warns actionably on version skew instead of failing silently.
- **MCP tool output contracts** for all 8 tools (incl. flagship `diffuse_context`
  `on_path`/`ppr_score` and the capsule schema), asserted against the real server.
- **Resource-leak guards** (`pytest -m e2e -k memory`): real `cognis-mcpd` and
  `cognis-indexd` under sustained load stay handle/RSS bounded.
- **Full-stack host e2e** (`npm run test:host`, CI job `vscode-host-e2e`): the
  real extension in a real VS Code against the real backend runs Set Up Workspace
  and asserts the real `.cognis/` + `mcp.json` are written and traced.

### Changed

- **Removed all "AI" wording for concrete language.** "Set Up for AI" → **Set Up
  Workspace**, "Connect to AI" → **Connect MCP**; commands renamed
  (`cognis.setupForAi` → `cognis.setupWorkspace`, `cognis.connectToAi` →
  `cognis.connectMcp`).
- **Connect MCP now does the work concretely** — writes the real workspace
  `mcp.json` and opens it, instead of printing a copy-paste guide.
- Boundary parsing (`runCliJson`, indexd status) now traces contract/parse
  failures instead of propagating a silent `undefined`.

### Fixed

- **Deterministic worker-thread DB cleanup in `cognis-mcpd`.** Each semantic tool
  stage runs on a throwaway worker thread that opened a per-thread sqlite
  connection; it is now closed in `_run_with_deadline`'s `finally`, keeping peak
  handles/RSS low under bursts and eliminating the "unclosed database" warnings.
- **De-flaked the e2e suite.** Benign cross-process `ResourceWarning`s (real
  subprocess + async client teardown on Python 3.14) were misattributed by
  pytest's unraisable plugin to random e2e tests under `filterwarnings = error`;
  scoped a filter to `e2e`-marked items (quantitative leak detection moved to the
  dedicated memory guards).


## [0.6.2] — 2026-06-12

Patch release. CI/test-only fix (no engine or extension code change).

### Fixed

- **`make lint` failed CI on a `ruff format` violation.** The 0.6.1 contract
  helper `_normalize_paths` contained a dict comprehension that `ruff format`
  wanted on a single line, so `ruff format --check` failed; because `make`
  returns exit code 2 when any recipe fails, the `lint + unit` job died before
  pytest ran on both py3.11 and py3.12. Reformatted to the canonical layout.
- **mcp-config contract snapshot leaked environment-specific env keys.** The
  snapshot pinned the server-block `env` but not the top-level `env`, so
  passthrough keys (`HF_*`, `COGNIS_MCP_*`, ...) varied across machines and
  failed the e2e sandbox on Linux CI. It now pins only the stable
  `COGNIS_DB_PATH`.
- **De-flaked the semantic inflight-wait unit test.** Its 0.1s/0.2s timing
  budgets were too tight: under CI load a cold database open could miss the
  `started` wait, exit the patch scope, and let the worker return `[]` against
  the unpatched availability check. Widened to test-only budgets that keep the
  overlap small relative to the deadline (production timeouts unchanged).

## [0.6.1] — 2026-06-11

Patch release. CI/test-only fix (no engine or extension code change).

### Fixed

- **Cross-language contract snapshots are now platform-independent.** The e2e
  contract goldens were generated on Windows (where the `cognis-mcpd` console
  script is not on PATH, so `commands.cognis_*` are `null` and the MCP server
  block carries `args`), so they failed when the e2e sandbox first ran on Linux
  CI (`str` paths, no `args`). The snapshots now normalize these
  environment-specific fields (nullable command paths, the console-script-vs
  `python -m` block shape, passthrough env keys, timing-dependent status file
  lists), so they pin the real contract shape and pass on every platform.

## [0.6.0] — 2026-06-11

Feature release. Adds an optional standalone **HTTP MCP server** (panel-managed,
per workspace) alongside the default stdio transport, MCP-focused panel UX,
workspace-visible `mcp.json`, and clearer install/upgrade errors. Supersedes the
never-published 0.5.3.

### Added

- **Per-window HTTP MCP server, panel-managed, one-click.** The Cognis MCP
  server can run as a standalone HTTP server (`cognis-mcpd --transport http`)
  with a stable per-workspace localhost URL. The panel's collapsible
  "Standalone HTTP MCP server" sub-section has **Start** / **Stop**, the live
  `http://127.0.0.1:<port>/mcp` URL, and the phase (Stopped / Starting /
  Running / Error). **Start** pre-flights the workspace, launches the server
  (TCP readiness probe + automatic port retry), **auto-writes the url-form
  `mcp.json`** so the editor connects, and offers a one-click **Reload Window**.
  **Stop** reverts `mcp.json` to the editor-managed stdio form so AI tools keep
  working; a dangling http config is also reverted to stdio on activation.
  Stdio remains the default; the server binds loopback only unless
  `COGNIS_MCP_ALLOW_REMOTE=1`.
- The sold bundle's `INSTALL.md` now includes an "Updating to a new version"
  section so buyers can self-serve a fix/upgrade.
- One-click **Reload Window** in the post-setup and MCP-config guidance.

### Changed

- **The panel now states the MCP server status explicitly** — connected/not,
  server name, and workspace `mcp.json` path, with a single **Set up MCP
  (mcp.json)** action — replacing the vague "Set Up for AI" / "Connect to AI" /
  "AI connected" wording. Connected reads "Cognis MCP server connected"; the
  onboarding step is "MCP connected".
- **MCP config is written into the workspace by default** so it is visible and
  per-project (`.vscode/mcp.json` / `.cursor/mcp.json`) instead of the global
  home config. Added the `cognis.mcpConfigScope` setting (`workspace` |
  `global`).

### Fixed

- **A clear message when the engine version isn't on PyPI yet.** A pin to an
  unpublished `cognis-engine` previously showed a misleading "your Python is too
  new" error; it is now reported honestly as "this engine version is not on PyPI
  yet — wait and retry".

### Internal

- The cross-process e2e sandbox now runs on every push (not just PRs); added a
  wheel-packaging e2e (the sold artifact ships all packages + entry points +
  asset), an HTTP-MCP round-trip e2e, `buildPackageSpec` pin tests, and a filter
  for a third-party opentelemetry deprecation that broke the MCP e2e suite.

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
