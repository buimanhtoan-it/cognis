"""Property-based tests for the Capsule composer — Task 14.6 (CP-8, CP-9).

**Validates: Requirements 19.1, 19.2, 19.3** (REQ-CAP-1)

Two properties are tested:

CP-8: Composing a capsule with random ``max_tokens ∈ [500, 32000]`` always
  produces output with ``token_estimate ≤ max_tokens``.

CP-9: ``sources[]`` is non-empty for every capsule that has at least one
  populated section (``root_cause_candidates`` or ``relevant_symbols``).

The tests use Hypothesis for property generation and a real in-memory SQLite
database (no mocking) to exercise the full composition pipeline.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any

import pytest
from cognis.capsule.composer import CapsuleComposer
from cognis.capsule.models import ContextCapsule
from cognis.db import Database, upsert_symbol
from cognis.models import SymbolNode
from cognis.planner import TaskMode
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

pytestmark = pytest.mark.pbt

# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

_TASK_MODES: list[TaskMode] = ["bugfix", "feature", "refactor", "explain", "migrate", "review"]
_LAYERS = ["lexical", "semantic", "structural"]

# Valid symbol_id format (simplified for tests)
_SYMBOL_ID_RE = st.from_regex(
    r"py:src/[a-z]{1,8}\.py:[a-z]{1,8}\.[a-z]{1,8}@[a-f0-9]{8}", fullmatch=True
)


@dataclass
class FakeHit:
    """Minimal Hit stand-in for PBT (avoids importing cognis_retrieval)."""

    symbol_id: str
    score: float
    layer: str
    reason: str
    evidence: dict[str, Any] = field(default_factory=dict)


def _hit_strategy() -> Any:
    """Generate a valid FakeHit."""
    return st.builds(
        FakeHit,
        symbol_id=_SYMBOL_ID_RE,
        score=st.floats(min_value=0.0, max_value=1.0, allow_nan=False, allow_infinity=False),
        layer=st.sampled_from(_LAYERS),
        reason=st.text(min_size=1, max_size=50),
        evidence=st.just({}),
    )


def _hits_strategy() -> Any:
    """Generate a list of 0-10 FakeHit objects."""
    return st.lists(_hit_strategy(), min_size=0, max_size=10)


def _build_db_with_symbols(symbols: list[SymbolNode], tmp_path: str) -> Database:
    """Create a real DB and populate it with the given symbols."""
    db = Database(tmp_path, vec_enabled=False)
    for sym in symbols:
        upsert_symbol(db, sym)
    return db


def _make_symbol_for_hit(hit: FakeHit) -> SymbolNode:
    """Create a SymbolNode that corresponds to a FakeHit."""
    parts = hit.symbol_id.split(":")
    file_path = parts[1] if len(parts) > 1 else "src/mod.py"
    qname_at = parts[2] if len(parts) > 2 else "mod.func@abcd1234"
    qname = qname_at.split("@")[0] if "@" in qname_at else qname_at
    name = qname.split(".")[-1]
    module = qname.split(".")[0] if "." in qname else qname

    return SymbolNode(
        id=hit.symbol_id,
        kind="function",
        name=name,
        qualified_name=qname,
        language="python",
        module=module,
        file_path=file_path,
        line_start=1,
        line_end=10,
        content_hash="abc123",
        body_excerpt=f"def {name}(): pass",
        untrusted_flags=[],
        updated_at=int(time.time()),
    )


# ---------------------------------------------------------------------------
# CP-8: token_estimate ≤ max_tokens
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(
    max_examples=100,
    suppress_health_check=[HealthCheck.function_scoped_fixture],
    deadline=None,
)
@given(
    max_tokens=st.integers(min_value=500, max_value=32000),
    hits=_hits_strategy(),
    mode=st.sampled_from(_TASK_MODES),
)
def test_cp8_token_estimate_within_budget(
    tmp_path: Any,
    max_tokens: int,
    hits: list[FakeHit],
    mode: TaskMode,
) -> None:
    """**Validates: Requirements 19.1, 17.1**

    CP-8: For any max_tokens ∈ [500, 32000], the composed capsule always
    has token_estimate ≤ max_tokens.
    """
    # Build DB with symbols for each hit (deduplicated by symbol_id)
    seen_ids: set[str] = set()
    symbols: list[SymbolNode] = []
    for hit in hits:
        if hit.symbol_id not in seen_ids:
            seen_ids.add(hit.symbol_id)
            symbols.append(_make_symbol_for_hit(hit))

    db_path = str(tmp_path / f"test_cp8_{max_tokens}_{mode}.db")
    db = _build_db_with_symbols(symbols, db_path)

    composer = CapsuleComposer()
    capsule: ContextCapsule = composer.compose(
        task=f"test task for mode={mode}",
        mode=mode,
        confidence=0.9,
        hits=hits,  # type: ignore[arg-type]
        max_tokens=max_tokens,
        db=db,
    )

    assert isinstance(capsule, ContextCapsule)
    assert capsule.token_estimate <= max_tokens, (
        f"CP-8 violation: token_estimate={capsule.token_estimate} > max_tokens={max_tokens} "
        f"(mode={mode}, hits={len(hits)})"
    )


# ---------------------------------------------------------------------------
# CP-9: sources[] non-empty for every populated section
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(
    max_examples=100,
    suppress_health_check=[HealthCheck.function_scoped_fixture],
    deadline=None,
)
@given(
    hits=_hits_strategy(),
    mode=st.sampled_from(_TASK_MODES),
    max_tokens=st.integers(min_value=500, max_value=32000),
)
def test_cp9_sources_non_empty_for_populated_sections(
    tmp_path: Any,
    hits: list[FakeHit],
    mode: TaskMode,
    max_tokens: int,
) -> None:
    """**Validates: Requirements 19.2, 19.3**

    CP-9: For every composed capsule, if any section (root_cause_candidates
    or relevant_symbols) is populated, then sources[] must be non-empty.

    Additionally, every symbol_id in populated sections must have a
    corresponding source entry (per-claim source rule).
    """
    seen_ids: set[str] = set()
    symbols: list[SymbolNode] = []
    for hit in hits:
        if hit.symbol_id not in seen_ids:
            seen_ids.add(hit.symbol_id)
            symbols.append(_make_symbol_for_hit(hit))

    db_path = str(tmp_path / f"test_cp9_{mode}_{max_tokens}.db")
    db = _build_db_with_symbols(symbols, db_path)

    composer = CapsuleComposer()
    capsule = composer.compose(
        task=f"test for cp9 mode={mode}",
        mode=mode,
        confidence=0.9,
        hits=hits,  # type: ignore[arg-type]
        max_tokens=max_tokens,
        db=db,
    )

    has_populated_section = bool(capsule.root_cause_candidates or capsule.relevant_symbols)

    if has_populated_section:
        assert len(capsule.sources) > 0, (
            "CP-9 violation: populated sections exist but sources[] is empty"
        )

        # Per-claim source check
        source_ids = {s.id for s in capsule.sources}
        for rcc in capsule.root_cause_candidates:
            assert rcc.symbol_id in source_ids, (
                f"CP-9 violation: root_cause_candidate '{rcc.symbol_id}' has no source"
            )
        for rs in capsule.relevant_symbols:
            assert rs.symbol_id in source_ids, (
                f"CP-9 violation: relevant_symbol '{rs.symbol_id}' has no source"
            )

    # Additionally: empty sections means empty lists not None
    assert capsule.root_cause_candidates is not None
    assert capsule.relevant_symbols is not None
    assert capsule.sources is not None
