# Full-flow coverage framework

A single command that runs the **entire** test suite — unit, integration,
property-based, and the cross-process e2e flow — under coverage, merges the
data (including code that runs inside spawned `cognis-cli` / `cognis-indexd` /
`cognis-mcpd` subprocesses), and reports exactly which lines/branches are still
uncovered. The goal is to develop against a measured target instead of testing
by hand.

## Run it

```bash
make coverage          # full suite under coverage + terminal report (gap list)
make coverage-html     # same, plus an HTML report under htmlcov/
```

or directly:

```bash
python scripts/coverage_full.py            # text report
python scripts/coverage_full.py --html     # + htmlcov/index.html
python scripts/coverage_full.py --fail-under 80
python scripts/coverage_full.py -m "unit or integration"   # subset by marker
```

## How subprocess coverage works

The e2e tests run the real apps over process boundaries. Normal coverage only
sees the in-process test code, so the runner:

1. sets `COVERAGE_PROCESS_START=<pyproject.toml>` (read by `coverage.process_startup`),
2. prepends `tests/coverage/` to `PYTHONPATH` so each child imports
   [`sitecustomize.py`](./sitecustomize.py) at startup and begins recording,
3. runs the suite with `coverage run --parallel-mode -m pytest`,
4. `coverage combine` merges every `.coverage.*` file (in-process + children),
5. `coverage report` / `coverage html` prints the gaps.

`[tool.coverage.paths]` in `pyproject.toml` collapses the same file seen from
different working directories so the merge lines up.

## Known limitation (Windows daemon)

`cognis-indexd` is stopped in e2e via `Popen.terminate()`. On POSIX that is a
catchable `SIGTERM` and `[tool.coverage.run] sigterm = true` flushes the child's
data. On **Windows**, `terminate()` is a hard `TerminateProcess` that cannot
flush, so the daemon's *subprocess* lines are not captured there. The daemon's
logic is still measured by the in-process tests (`tests/unit/test_indexd_daemon.py`,
`tests/integration/test_indexd_daemon.py`). CI (Linux) captures the daemon
subprocess directly.

## Reading the report

`show_missing = true` prints the uncovered line ranges per file. Drive the
number up by adding a focused test for each listed range; `fail_under` in
`pyproject.toml` is the CI gate — raise it as coverage grows so it never
regresses.
