"""Pydantic schemas for the eval harness — task 4 of ``.kiro/specs/cognis/tasks.md``.

Two schemas live here:

- :class:`GoldenQuery` — one record from the golden-set JSONL, defining a
  natural-language ``task`` plus its expected retrieval ground truth.
- :class:`EvalReport` — the top-level report emitted by
  :func:`cognis_eval.runner.run_eval` (json + markdown). Includes per-query
  :class:`QueryResult` rows and the aggregate metrics (Recall@k, MRR,
  capsule token efficiency).

Schemas are ``frozen=True`` so they're hashable and safe to share across the
runner / CLI / report-renderer call sites without defensive copies.

The ``task_mode_expected`` literal mirrors the planner classifier surface
(``bugfix | feature | refactor | explain | review | migrate``).
"""

from __future__ import annotations

from typing import Final, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

# ---------------------------------------------------------------------------
# Type aliases
# ---------------------------------------------------------------------------

TaskMode = Literal[
    "bugfix",
    "feature",
    "refactor",
    "explain",
    "review",
    "migrate",
]
"""Allowed values for :attr:`GoldenQuery.task_mode_expected`."""


# Reused ConfigDict for every model in this module.
_FROZEN_MODEL_CONFIG: Final[ConfigDict] = ConfigDict(
    extra="forbid",
    validate_assignment=True,
    frozen=True,
)


# ---------------------------------------------------------------------------
# GoldenQuery
# ---------------------------------------------------------------------------


class GoldenQuery(BaseModel):
    """One row of the golden-set JSONL.

    The format is locked by task 4.1::

        {"id", "task", "task_mode_expected",
         "expected_symbol_ids": [...],
         "expected_call_chain": [...] | null}
    """

    model_config = _FROZEN_MODEL_CONFIG

    id: str = Field(min_length=1, max_length=128)
    """Stable id for the query — used as a key in eval reports."""

    task: str = Field(min_length=1, max_length=4096)
    """Natural-language task string passed to the retrieval strategy."""

    task_mode_expected: TaskMode
    """Ground-truth task mode the planner should classify this query as."""

    expected_symbol_ids: list[str] = Field(default_factory=list)
    """Set of relevant SymbolNode ids (used by Recall@k / MRR)."""

    expected_call_chain: list[str] | None = None
    """Optional ordered call chain ``["caller", "callee", ...]`` for structural eval."""

    @field_validator("id", "task")
    @classmethod
    def _strip_non_empty(cls, value: str) -> str:
        """Reject pure-whitespace ``id`` / ``task`` (Pydantic's min_length lets " " through)."""
        if not value.strip():
            raise ValueError("must not be blank")
        return value

    @field_validator("expected_symbol_ids")
    @classmethod
    def _ids_are_non_empty_strings(cls, value: list[str]) -> list[str]:
        for item in value:
            if not isinstance(item, str) or not item.strip():
                raise ValueError("expected_symbol_ids entries must be non-empty strings")
        return value

    @field_validator("expected_call_chain")
    @classmethod
    def _call_chain_entries_non_empty(cls, value: list[str] | None) -> list[str] | None:
        if value is None:
            return None
        for item in value:
            if not isinstance(item, str) or not item.strip():
                raise ValueError("expected_call_chain entries must be non-empty strings")
        return value


# ---------------------------------------------------------------------------
# QueryResult — per-query slice of the report
# ---------------------------------------------------------------------------


class QueryResult(BaseModel):
    """Per-query metrics row in :class:`EvalReport`."""

    model_config = _FROZEN_MODEL_CONFIG

    id: str
    task: str
    task_mode_expected: TaskMode
    expected_symbol_ids: list[str]
    retrieved_symbol_ids: list[str]
    """Top-k symbol ids returned by the strategy (already truncated to ``k``)."""

    recall_at_k: float = Field(ge=0.0, le=1.0)
    reciprocal_rank: float = Field(ge=0.0, le=1.0)
    notes: list[str] = Field(default_factory=list)
    """Free-form per-query notes — e.g. "recall undefined: empty expected set"."""


# ---------------------------------------------------------------------------
# EvalReport — top-level report
# ---------------------------------------------------------------------------


class EvalReport(BaseModel):
    """Top-level eval report serialized to ``report.json`` + ``summary.md``.

    Aggregate metrics use the simple mean over ``queries[]``. Capsule token
    efficiency is a placeholder at MVP because the capsule composer doesn't
    land until task 14; the report carries an explicit ``..._note`` so a
    consumer can tell "real 0.0" apart from "not measured yet".
    """

    model_config = _FROZEN_MODEL_CONFIG

    schema_version: Literal["1"] = "1"
    """Eval report schema version — bump when a breaking field shape lands."""

    generated_at: str
    """ISO-8601 UTC timestamp string (``YYYY-MM-DDTHH:MM:SSZ``)."""

    runtime_version: str
    """``cognis.__version__`` at report time, for traceability."""

    strategy: str
    """Name of the :class:`RetrievalStrategy` used (e.g. ``"null"``)."""

    k: int = Field(ge=1)
    num_queries: int = Field(ge=0)

    recall_at_k: float = Field(ge=0.0, le=1.0)
    """Mean Recall@k across all queries."""

    mrr: float = Field(ge=0.0, le=1.0)
    """Mean Reciprocal Rank across all queries."""

    capsule_token_efficiency: float = Field(ge=0.0)
    """Ratio of relevant tokens to total tokens in composed capsules.

    Placeholder 0.0 at MVP — real measurement lands with the capsule composer.
    """

    capsule_token_efficiency_note: str | None = None
    """Caveat string when ``capsule_token_efficiency`` is a placeholder."""

    queries: list[QueryResult] = Field(default_factory=list)


__all__ = [
    "EvalReport",
    "GoldenQuery",
    "QueryResult",
    "TaskMode",
]
