# SWE-bench Lite Methodology

This document describes the manual procedure for comparing a baseline coding
workflow against the same workflow with `cognis` available through MCP.

This evaluation is not part of normal CI because it depends on external
services, live model access, and repository-specific test execution.

## Goal

The purpose of this evaluation is to measure whether `cognis` helps an agent
solve real bug-fix tasks more effectively than a baseline workflow without
`cognis`.

## Prerequisites

- access to the [SWE-bench Lite](https://www.swebench.com/) dataset
- a working Claude Code setup with `cognis` configured
- a Python environment with the `datasets` package installed
- local checkouts of the repositories selected for evaluation

## Step 1: select the issue set

Choose 20 issues from SWE-bench Lite that are representative of bug-fix work.

Recommended selection criteria:

- clear failure symptoms
- coverage across multiple repositories
- no reliance on external services or infrastructure-heavy setup

Record the selected issue ids in a local file under `eval-reports/`.

## Step 2: prepare the repositories

For each repository referenced by the selected issues:

1. clone the repository
2. check out the issue's base commit
3. create and populate a `cognis` index for that repository

Example:

```bash
git clone https://github.com/<org>/<repo> /tmp/swe-bench-repos/<repo>
cd /tmp/swe-bench-repos/<repo>
git checkout <base_commit>

COGNIS_DB_PATH=/tmp/swe-bench-<repo>.db cognis-cli init
COGNIS_DB_PATH=/tmp/swe-bench-<repo>.db cognis-cli index --full .
```

## Step 3: configure the agent

Configure Claude Code so that it can launch `cognis-mcpd` with the appropriate
`COGNIS_DB_PATH` for the repository under test.

See [../mcp-client-config.md](../mcp-client-config.md) for concrete examples.

## Step 4: run both variants

For each issue, perform two runs:

### Baseline run

- disable `cognis`
- ask the agent to fix the issue using the same prompt template
- record patch correctness, time to useful patch, and tool usage

### `cognis`-enabled run

- enable `cognis`
- repeat the same task
- record the same measurements

Try to keep all other conditions unchanged, including model selection and prompt
framing.

## Step 5: score the result

Apply the generated patch and run the relevant tests:

```bash
cd /tmp/swe-bench-repos/<repo>
git apply /tmp/patch-<issue_id>.diff
python -m pytest tests/<relevant_test_file> -x
```

Count a patch as correct only when the failing tests for the issue pass after
the patch is applied.

## Step 6: summarize the results

Recommended summary metrics:

| Metric | Meaning |
| --- | --- |
| Resolution rate | Correct patches divided by total issues |
| Relative improvement | Difference between `cognis` and baseline resolution rates |
| Mean tool calls | Average `cognis` tool usage on resolved issues |
| Time to patch | Median time from task start to usable patch |

Write the results to a local report under `eval-reports/`.

## Known limitations

- The procedure is semi-manual.
- Different repositories require different test environments.
- Agent behavior may vary with model version and prompt changes.
- Results depend on indexing quality for the selected repository state.

## References

- [SWE-bench paper](https://arxiv.org/abs/2310.06770)
- [SWE-bench Lite dataset](https://huggingface.co/datasets/princeton-nlp/SWE-bench_Lite)
