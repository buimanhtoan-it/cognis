"""Unit tests for ``cognis_eval`` — task 4 of ``.kiro/specs/cognis/tasks.md``.

Covers:

- :class:`GoldenQuery` validation (required fields, blank-string rejection,
  literal enforcement, optional ``expected_call_chain``).
- JSONL loader (skip blank lines, skip ``#`` comments, error reporting with
  line numbers).
- :func:`recall_at_k` math (basic case, k cutoff, perfect hit, no-hit, empty
  expected → 0.0 + per-query note).
- :func:`reciprocal_rank` math (rank 1, rank 3, no hit, empty expected).
- End-to-end :func:`run_eval` with :class:`NullStrategy` producing
  ``report.json`` + ``summary.md``.
- :func:`prepare_out_dir` materializes ``<base>/<UTC>/`` with a deterministic
  timestamp when ``now`` is supplied.
- The seed fixture under ``tests/fixtures/eval/golden.jsonl`` parses cleanly
  and covers all 6 task modes (task 4.4 contract).
"""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, get_args

import pytest
from cognis_eval.models import EvalReport, GoldenQuery, TaskMode
from cognis_eval.runner import (
    CAPSULE_PLACEHOLDER_NOTE,
    DEFAULT_K,
    RECALL_UNDEFINED_NOTE,
    NullStrategy,
    load_golden_set,
    prepare_out_dir,
    recall_at_k,
    reciprocal_rank,
    render_markdown,
    run_eval,
)
from cognis_eval.strategy import HybridStrategy
from pydantic import ValidationError

if TYPE_CHECKING:
    from cognis_eval.runner import RetrievalStrategy

REPO_ROOT: Path = Path(__file__).resolve().parents[2]
SEED_GOLDEN: Path = REPO_ROOT / "tests" / "fixtures" / "eval" / "golden.jsonl"


# ---------------------------------------------------------------------------
# GoldenQuery validation
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_golden_query_minimal_record_validates() -> None:
    q = GoldenQuery(
        id="q1",
        task="why does login fail?",
        task_mode_expected="bugfix",
        expected_symbol_ids=["py:src/auth.py:login@abc123"],
    )
    assert q.id == "q1"
    assert q.task_mode_expected == "bugfix"
    assert q.expected_call_chain is None


@pytest.mark.unit
def test_golden_query_with_call_chain_validates() -> None:
    q = GoldenQuery(
        id="q2",
        task="trace the login path",
        task_mode_expected="bugfix",
        expected_symbol_ids=["a", "b"],
        expected_call_chain=["a", "b"],
    )
    assert q.expected_call_chain == ["a", "b"]


@pytest.mark.unit
def test_golden_query_rejects_blank_id() -> None:
    with pytest.raises(ValidationError):
        GoldenQuery(
            id="   ",
            task="t",
            task_mode_expected="bugfix",
            expected_symbol_ids=[],
        )


@pytest.mark.unit
def test_golden_query_rejects_blank_task() -> None:
    with pytest.raises(ValidationError):
        GoldenQuery(
            id="q",
            task="",
            task_mode_expected="feature",
            expected_symbol_ids=[],
        )


@pytest.mark.unit
def test_golden_query_rejects_unknown_mode() -> None:
    with pytest.raises(ValidationError):
        GoldenQuery.model_validate(
            {
                "id": "q",
                "task": "t",
                "task_mode_expected": "chore",  # not in the eval literal
                "expected_symbol_ids": [],
            }
        )


@pytest.mark.unit
def test_golden_query_rejects_extra_fields() -> None:
    with pytest.raises(ValidationError):
        GoldenQuery.model_validate(
            {
                "id": "q",
                "task": "t",
                "task_mode_expected": "feature",
                "expected_symbol_ids": [],
                "unexpected_extra": True,
            }
        )


@pytest.mark.unit
def test_golden_query_rejects_blank_expected_symbol() -> None:
    with pytest.raises(ValidationError):
        GoldenQuery(
            id="q",
            task="t",
            task_mode_expected="feature",
            expected_symbol_ids=["", "valid:id@1234"],
        )


# ---------------------------------------------------------------------------
# JSONL loader
# ---------------------------------------------------------------------------


def _write_jsonl(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


@pytest.mark.unit
def test_load_golden_set_skips_blank_and_comment_lines(tmp_path: Path) -> None:
    p = tmp_path / "g.jsonl"
    _write_jsonl(
        p,
        [
            "# header comment",
            "",
            json.dumps(
                {
                    "id": "q1",
                    "task": "do thing",
                    "task_mode_expected": "feature",
                    "expected_symbol_ids": ["a"],
                }
            ),
            "   ",
            "  # indented comment",
            json.dumps(
                {
                    "id": "q2",
                    "task": "explain auth",
                    "task_mode_expected": "explain",
                    "expected_symbol_ids": [],
                    "expected_call_chain": None,
                }
            ),
        ],
    )
    queries = load_golden_set(p)
    assert [q.id for q in queries] == ["q1", "q2"]


@pytest.mark.unit
def test_load_golden_set_missing_file_raises(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        load_golden_set(tmp_path / "missing.jsonl")


@pytest.mark.unit
def test_load_golden_set_invalid_json_reports_lineno(tmp_path: Path) -> None:
    p = tmp_path / "g.jsonl"
    _write_jsonl(
        p,
        [
            json.dumps(
                {
                    "id": "ok",
                    "task": "ok",
                    "task_mode_expected": "feature",
                    "expected_symbol_ids": [],
                }
            ),
            "{not json",
        ],
    )
    with pytest.raises(ValueError, match=":2:"):
        load_golden_set(p)


@pytest.mark.unit
def test_load_golden_set_invalid_record_reports_lineno(tmp_path: Path) -> None:
    p = tmp_path / "g.jsonl"
    _write_jsonl(
        p,
        [
            json.dumps(
                {
                    "id": "q1",
                    "task": "t",
                    "task_mode_expected": "not_a_mode",
                    "expected_symbol_ids": [],
                }
            ),
        ],
    )
    with pytest.raises(ValueError, match=":1:"):
        load_golden_set(p)


@pytest.mark.unit
def test_load_golden_set_rejects_non_object_line(tmp_path: Path) -> None:
    p = tmp_path / "g.jsonl"
    _write_jsonl(p, ["[1, 2, 3]"])
    with pytest.raises(ValueError, match="expected JSON object"):
        load_golden_set(p)


# ---------------------------------------------------------------------------
# Metric primitives
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_recall_at_k_stem_match_ignores_content_hash() -> None:
    """Golden fixtures use placeholder hashes; indexed ids use real content hashes."""
    assert recall_at_k(
        ["ts:src/auth/jwt.ts:validate@1a2b3c4d"],
        ["ts:src/auth/jwt.ts:validate@deadbeef"],
        k=10,
    ) == pytest.approx(1.0)


def test_recall_at_k_basic_partial_hit() -> None:
    # 1 of 2 expected found → 0.5
    assert recall_at_k(["a", "x"], ["a", "b"], k=10) == pytest.approx(0.5)


@pytest.mark.unit
def test_recall_at_k_perfect_hit() -> None:
    assert recall_at_k(["a", "b", "c"], ["a", "b"], k=10) == pytest.approx(1.0)


@pytest.mark.unit
def test_recall_at_k_truncates_to_k() -> None:
    # The relevant 'a' sits at rank 5; with k=4 it shouldn't count.
    assert recall_at_k(["x", "y", "z", "w", "a"], ["a"], k=4) == 0.0
    assert recall_at_k(["x", "y", "z", "w", "a"], ["a"], k=5) == pytest.approx(1.0)


@pytest.mark.unit
def test_recall_at_k_no_overlap() -> None:
    assert recall_at_k(["x", "y"], ["a", "b"], k=10) == 0.0


@pytest.mark.unit
def test_recall_at_k_empty_expected_returns_zero() -> None:
    # Mathematically undefined; runner reports 0.0 + a note (see RECALL_UNDEFINED_NOTE).
    assert recall_at_k(["a", "b"], [], k=10) == 0.0


@pytest.mark.unit
def test_recall_at_k_rejects_zero_k() -> None:
    with pytest.raises(ValueError):
        recall_at_k(["a"], ["a"], k=0)


@pytest.mark.unit
def test_reciprocal_rank_first_position() -> None:
    assert reciprocal_rank(["a", "b"], ["a"]) == pytest.approx(1.0)


@pytest.mark.unit
def test_reciprocal_rank_third_position() -> None:
    assert reciprocal_rank(["x", "y", "a"], ["a"]) == pytest.approx(1 / 3)


@pytest.mark.unit
def test_reciprocal_rank_no_hit() -> None:
    assert reciprocal_rank(["x", "y"], ["a"]) == 0.0


@pytest.mark.unit
def test_reciprocal_rank_empty_expected() -> None:
    assert reciprocal_rank(["a", "b"], []) == 0.0


# ---------------------------------------------------------------------------
# End-to-end run_eval with NullStrategy
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_run_eval_with_null_strategy_produces_report(tmp_path: Path) -> None:
    queries_path = tmp_path / "g.jsonl"
    _write_jsonl(
        queries_path,
        [
            json.dumps(
                {
                    "id": "q1",
                    "task": "find bug",
                    "task_mode_expected": "bugfix",
                    "expected_symbol_ids": ["a", "b"],
                }
            ),
            json.dumps(
                {
                    "id": "q2",
                    "task": "explain arch",
                    "task_mode_expected": "explain",
                    "expected_symbol_ids": [],
                }
            ),
        ],
    )
    out_dir = tmp_path / "out"
    fixed_now = datetime(2024, 1, 15, 12, 30, 45, tzinfo=UTC)

    report, written = run_eval(queries_path, out_dir, k=5, now=fixed_now, strategy=NullStrategy())

    assert written == out_dir
    assert (out_dir / "report.json").is_file()
    assert (out_dir / "summary.md").is_file()

    # JSON report parses back to the same EvalReport.
    payload = json.loads((out_dir / "report.json").read_text(encoding="utf-8"))
    rehydrated = EvalReport.model_validate(payload)
    assert rehydrated == report

    # Aggregate values are well-typed (NullStrategy → all zeros).
    assert report.strategy == "null"
    assert report.k == 5
    assert report.num_queries == 2
    assert report.recall_at_k == 0.0
    assert report.mrr == 0.0
    assert report.capsule_token_efficiency == 0.0
    assert report.capsule_token_efficiency_note == CAPSULE_PLACEHOLDER_NOTE
    assert report.generated_at == "2024-01-15T12:30:45Z"

    # Per-query: q2 has empty expected → recall undefined note.
    by_id = {q.id: q for q in report.queries}
    assert by_id["q1"].notes == []
    assert by_id["q2"].notes == [RECALL_UNDEFINED_NOTE]
    for q in report.queries:
        assert q.retrieved_symbol_ids == []


@pytest.mark.unit
def test_run_eval_default_k_is_ten(tmp_path: Path) -> None:
    queries_path = tmp_path / "g.jsonl"
    _write_jsonl(
        queries_path,
        [
            json.dumps(
                {
                    "id": "q1",
                    "task": "t",
                    "task_mode_expected": "feature",
                    "expected_symbol_ids": ["a"],
                }
            ),
        ],
    )
    out_dir = tmp_path / "out"
    report, _ = run_eval(queries_path, out_dir)
    assert DEFAULT_K == 10
    assert report.k == DEFAULT_K


@pytest.mark.unit
def test_run_eval_with_custom_strategy_scores_hits(tmp_path: Path) -> None:
    """A strategy that returns a known-good ranking yields recall=1.0 / mrr=1.0."""

    class _PerfectStrategy:
        name = "perfect"

        def retrieve(self, query: str, k: int) -> list[str]:
            return ["a", "b", "c"]

    queries_path = tmp_path / "g.jsonl"
    _write_jsonl(
        queries_path,
        [
            json.dumps(
                {
                    "id": "q1",
                    "task": "t",
                    "task_mode_expected": "bugfix",
                    "expected_symbol_ids": ["a"],
                }
            ),
            json.dumps(
                {
                    "id": "q2",
                    "task": "t",
                    "task_mode_expected": "feature",
                    "expected_symbol_ids": ["a", "b"],
                }
            ),
        ],
    )
    strategy: RetrievalStrategy = _PerfectStrategy()
    report, _ = run_eval(queries_path, tmp_path / "out", k=3, strategy=strategy)
    assert report.recall_at_k == pytest.approx(1.0)
    assert report.mrr == pytest.approx(1.0)
    assert report.strategy == "perfect"


@pytest.mark.unit
def test_run_eval_rejects_zero_k(tmp_path: Path) -> None:
    queries_path = tmp_path / "g.jsonl"
    queries_path.write_text("", encoding="utf-8")
    with pytest.raises(ValueError):
        run_eval(queries_path, tmp_path / "out", k=0)


@pytest.mark.unit
def test_run_eval_empty_golden_set_writes_zero_query_report(tmp_path: Path) -> None:
    queries_path = tmp_path / "g.jsonl"
    queries_path.write_text("# only comments\n\n", encoding="utf-8")
    report, _ = run_eval(queries_path, tmp_path / "out")
    assert report.num_queries == 0
    assert report.recall_at_k == 0.0
    assert report.mrr == 0.0


# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_prepare_out_dir_uses_utc_timestamp(tmp_path: Path) -> None:
    fixed = datetime(2024, 1, 15, 12, 30, 45, tzinfo=UTC)
    nested = prepare_out_dir(tmp_path / "eval-reports", now=fixed)
    assert nested == tmp_path / "eval-reports" / "20240115T123045Z"
    assert nested.is_dir()


@pytest.mark.unit
def test_render_markdown_includes_aggregate_and_per_query() -> None:
    fixed = datetime(2024, 1, 15, 12, 30, 45, tzinfo=UTC)
    queries_path = Path("nonexistent")  # not actually loaded — we hand-build a report
    del queries_path
    report = EvalReport(
        generated_at=fixed.strftime("%Y-%m-%dT%H:%M:%SZ"),
        runtime_version="0.0.0",
        strategy="null",
        k=10,
        num_queries=0,
        recall_at_k=0.0,
        mrr=0.0,
        capsule_token_efficiency=0.0,
        capsule_token_efficiency_note=CAPSULE_PLACEHOLDER_NOTE,
        queries=[],
    )
    md = render_markdown(report)
    assert "# cognis eval report" in md
    assert "Recall@10" in md
    assert "MRR" in md
    assert "Capsule token efficiency" in md
    assert "_No queries evaluated._" in md
    assert CAPSULE_PLACEHOLDER_NOTE in md


# ---------------------------------------------------------------------------
# NullStrategy contract
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_null_strategy_returns_empty() -> None:
    s = NullStrategy()
    assert s.name == "null"
    assert s.retrieve("anything", k=5) == []


@pytest.mark.unit
def test_hybrid_strategy_has_stable_name() -> None:
    assert HybridStrategy.name == "hybrid"


# ---------------------------------------------------------------------------
# Seed fixture sanity (task 4.4)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_seed_golden_set_parses_and_covers_all_modes() -> None:
    """The committed seed at tests/fixtures/eval/golden.jsonl loads cleanly."""
    if not SEED_GOLDEN.is_file():  # pragma: no cover - guarded by repo layout
        pytest.skip(f"seed fixture missing: {SEED_GOLDEN}")
    queries = load_golden_set(SEED_GOLDEN)
    assert len(queries) >= 10, "seed should hold ≥ 10 placeholder queries (task 4.4)"
    modes_present = {q.task_mode_expected for q in queries}
    assert modes_present == set(get_args(TaskMode)), "seed must cover all 6 task modes per task 4.4"


@pytest.mark.unit
def test_strategy_from_env_returns_none_without_db(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from cognis_eval.strategy import strategy_from_env

    monkeypatch.setenv("COGNIS_DB_PATH", str(tmp_path / "missing.db"))
    assert strategy_from_env() is None


@pytest.mark.unit
def test_strategy_from_env_returns_hybrid_when_db_exists(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from cognis_eval.strategy import HybridStrategy, strategy_from_env

    db_path = tmp_path / "uckg.db"
    db_path.write_bytes(b"")
    monkeypatch.setenv("COGNIS_DB_PATH", str(db_path))
    live = strategy_from_env()
    assert isinstance(live, HybridStrategy)


@pytest.mark.unit
def test_run_eval_defaults_to_hybrid_when_db_env_points_at_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    db_path = tmp_path / "uckg.db"
    db_path.write_bytes(b"")
    monkeypatch.setenv("COGNIS_DB_PATH", str(db_path))

    queries_path = tmp_path / "g.jsonl"
    _write_jsonl(
        queries_path,
        [
            json.dumps(
                {
                    "id": "q1",
                    "task": "find bug",
                    "task_mode_expected": "bugfix",
                    "expected_symbol_ids": ["missing-symbol"],
                }
            ),
        ],
    )
    report, _ = run_eval(queries_path, tmp_path / "out", k=3)
    assert report.strategy == "hybrid"


@pytest.mark.unit
def test_seed_golden_set_smoke_runs_with_null_strategy(tmp_path: Path) -> None:
    """End-to-end smoke: real seed file + NullStrategy produces a valid report."""
    if not SEED_GOLDEN.is_file():  # pragma: no cover - guarded by repo layout
        pytest.skip(f"seed fixture missing: {SEED_GOLDEN}")
    fixed = datetime(2024, 6, 1, tzinfo=UTC)
    report, out = run_eval(SEED_GOLDEN, tmp_path / "out", k=10, now=fixed, strategy=NullStrategy())
    assert report.num_queries >= 10
    assert report.recall_at_k == 0.0  # NullStrategy retrieves nothing
    assert (out / "report.json").is_file()
    assert (out / "summary.md").is_file()
