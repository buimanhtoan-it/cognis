"""Golden-set runner — task 4.2 of ``.kiro/specs/cognis/tasks.md``.

Reads a JSONL golden set, runs each :class:`~cognis_eval.models.GoldenQuery`
through a pluggable :class:`RetrievalStrategy`, and computes Recall@k, MRR,
and a placeholder capsule token-efficiency metric. Emits two artifacts:

- ``report.json`` — :class:`~cognis_eval.models.EvalReport` serialized.
- ``summary.md`` — human-readable Markdown summary used in CI job summaries.

Retrieval is deliberately pluggable so the harness is decoupled from the
indexer pipeline (which lands in tasks 6+). At MVP the only strategy is
:class:`NullStrategy`, which returns no hits and lets the harness be smoke-
tested without any backing store. Real strategies (lexical / semantic /
structural) wire up in task 12.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from datetime import UTC, datetime
from pathlib import Path
from typing import Final, Protocol, runtime_checkable

from cognis import __version__

from cognis_eval.models import EvalReport, GoldenQuery, QueryResult

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_K: Final[int] = 10
"""Default top-K used by Recall@k when the caller doesn't override it."""

#: Note attached to ``EvalReport.capsule_token_efficiency`` until task 14.
CAPSULE_PLACEHOLDER_NOTE: Final[str] = (
    "TODO: capsule composer lands in task 14 of .kiro/specs/cognis/tasks.md; "
    "capsule_token_efficiency reported as 0.0 until then."
)

#: Note attached to a per-query :class:`QueryResult` when recall is undefined.
RECALL_UNDEFINED_NOTE: Final[str] = (
    "recall_at_k undefined for empty expected_symbol_ids; reported as 0.0"
)


# ---------------------------------------------------------------------------
# RetrievalStrategy protocol
# ---------------------------------------------------------------------------


@runtime_checkable
class RetrievalStrategy(Protocol):
    """Pluggable retrieval contract used by the eval runner.

    ``name`` is a stable identifier surfaced in the report (e.g. ``"null"``,
    ``"lexical"``, ``"hybrid"``). Real strategies implementing the layers in
    design *Retrieval Mesh* arrive in task 12.
    """

    name: str

    def retrieve(self, query: str, k: int) -> list[str]:  # pragma: no cover - protocol
        """Return up to ``k`` symbol ids ranked best-first for ``query``."""
        ...


class NullStrategy:
    """Placeholder strategy that returns no hits.

    Lets the eval harness be smoke-tested before the real retrieval layers
    exist (task 12). Recall@k will always be 0.0 and MRR 0.0; the runner
    still emits a well-formed report so CI wiring (task 4.5) is exercisable.
    """

    name: str = "null"

    def retrieve(self, query: str, k: int) -> list[str]:
        del query, k  # unused — placeholder
        return []


# ---------------------------------------------------------------------------
# JSONL loader
# ---------------------------------------------------------------------------


def load_golden_set(path: str | Path) -> list[GoldenQuery]:
    """Load a JSONL golden set from ``path`` and validate every record.

    Rules:

    - Blank lines are skipped.
    - Lines starting with ``#`` (after whitespace strip) are treated as comments.
    - Each remaining line MUST parse to a JSON object validating against
      :class:`GoldenQuery`.

    Raises:
        FileNotFoundError: if ``path`` does not exist.
        ValueError: if any non-blank, non-comment line fails to parse or
            validate. The error message includes the 1-based line number.
    """
    golden_path = Path(path)
    if not golden_path.exists():
        raise FileNotFoundError(f"golden set not found: {golden_path}")

    queries: list[GoldenQuery] = []
    with golden_path.open(encoding="utf-8") as handle:
        for lineno, raw in enumerate(handle, start=1):
            stripped = raw.strip()
            if not stripped:
                continue
            if stripped.startswith("#"):
                continue
            try:
                payload = json.loads(stripped)
            except json.JSONDecodeError as exc:
                raise ValueError(f"{golden_path}:{lineno}: invalid JSON ({exc.msg})") from exc
            if not isinstance(payload, dict):
                raise ValueError(
                    f"{golden_path}:{lineno}: expected JSON object, got {type(payload).__name__}"
                )
            try:
                queries.append(GoldenQuery.model_validate(payload))
            except Exception as exc:  # pydantic.ValidationError or sub-validators
                raise ValueError(f"{golden_path}:{lineno}: invalid GoldenQuery: {exc}") from exc
    return queries


# ---------------------------------------------------------------------------
# Metric primitives
# ---------------------------------------------------------------------------


def symbol_stem(symbol_id: str) -> str:
    """Return ``lang:path:name`` without the ``@content_hash`` suffix."""
    at = symbol_id.rfind("@")
    if at > 0:
        return symbol_id[:at]
    return symbol_id


def _matches_expected(retrieved_id: str, expected: Iterable[str]) -> bool:
    """True when *retrieved_id* equals an expected id or shares the same stem."""
    expected_list = list(expected)
    if retrieved_id in expected_list:
        return True
    stem = symbol_stem(retrieved_id)
    return any(symbol_stem(exp) == stem for exp in expected_list)


def recall_at_k(retrieved: Iterable[str], expected: Iterable[str], k: int) -> float:
    """Return ``Recall@k`` for one query.

    Defined as ``|expected ∩ retrieved[:k]| / |expected|``. When ``expected``
    is empty the metric is mathematically undefined; we return 0.0 and the
    caller is responsible for surfacing a "note" to the user.

    Args:
        retrieved: Ranked symbol ids returned by the strategy.
        expected: Ground-truth symbol ids.
        k: Cutoff rank — only the first ``k`` retrieved ids count.

    Raises:
        ValueError: if ``k < 1``.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    expected_set = set(expected)
    if not expected_set:
        return 0.0
    top_k = list(retrieved)[:k]
    hits = sum(1 for sid in top_k if _matches_expected(sid, expected_set))
    return hits / len(expected_set)


def reciprocal_rank(retrieved: Iterable[str], expected: Iterable[str]) -> float:
    """Return reciprocal rank of the first relevant hit (0.0 if none).

    Standard MRR per-query term. When ``expected`` is empty there is no
    relevant hit possible, so we return 0.0.
    """
    expected_set = set(expected)
    if not expected_set:
        return 0.0
    for index, sid in enumerate(retrieved, start=1):
        if _matches_expected(sid, expected_set):
            return 1.0 / index
    return 0.0


def _mean(values: list[float]) -> float:
    """Arithmetic mean — empty list returns 0.0 to keep the report well-typed."""
    if not values:
        return 0.0
    return sum(values) / len(values)


# ---------------------------------------------------------------------------
# Output formatting
# ---------------------------------------------------------------------------


def _utc_timestamp_dirname(now: datetime | None = None) -> str:
    """Return a directory-safe UTC timestamp like ``20240115T123045Z``."""
    moment = now if now is not None else datetime.now(UTC)
    return moment.strftime("%Y%m%dT%H%M%SZ")


def _iso_utc(now: datetime | None = None) -> str:
    """Return an ISO-8601 UTC string like ``2024-01-15T12:30:45Z``."""
    moment = now if now is not None else datetime.now(UTC)
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


def render_markdown(report: EvalReport) -> str:
    """Render an :class:`EvalReport` as a human-readable Markdown summary."""
    lines: list[str] = []
    lines.append("# cognis eval report")
    lines.append("")
    lines.append(f"- generated at: `{report.generated_at}`")
    lines.append(f"- runtime version: `{report.runtime_version}`")
    lines.append(f"- strategy: `{report.strategy}`")
    lines.append(f"- k: `{report.k}`")
    lines.append(f"- queries: `{report.num_queries}`")
    lines.append("")
    lines.append("## Aggregate metrics")
    lines.append("")
    lines.append("| metric | value |")
    lines.append("| --- | --- |")
    lines.append(f"| Recall@{report.k} | {report.recall_at_k:.4f} |")
    lines.append(f"| MRR | {report.mrr:.4f} |")
    lines.append(f"| Capsule token efficiency | {report.capsule_token_efficiency:.4f} |")
    if report.capsule_token_efficiency_note:
        lines.append("")
        lines.append(f"> {report.capsule_token_efficiency_note}")
    lines.append("")
    lines.append("## Per-query results")
    lines.append("")
    if not report.queries:
        lines.append("_No queries evaluated._")
        lines.append("")
        return "\n".join(lines)
    lines.append("| id | mode | recall | rr | retrieved | expected |")
    lines.append("| --- | --- | --- | --- | --- | --- |")
    for q in report.queries:
        lines.append(
            f"| `{q.id}` "
            f"| {q.task_mode_expected} "
            f"| {q.recall_at_k:.4f} "
            f"| {q.reciprocal_rank:.4f} "
            f"| {len(q.retrieved_symbol_ids)} "
            f"| {len(q.expected_symbol_ids)} |"
        )
    notes_rows = [(q.id, note) for q in report.queries for note in q.notes]
    if notes_rows:
        lines.append("")
        lines.append("### Notes")
        lines.append("")
        for qid, note in notes_rows:
            lines.append(f"- `{qid}`: {note}")
    lines.append("")
    return "\n".join(lines)


def write_report(report: EvalReport, out_dir: str | Path) -> Path:
    """Persist the report to ``out_dir`` as ``report.json`` + ``summary.md``.

    Returns the resolved ``out_dir`` :class:`~pathlib.Path`. Parent dirs are
    created on demand. ``out_dir`` is *not* timestamped here — the caller
    decides whether to nest under ``eval-reports/<timestamp>/``.
    """
    out_path = Path(out_dir)
    out_path.mkdir(parents=True, exist_ok=True)

    json_path = out_path / "report.json"
    json_path.write_text(
        json.dumps(report.model_dump(mode="json"), indent=2, sort_keys=True),
        encoding="utf-8",
    )

    md_path = out_path / "summary.md"
    md_path.write_text(render_markdown(report), encoding="utf-8")

    return out_path


# ---------------------------------------------------------------------------
# Top-level runner
# ---------------------------------------------------------------------------


def run_eval(
    queries_path: str | Path,
    out_dir: str | Path,
    *,
    k: int = DEFAULT_K,
    strategy: RetrievalStrategy | None = None,
    now: datetime | None = None,
) -> tuple[EvalReport, Path]:
    """Run the eval harness end-to-end.

    Args:
        queries_path: JSONL file holding :class:`GoldenQuery` records.
        out_dir: Directory the runner writes ``report.json`` + ``summary.md``
            into. Will be created if absent. Callers typically pass
            ``eval-reports/<UTC_TIMESTAMP>/`` (see :func:`prepare_out_dir`).
        k: Cutoff rank for Recall@k (default :data:`DEFAULT_K`).
        strategy: Retrieval strategy to evaluate. Defaults to
            :class:`NullStrategy` for the smoke-test path.
        now: Optional datetime override (used by tests for deterministic
            timestamps).

    Returns:
        ``(report, out_path)`` where ``report`` is the typed
        :class:`EvalReport` and ``out_path`` is the resolved directory the
        report was written to.

    Raises:
        ValueError: when ``k < 1``.
        FileNotFoundError: when ``queries_path`` does not exist.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")

    if strategy is not None:
        active_strategy: RetrievalStrategy = strategy
    else:
        from cognis_eval.strategy import strategy_from_env

        live = strategy_from_env()
        active_strategy = live if live is not None else NullStrategy()
    queries = load_golden_set(queries_path)

    results: list[QueryResult] = []
    recalls: list[float] = []
    rrs: list[float] = []

    for query in queries:
        retrieved = list(active_strategy.retrieve(query.task, k))
        truncated = retrieved[:k]
        notes: list[str] = []
        if not query.expected_symbol_ids:
            notes.append(RECALL_UNDEFINED_NOTE)
        recall = recall_at_k(truncated, query.expected_symbol_ids, k)
        rr = reciprocal_rank(truncated, query.expected_symbol_ids)
        results.append(
            QueryResult(
                id=query.id,
                task=query.task,
                task_mode_expected=query.task_mode_expected,
                expected_symbol_ids=list(query.expected_symbol_ids),
                retrieved_symbol_ids=truncated,
                recall_at_k=recall,
                reciprocal_rank=rr,
                notes=notes,
            )
        )
        recalls.append(recall)
        rrs.append(rr)

    report = EvalReport(
        generated_at=_iso_utc(now),
        runtime_version=__version__,
        strategy=active_strategy.name,
        k=k,
        num_queries=len(queries),
        recall_at_k=_mean(recalls),
        mrr=_mean(rrs),
        capsule_token_efficiency=0.0,
        capsule_token_efficiency_note=CAPSULE_PLACEHOLDER_NOTE,
        queries=results,
    )

    out_path = write_report(report, out_dir)
    return report, out_path


def prepare_out_dir(base: str | Path, *, now: datetime | None = None) -> Path:
    """Return ``<base>/<UTC_TIMESTAMP>/`` (created on demand).

    Used by :func:`cognis.cli.main.cmd_eval` so each invocation gets its own
    timestamped subdirectory under ``eval-reports/``.
    """
    nested = Path(base) / _utc_timestamp_dirname(now)
    nested.mkdir(parents=True, exist_ok=True)
    return nested


__all__ = [
    "CAPSULE_PLACEHOLDER_NOTE",
    "DEFAULT_K",
    "RECALL_UNDEFINED_NOTE",
    "NullStrategy",
    "RetrievalStrategy",
    "load_golden_set",
    "prepare_out_dir",
    "recall_at_k",
    "reciprocal_rank",
    "render_markdown",
    "run_eval",
    "write_report",
]
