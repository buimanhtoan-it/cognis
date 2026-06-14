# Eval Baseline Reference

This document describes the local eval baseline used as a **no-regression smoke
gate** on the synthetic fixture golden.

> **Scope (read first).** This is **not** a public quality claim and **not** an
> absolute quality bar. Authoritative retrieval quality is the `.benchmarks/`
> harness on objective, PR-derived ground truth (see
> [../development-criteria.md](../development-criteria.md), Pillar 1). The gate
> here only catches accidental regressions on the hand-authored golden.

## What this baseline covers

The baseline is built from fixture repositories and a curated golden query set.
It exists to answer two questions:

1. does retrieval return the expected symbols consistently?
2. do changes regress the *currently measured* Recall@k / MRR beyond tolerance?

CI compares local results against the recorded baseline in
[../../eval-baselines/phase1.json](../../eval-baselines/phase1.json). That file
records the **measured** `recall_at_k_baseline` / `mrr_baseline` and fails only
on a regression beyond `regression_tolerance` (default `0.05` absolute) — never
an aspirational minimum.

## Dataset summary

- total queries: 126 (synthetic, hand-authored concept labels)
- task modes covered: `bugfix`, `feature`, `refactor`, `explain`, `review`, `migrate`
- fixture repositories:
  - `mini-ts-app`
  - `mini-py-svc`
  - `mini-go-svc`
- golden query file: `tests/fixtures/eval/golden.jsonl`

## Metrics

Baselines are the measured values recorded in `eval-baselines/phase1.json`; the
gate fails only on a regression beyond `regression_tolerance`.

| Metric | Meaning | Recorded baseline |
| --- | --- | --- |
| Recall@10 | At least one expected symbol appears in the top 10 results | `0.5045` (no-regression, tol `0.05`) |
| MRR | Mean reciprocal rank of the first expected symbol | `0.3323` (no-regression, tol `0.05`) |
| Token efficiency | Relevant tokens divided by total capsule tokens | informational |
| Latency p95 | Retrieval and capsule latency | see [../performance.md](../performance.md) |

## Running the eval locally

### 1. Index the fixture repositories

Linux / macOS:

```bash
bash scripts/ci_index_fixtures.sh
```

Windows PowerShell:

```powershell
.\scripts\ci_index_fixtures.ps1
```

### 2. Run the eval harness

```bash
python scripts/run_eval.py --golden tests/fixtures/eval/golden.jsonl --k 10
```

### 3. Review the generated report

Local reports are written under:

```text
eval-reports/<timestamp>/
```

### 4. Compare against the baseline

```bash
python scripts/compare_eval_baseline.py eval-reports/<timestamp>/report.json
```

## Interpreting the results

When the baseline regresses, check:

- whether the expected symbols still exist in the indexed fixtures
- whether lexical, semantic, or structural retrieval changed behavior
- whether result weighting or query rewriting changed
- whether capsule truncation is removing relevant evidence

## Refreshing the baseline

Refresh `eval-baselines/phase1.json` **deliberately** when retrieval changes on
purpose: record the new measured Recall@k / MRR from a CI run and note why in the
commit. Never bump it silently just to make a build pass.

## Tuning areas

When recall is lower than expected, the most common areas to review are:

- planner layer weighting
- lexical query rewriting
- result limits before composition
- semantic availability and embedding quality

## Related evaluation

For the manual SWE-bench process, see
[swe-bench-methodology.md](swe-bench-methodology.md).

## Release checklist items backed by this baseline

Use this baseline to help confirm:

- retrieval recall remains above the accepted floor
- the MCP tool surface still behaves correctly with indexed fixture data
- cross-language indexing remains functional
- performance targets have not regressed without explanation
