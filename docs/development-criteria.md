# Development criteria — the measurement loop for every release

The reference for **what we measure** to develop cognis sustainably, release
after release. Every cycle is judged against the four pillars below. Each
criterion names **where it is measured** (an existing command/artifact) and a
**target / gate**, so progress is measurable rather than assumed.

This document indexes the existing instruments; it does not duplicate them:
- Retrieval quality benchmark → the benchmark harness data under `.benchmarks/`
  (developer-local; not shipped in the binary).
- Reliability / correctness → the Cargo workspace test suite (`cargo test
  --workspace`), including the `cognis-eval` differential parity + golden-set
  harness.
- Coverage → `cargo llvm-cov --workspace`.

> Evidence discipline (applies everywhere): label every result as **proven**
> (algebra machine-verified), **empirically supported** (beats baselines on a
> finite, named sample — always quote n), or **conjectured**. A passing
> benchmark on finite data is empirically supported, not proven.

## Benchmark provenance & reproducibility

Measurements run against **named, free, public GitHub repositories** — not
private or synthetic data (the bundled tiny sample repo is used only for the
fast CI smoke gate, and is labeled as such). Every committed baseline records
the **exact source it measured**: origin URL, HEAD commit, `git describe`
version, and whether the tree was dirty (`repo_provenance` in the JSON).

- Baselines **pin a specific commit** (recorded), not a moving branch HEAD, so
  runs are comparable release-over-release. Refresh the corpus deliberately (and
  the recorded commit changes), never silently.
- Current large-repo reference: `psf/requests` (recorded in
  `tests/e2e/baselines/requests.json`).

---

## Pillar 1 — Retrieval quality (governs accuracy of any public claim)

Measured by the benchmark harness on **objective, PR-derived** ground truth
(the symbols changed in a real bug-fix commit are the answer), not author-chosen
concept labels. Results live in `.benchmarks/public/RESULTS.md`.

| Criterion | Where | Bar to support an "outperforms standard baselines" claim |
| --- | --- | --- |
| Recall@k, MRR | benchmark data | beats BM25, dense KNN, and RRF on the macro average |
| Contamination@k | benchmark data | ≤ RRF (lower is better) |
| Ground-truth objectivity | PR-mining + leakage check | structure-blind; circularity measured, not assumed |
| Sample size & breadth | benchmark log | ≥ 60 resolvable objective queries, ≥ 2 languages at scale |
| Reproducibility | benchmark log | reproduces from a fresh clone |
| Math soundness | CSAR theorem property tests | identities machine-verified to machine epsilon |

Until all six hold, the accurate description is "a local, mathematically-grounded
retrieval engine, with quality under active benchmarking" — public performance
claims should not outrun this bar.

## Pillar 2 — UX / performance (protects first-use experience)

Measured against the recorded reference run (psf/requests, 736 symbols, CPU):

| Criterion | Reference | Target / budget |
| --- | --- | --- |
| Time-to-first-lexical-result | seconds (Phase A) | search usable before embeddings finish |
| Embedding-progress moved | true (70→100 %, "X/N") | must stay true (never a static bar) |
| Embedding throughput | track per cycle | alert if symbols/sec drops > 20 % vs baseline |
| Server warm/startup (one-time) | track | the lever if we invest in startup |
| Steady-state semantic query (hot) | sub-second | p50 < 0.3 s |
| Cold-index per-phase split | parse/resolve/write balanced | flag if a non-embed phase regresses |

## Pillar 3 — Reliability / correctness (CI gates, must stay green)

| Criterion | Command | Gate |
| --- | --- | --- |
| Format | `cargo fmt --all --check` | clean |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Unit + property + parity | `cargo test --workspace` | 100 % pass (CSAR theorems T1–T5, FTS/vec/fusion/CSAR parity) |
| Differential + golden eval | `cargo test -p cognis-eval` | parity holds vs the checked-in oracle goldens |
| Coverage | `cargo llvm-cov --workspace` | `fail_under` ratchets up only, never down |
| Cross-app contracts | `tests/e2e/contracts/` ↔ `apps/cognis-vscode` `contractParity.test.ts` | extension ↔ CLI JSON shapes pinned (regenerate only on intentional change) |
| MCP tool output contract | mcpd contract checks | all 8 AI-facing tools keep the keys agents depend on — search/lookup/trace/resolve, hybrid `discover_symbols`, flagship `diffuse_context` (`on_path`/`ppr_score`), `retrieve_context_capsule` schema, error envelope |
| Contract version lockstep | handshake check | engine contract version == extension `EXPECTED_CONTRACT_VERSION` (bump both together); `cognis cli handshake` advertises the negotiated payload |
| Panel UI e2e | `npm run test:e2e` | all states pass; every button posts a valid command |
| Full-stack host e2e | `npm run test:host` (CI: `vscode-host-e2e`, xvfb) | the real extension in a real VS Code host, against the real `cognis` binary, runs `cognis.setupWorkspace` and writes a real `.cognis/config.yaml` + workspace `mcp.json`; the flow appears in `diagnostics.jsonl` |
| Flow tracing (bug trace) | extension (`diagnostics.test.ts`) | every user flow is reconstructable from `diagnostics.jsonl`: each progress-wrapped flow logs start/ok/fail+duration, start/stop MCP + connectMcp + handshake log explicitly, every surfaced error logs guidance, every CLI call logs exit+duration |

## Pillar 4 — Scaling / cost (protects large-repo viability)

Measured against a real large-repo index **and** against the MCP process/RAM
topology harness:

| Criterion | Reference | Gate |
| --- | --- | --- |
| DB bytes / symbol | track (requests) | flag superlinear growth |
| Cold-index wall time (large repo) | record per cycle | sub-linear vs symbol count |
| Memory / handle footprint (mcpd) | sustained tool load | real server stays resource-bounded: OS-handle and RSS growth stay flat over hundreds of calls (the leak fingerprint is a near-linear handle climb) |
| Process cardinality (`A`/`H`/`I`) | [`tests/e2e/private-bytes/`](../tests/e2e/private-bytes/README.md) | heavy repository daemons ≤ `A`; indexd ≤ 1 per repo and ≤ `I`; thin proxies ≤ `H` and model-free; no host × repository heavy fan-out |
| Idle aggregate private bytes (Windows authoritative) | same harness, ≥5 clean runs, isolated temp homes | **Target** median ≤ 0.615 GiB on an equivalent stabilized-idle reproduction of the recorded ~1.23 GiB multi-process snapshot **on the same machine/build/model/topology**; no run exceeds the ~1.23 GiB baseline; zero owned Cognis daemon/orphan processes after stop + grace period. **This is a target, not an achieved universal result.** |
| Active-load peak private bytes | same harness (`--active-load`) | reported **separately** from idle median; never substituted for the idle gate |

**Evidence discipline for RAM / process claims:**

- Label every published figure as **empirical** for named hardware, OS, build
  (git sha / version), model fingerprint, and topology (`A`, `H`, `I`, warm
  policy, stdio mode, sharing-gate state). Quote `n` (runs).
- Distinguish process cardinality, idle private bytes, active peak, model
  mappings, and run variance (preservation 3.11).
- Never present 0.615 GiB as “already achieved” without attaching a report from
  the private-bytes harness on the named machine/topology.
- Never touch the developer’s real `.cognis` or host MCP config during
  measurement (preservation 3.10).

Related operator docs: [mcp-client-config.md](mcp-client-config.md) (scope,
migration/rollback, eager/lazy, multi-host lifecycle),
[security.md](security.md) (loopback / credentials / fingerprints),
[e2e-testing.md](e2e-testing.md) § private-byte measurement,
[performance.md](performance.md) § process cardinality.


---

## The per-cycle loop

1. **Baseline** at cycle start: refresh the committed smoke baseline
   (`tests/e2e/baselines/sample.json`, used by CI for correctness). For a
   real-repo perf trend, diff against the committed large-repo reference
   (`tests/e2e/baselines/requests.json`).
2. **Develop**: every change should leave at least one of *correct / accurate /
   efficient* measurably better, or produce a sound negative result that
   prevents wasted effort.
3. **Gate** before merge: Pillar 3 green (`cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`); Pillar-2/4 invariants tracked against the
   baselines; Pillar 1 governs any new public claim.
4. **Record**: update the benchmark log (quality, labeled by evidence tier) and
   `CHANGELOG.md` (`[Unreleased]`).

## Automated regression gate

The eval harness (`cognis-eval`, run via `cargo test -p cognis-eval`) turns the
committed baselines into a gate:

- **Hard invariants** (hardware-independent flow correctness — embedding bar
  moves, semantic search returns hits, index has vectors, health ok, symbol
  search resolves) **fail the build**.
- **Soft perf budgets** (throughput, hot-query latency, warm time, DB
  bytes/symbol) are reported relative to the baseline and tracked for trend, so
  CI on variable hardware gates on correctness while perf is monitored.

Two committed reference baselines under `tests/e2e/baselines/`:
- `sample.json` — bundled sample repo; fast, hardware-independent invariants;
  used by CI.
- `requests.json` — large real repo (psf/requests, 736 symbols); the perf/scaling
  trend reference (run locally, machine-specific latency, soft budgets).

### Synthetic-golden eval gate (no-regression smoke, NOT a quality claim)

The eval harness also runs the hybrid eval over the synthetic fixture golden and
gates it against `eval-baselines/phase1.json`. This is a **regression smoke gate
only**: `phase1.json` records the *measured* Recall@k / MRR on that hand-authored
golden and fails only on a regression beyond `regression_tolerance`. It is
**not** an absolute quality bar and **not** a public claim — authoritative
retrieval quality is Pillar 1 (the `.benchmarks/` harness on objective PR-derived
truth). Refresh the baseline deliberately (record the new measured value, note
why) when retrieval changes on purpose.

## Practices to avoid

- Quoting a finite-sample number as if it were a general guarantee.
- Presenting concept-label recall as evidence of generalization.
- Omitting the contamination / regression / skip columns from a comparison.
- Lowering a coverage or quality gate to make a build pass.
- Public performance claims ahead of the Pillar-1 bar.
- Claiming MCP process/RAM “≤ 0.615 GiB” or “half the baseline” without a named
  machine/build/topology report from `tests/e2e/private-bytes/` and without
  separating idle median from active-load peak.
- Treating raw process count or RSS anecdotes as a substitute for
  platform-correct private-byte / process-tree measurement.


## Status (this release)

- Pillars 2 & 3: green and instrumented.
- Pillar 1: the **sample-size / breadth** criterion is met — the objective
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
- Pillar 4 process/RAM: the measurement procedure and harness exist under
  `tests/e2e/private-bytes/`. Defaults are workspace MCP scope, thin-proxy
  stdio, and sharing gate OFF. The ~1.23 GiB → median ≤ 0.615 GiB figure remains
  an **acceptance target** for equivalent stabilized-idle reproductions on a
  named machine/build/topology — **not** a result claimed as already achieved in
  this document. Attach harness reports before any release claim.
- The regression gate is built into the `cognis-eval` harness and the committed
  baselines, exercised by CI.
