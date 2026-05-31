# Eval Baseline Reference

This document describes the local eval baseline used to measure retrieval
quality and release readiness.

## What this baseline covers

The baseline is built from fixture repositories and a curated golden query set.
It exists to answer two questions:

1. does retrieval return the expected symbols consistently?
2. do changes regress the current quality threshold?

CI compares local results against the thresholds in
[../../eval-baselines/phase1.json](../../eval-baselines/phase1.json).

## Dataset summary

- total queries: 110
- task modes covered: `bugfix`, `feature`, `refactor`, `explain`, `review`, `migrate`
- fixture repositories:
  - `mini-ts-app`
  - `mini-py-svc`
  - `mini-go-svc`
- golden query file: `tests/fixtures/eval/golden.jsonl`

## Metrics

| Metric | Meaning | Current threshold |
| --- | --- | --- |
| Recall@10 | At least one expected symbol appears in the top 10 results | `>= 0.70` |
| MRR | Mean reciprocal rank of the first expected symbol | informational |
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

## Current placeholder status

The measured tables in this document are still placeholders until updated from a
real eval run. Replace placeholder values with current outputs before using this
document as a release sign-off artifact.

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
