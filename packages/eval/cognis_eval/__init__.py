"""Eval harness — golden-set runner and metrics (task 4 of tasks.md).

Public surface:

- :class:`GoldenQuery`, :class:`EvalReport`, :class:`QueryResult`,
  :data:`TaskMode` — Pydantic schemas (see :mod:`cognis_eval.models`).
- :class:`RetrievalStrategy`, :class:`NullStrategy` — pluggable retrieval
  contract used by the runner (see :mod:`cognis_eval.runner`).
- :func:`run_eval`, :func:`load_golden_set`, :func:`recall_at_k`,
  :func:`reciprocal_rank`, :func:`render_markdown`, :func:`write_report`,
  :func:`prepare_out_dir` — runner entry points and helpers.
"""

from __future__ import annotations

from cognis_eval.models import (
    EvalReport,
    GoldenQuery,
    QueryResult,
    TaskMode,
)
from cognis_eval.runner import (
    CAPSULE_PLACEHOLDER_NOTE,
    DEFAULT_K,
    RECALL_UNDEFINED_NOTE,
    NullStrategy,
    RetrievalStrategy,
    load_golden_set,
    prepare_out_dir,
    recall_at_k,
    reciprocal_rank,
    render_markdown,
    run_eval,
    write_report,
)
from cognis_eval.strategy import HybridStrategy, strategy_from_env

__all__ = [
    "CAPSULE_PLACEHOLDER_NOTE",
    "DEFAULT_K",
    "RECALL_UNDEFINED_NOTE",
    "EvalReport",
    "GoldenQuery",
    "HybridStrategy",
    "NullStrategy",
    "QueryResult",
    "RetrievalStrategy",
    "TaskMode",
    "load_golden_set",
    "prepare_out_dir",
    "recall_at_k",
    "reciprocal_rank",
    "render_markdown",
    "run_eval",
    "strategy_from_env",
    "write_report",
]
