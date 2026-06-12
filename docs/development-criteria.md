# Development criteria — the measurement loop for every release

The reference for **what we measure** to develop cognis sustainably, release
after release. Every cycle is judged against the four pillars below. Each
criterion names **where it is measured** (an existing command/artifact) and a
**target / gate**, so progress is measurable rather than assumed.

This document indexes the existing instruments; it does not duplicate them:
- Retrieval quality benchmark → the benchmark harness under `.benchmarks/`
  (developer-local; not shipped in the package).
- UX / performance + retrieval correctness → `make e2e-report` (`tests/e2e/report.py`).
- Coverage → `make coverage` (`scripts/coverage_full.py`, `tests/coverage/`).

> Evidence discipline (applies everywhere): label every result as **proven**
> (algebra machine-verified), **empirically supported** (beats baselines on a
> finite, named sample — always quote n), or **conjectured**. A passing
> benchmark on finite data is empirically supported, not proven.

## Benchmark provenance & reproducibility

Measurements run against **named, free, public GitHub repositories** — not
private or synthetic data (the bundled tiny sample repo is used only for the
fast CI smoke gate, and is labeled as such). Every report and committed baseline
records the **exact source it measured**: origin URL, HEAD commit, `git
describe` version, and whether the tree was dirty (`repo_provenance` in the JSON
/ a "Measured against:" line in the Markdown).

- Reproduce any number exactly:
  `python tests/e2e/report.py --clone <url> --ref <commit>` clones fresh and
  records the resolved commit; `--repo <local checkout>` measures a local clone
  and records its commit.
- Baselines **pin a specific commit** (recorded), not a moving branch HEAD, so
  runs are comparable release-over-release. Refresh the corpus deliberately (and
  the recorded commit changes), never silently.
- Current large-repo reference: `psf/requests` (recorded in
  `tests/e2e/baselines/requests.json`).

---

## Pillar 1 — Retrieval quality (governs accuracy of any public claim)

Measured by the benchmark harness on **objective, PR-derived** ground truth
(the symbols changed in a real bug-fix commit are the answer), not author-chosen
concept labels.

| Criterion | Where | Bar to support an "outperforms standard baselines" claim |
| --- | --- | --- |
| Recall@k, MRR | benchmark harness | beats BM25, dense KNN, and RRF on the macro average |
| Contamination@k | benchmark harness | ≤ RRF (lower is better) |
| Ground-truth objectivity | PR-mining + leakage check | structure-blind; circularity measured, not assumed |
| Sample size & breadth | benchmark log | ≥ 60 resolvable objective queries, ≥ 2 languages at scale |
| Reproducibility | benchmark log | reproduces from a fresh clone |
| Math soundness | verification scripts | identities machine-verified to machine epsilon |

Until all six hold, the accurate description is "a local, mathematically-grounded
retrieval engine, with quality under active benchmarking" — public performance
claims should not outrun this bar.

## Pillar 2 — UX / performance (protects first-use experience)

Measured by `make e2e-report` (emits `eval-reports/.../e2e-report.json`).
Reference values (psf/requests, 736 symbols, CPU) recorded this cycle:

| Criterion | Reference | Target / budget |
| --- | --- | --- |
| Time-to-first-lexical-result | seconds (Phase A) | search usable before embeddings finish |
| Embedding-progress moved | true (70→100 %, "X/N") | must stay true (never a static bar) |
| Embedding throughput | ~0.2 s/symbol (embed ≈ 98 % of cold index) | alert if symbols/sec drops > 20 % vs baseline |
| Server warm/startup (one-time) | ~17 s (model + framework import) | track; the lever if we invest in startup |
| Steady-state semantic query (hot) | ~0.04 s | p50 < 0.3 s |
| Cold-index per-phase split | parse/resolve/write ~1 s each | flag if a non-embed phase regresses |

## Pillar 3 — Reliability / correctness (CI gates, must stay green)

| Criterion | Command | Gate |
| --- | --- | --- |
| Lint + format | `make lint` | clean |
| Types | `make typecheck` | clean; expand the mypy-strict scope each cycle (now: `packages/core` + `cognis_indexer`; next: `cognis_retrieval`) |
| Unit + property + integration | `make test` | 100 % pass |
| Coverage | `make coverage` | `fail_under` ratchets up only (currently 60; measured ≈ 76), never down |
| Cross-app e2e | `make e2e` | all pass; **runs on every push** (ci.yml `e2e-sandbox` job) + cross-platform on PRs; no hidden flakes (reproduce-in-isolation and classify, never blind retry) |
| Sold-artifact packaging | `pytest -m e2e -k wheel` | the built wheel ships all 8 packages + 3 console entry points + the logo asset |
| Backend install/upgrade | extension unit (`buildPackageSpec`, `classifyPipFailure`) | the engine pin is deterministic (`==<ext version>`); pip failures classified (incl. "engine not on PyPI yet" vs "Python too new") |
| Panel UI e2e | `npm run test:e2e` | all states pass; every button posts a valid command |
| Full-stack host e2e | `npm run test:host` (CI: `vscode-host-e2e` job, xvfb) | the real extension in a real VS Code host, against the real Python backend, runs `cognis.setupWorkspace` and writes a real `.cognis/config.yaml` + workspace `mcp.json`; the flow appears in `diagnostics.jsonl`. Needs `COGNIS_TEST_PYTHON` (skips otherwise) |
| Cross-language contracts | contract snapshots | extension ↔ CLI JSON shapes pinned (regenerate only on intentional change) |
| MCP tool output contract | `pytest -m e2e -k mcp_tool_contracts` | all 8 AI-facing tools keep the keys agents depend on — search/lookup/trace/resolve, hybrid `discover_symbols`, flagship `diffuse_context` (`on_path`/`ppr_score`), `retrieve_context_capsule` schema, error envelope; tool set matches `cognis.contract.MCP_TOOLS` (asserted against the live server) |
| Contract version lockstep | `pytest -m e2e -k contract_version` | backend `CONTRACT_VERSION` == extension `EXPECTED_CONTRACT_VERSION` (bump both together); `cognis-cli handshake` advertises the negotiated payload |
| Handshake skew handling | extension unit (`contract.test.ts`) | every skew case (older/newer/missing-capability/unreadable) maps to a clear, actionable verdict; usable only when required capabilities are present |
| Flow tracing (bug trace) | extension (`diagnostics.test.ts`) | every user flow is reconstructable from `diagnostics.jsonl` (Cognis: Show Diagnostics Log): each progress-wrapped flow logs start/ok/fail+duration via `trace.span`, start/stop MCP + connectMcp + handshake log explicitly, every surfaced error logs via `showErrorGuidance`, every CLI call logs exit+duration, unknown indexd `phase` recorded once |

## Pillar 4 — Scaling / cost (protects large-repo viability)

Measured by `make e2e-report E2E_REPORT_ARGS="--repo <repo>"`:

| Criterion | Reference | Gate |
| --- | --- | --- |
| DB bytes / symbol | ~27 KB (requests) | track; flag superlinear growth |
| Cold-index wall time (large repo) | record per cycle | sub-linear vs symbol count |
| Memory / handle footprint (mcpd) | `pytest -m e2e -k memory` | real server under sustained tool load stays resource-bounded: OS-handle growth ≤ 60 and RSS growth ≤ 80 MB over hundreds of calls (the leak fingerprint is a near-linear handle climb). Lexical guard is model-free (CI); semantic guard exercises the per-call worker-thread connection path (local embedder; nightly/local) |

---

## The per-cycle loop

1. **Baseline** at cycle start: `make e2e-baseline-update` refreshes the committed
   smoke baseline (`tests/e2e/baselines/sample.json`, used by CI for correctness).
   For a real-repo perf trend, diff against the committed large-repo reference:
   ```
   make e2e-report E2E_REPORT_ARGS="--repo <repo> --out eval-reports/<name>"
   python scripts/compare_baseline.py --current eval-reports/<name>/e2e-report.json \
       --baseline tests/e2e/baselines/requests.json
   ```
2. **Develop**: every change should leave at least one of *correct / accurate /
   efficient* measurably better, or produce a sound negative result that
   prevents wasted effort.
3. **Gate** before merge: Pillar 3 green (`make lint typecheck test`);
   `make compare-baseline` enforces Pillar-2/4 invariants (and perf budgets under
   `COMPARE_ARGS="--strict-perf"`); Pillar 1 governs any new public claim.
4. **Record**: update the benchmark log (quality, labeled by evidence tier) and
   `CHANGELOG.md` (`[Unreleased]`).

## Automated regression gate

`scripts/compare_baseline.py` turns the e2e report into a gate:

- **Hard invariants** (hardware-independent flow correctness — embedding bar
  moves, semantic search returns hits, index has vectors, health ok, symbol
  search resolves) **fail the build**.
- **Soft perf budgets** (throughput, hot-query latency, warm time, DB
  bytes/symbol) are reported relative to the baseline and only fail under
  `--strict-perf` (default tolerance ±50 %), so CI on variable hardware gates on
  correctness while perf is tracked for trend.

Wired into `.github/workflows/nightly-eval.yml`. Commands:
`make compare-baseline`, `make e2e-baseline-update`.

Two committed reference baselines under `tests/e2e/baselines/`:
- `sample.json` — bundled sample repo; fast, hardware-independent invariants;
  used by CI.
- `requests.json` — large real repo (psf/requests, 736 symbols); the perf/scaling
  trend reference (run locally, machine-specific latency, soft budgets).

## Practices to avoid

- Quoting a finite-sample number as if it were a general guarantee.
- Presenting concept-label recall as evidence of generalization.
- Omitting the contamination / regression / skip columns from a comparison.
- Lowering a coverage or quality gate to make a build pass.
- Public performance claims ahead of the Pillar-1 bar.

## Status (this release)

- Pillars 2 & 3: green and instrumented.
- Pillar 1: the **sample-size / breadth** criterion is now met — the objective
  (PR-derived, structure-blind) key spans 276 queries across 5 public repos in
  two languages at scale (Python: requests; Java: jsoup). The
  **"outperforms standard baselines"** bar is intentionally *not* claimed: on
  objective bug-fix truth the strongest ranker is RRF (BM25 + dense), and every
  structural ranking variant (raw PPR, degree-free lift, hub-suppressed lift,
  query-conditional contrast) is a settled negative — so the engine ranks with
  RRF and uses structure (CSAR/UNION) as proven low-contamination on-path
  context. Pillar 1 governs public claims, not whether the tool ships; the
  honest framing is "a local, mathematically-grounded retrieval engine, RRF-ranked,
  with structure as proven on-path context."
- The regression gate (`scripts/compare_baseline.py`) is built, committed, and
  wired into nightly CI.
