"""Subprocess coverage bootstrap.

Python imports a module named ``sitecustomize`` automatically at interpreter
startup if one is importable. The full-flow coverage runner
(``scripts/coverage_full.py``) prepends *this directory* to ``PYTHONPATH`` and
sets ``COVERAGE_PROCESS_START`` for every child process the e2e harness spawns
(cognis-cli / cognis-indexd / cognis-mcpd). When both conditions hold, this
hook starts coverage in the child so its lines are measured too — without it,
only the in-process test code would be counted and the real app flow that runs
over process boundaries would show as uncovered.

It is a no-op unless ``COVERAGE_PROCESS_START`` is set, so it never affects
ordinary runs even if this directory leaks onto ``PYTHONPATH``.
"""

from __future__ import annotations

import os

if os.environ.get("COVERAGE_PROCESS_START"):
    try:
        import coverage

        coverage.process_startup()
    except Exception:  # pragma: no cover - coverage absent in some child envs
        pass
