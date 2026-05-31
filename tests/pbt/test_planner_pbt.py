"""Property-based tests for cognis.planner — CP-8.

**Validates: Requirements 17.1, 17.2** (REQ-PLN-3: max_tokens respected,
absent layers reallocate budget — no budget loss).

CP-8: For any task string and max_tokens:
  - ``sum(quotas.values()) ≤ max_tokens``
  - Absent layers reallocate their budget to the next-priority available layer
    (no tokens lost from the distributable pool).

Uses Hypothesis with:
  - ``st.text()`` for task strings
  - ``st.integers(500, 32000)`` for max_tokens
  - ``st.frozensets`` of layer name subsets for available_layers
"""

from __future__ import annotations

import pytest
from cognis.planner import RESERVED_TOKENS, Planner, TaskMode
from hypothesis import given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

_ALL_LAYER_NAMES = ["lexical", "semantic", "structural", "temporal", "behavioral"]
_ALL_MODES: list[TaskMode] = ["bugfix", "feature", "refactor", "explain", "migrate", "review"]

# Task string strategy: arbitrary text (including empty, unicode, long strings).
task_strategy = st.text(min_size=0, max_size=500)

# max_tokens strategy: integers in [500, 32000] per design hard limits.
max_tokens_strategy = st.integers(min_value=500, max_value=32000)

# available_layers strategy: any subset of the five layer names (as frozenset).
available_layers_strategy = st.frozensets(
    st.sampled_from(_ALL_LAYER_NAMES),
    min_size=0,
    max_size=5,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_planner = Planner()


# ---------------------------------------------------------------------------
# CP-8 Property 1: sum(quotas.values()) ≤ max_tokens
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=200, deadline=None)
@given(
    task=task_strategy,
    max_tokens=max_tokens_strategy,
    available_layers=available_layers_strategy,
)
def test_cp8_budget_conservation(
    task: str,
    max_tokens: int,
    available_layers: frozenset[str],
) -> None:
    """CP-8: sum(quotas.values()) ≤ max_tokens for any task, max_tokens, and available_layers.

    **Validates: Requirements 17.1, 17.2**
    """
    mode, _ = _planner.classify(task)
    plan = _planner.layer_plan(mode)
    quotas = _planner.allocate_budget(max_tokens, plan, set(available_layers))

    total = sum(quotas.values())
    assert total <= max_tokens, (
        f"Budget overrun: sum={total} > max_tokens={max_tokens} "
        f"(task={task!r}, mode={mode!r}, available={sorted(available_layers)})"
    )


# ---------------------------------------------------------------------------
# CP-8 Property 2: absent layers reallocate — no budget loss
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=200, deadline=None)
@given(
    max_tokens=max_tokens_strategy,
    available_layers=available_layers_strategy,
    # Pick a fixed mode to make the test deterministic for any given input.
    mode=st.sampled_from(_ALL_MODES),
)
def test_cp8_no_budget_loss_from_absent_layers(
    max_tokens: int,
    available_layers: frozenset[str],
    mode: TaskMode,
) -> None:
    """CP-8: when at least one layer that is IN THE PLAN is available,
    distributable tokens must be fully allocated (absent layers' budget must
    not disappear).

    **Validates: Requirements 17.1, 17.2**
    """
    plan = _planner.layer_plan(mode)
    plan_layer_names = set(plan.keys())
    quotas = _planner.allocate_budget(max_tokens, plan, set(available_layers))

    total = sum(quotas.values())

    # Check if any plan layer is actually available.
    plan_layers_available = plan_layer_names & set(available_layers)

    if plan_layers_available and max_tokens > RESERVED_TOKENS:
        # When at least one plan layer is available and there are distributable
        # tokens, the total MUST equal max_tokens exactly (no tokens lost).
        assert total == max_tokens, (
            f"Budget loss detected: total={total} ≠ max_tokens={max_tokens} "
            f"(mode={mode!r}, available={sorted(available_layers)}, "
            f"plan_layers_available={sorted(plan_layers_available)})"
        )
    else:
        # No plan layers available OR max_tokens ≤ RESERVED_TOKENS:
        # all layer quotas must be 0; reserved ≤ max_tokens.
        assert quotas.lexical == 0
        assert quotas.semantic == 0
        assert quotas.structural == 0
        assert quotas.temporal == 0
        assert quotas.behavioral == 0
        assert total <= max_tokens


# ---------------------------------------------------------------------------
# CP-8 Property 3: reserved is always exactly RESERVED_TOKENS
# (or max_tokens if max_tokens < RESERVED_TOKENS, but strategy floor is 500)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=200, deadline=None)
@given(
    task=task_strategy,
    max_tokens=max_tokens_strategy,
    available_layers=available_layers_strategy,
)
def test_cp8_reserved_is_constant(
    task: str,
    max_tokens: int,
    available_layers: frozenset[str],
) -> None:
    """CP-8: reserved tokens are always exactly RESERVED_TOKENS.

    Since the strategy minimum is 500 == RESERVED_TOKENS, this always holds.

    **Validates: Requirements 17.1, 17.2**
    """
    mode, _ = _planner.classify(task)
    plan = _planner.layer_plan(mode)
    quotas = _planner.allocate_budget(max_tokens, plan, set(available_layers))

    assert quotas.reserved == RESERVED_TOKENS, (
        f"reserved={quotas.reserved} ≠ {RESERVED_TOKENS} (max_tokens={max_tokens})"
    )


# ---------------------------------------------------------------------------
# CP-8 Property 4: layer quotas are non-negative
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=200, deadline=None)
@given(
    task=task_strategy,
    max_tokens=max_tokens_strategy,
    available_layers=available_layers_strategy,
)
def test_cp8_all_quotas_non_negative(
    task: str,
    max_tokens: int,
    available_layers: frozenset[str],
) -> None:
    """CP-8: no layer quota can be negative.

    **Validates: Requirements 17.1, 17.2**
    """
    mode, _ = _planner.classify(task)
    plan = _planner.layer_plan(mode)
    quotas = _planner.allocate_budget(max_tokens, plan, set(available_layers))

    for val in quotas.values():
        assert val >= 0, f"Negative quota found: {quotas} (task={task!r}, max_tokens={max_tokens})"


# ---------------------------------------------------------------------------
# CP-8 Property 5: absent layers always have quota == 0
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=200, deadline=None)
@given(
    max_tokens=max_tokens_strategy,
    available_layers=available_layers_strategy,
    mode=st.sampled_from(_ALL_MODES),
)
def test_cp8_absent_layers_have_zero_quota(
    max_tokens: int,
    available_layers: frozenset[str],
    mode: TaskMode,
) -> None:
    """CP-8: layers NOT in available_layers must always have quota == 0.

    **Validates: Requirements 17.1, 17.2**
    """
    plan = _planner.layer_plan(mode)
    quotas = _planner.allocate_budget(max_tokens, plan, set(available_layers))
    lv = quotas.layer_values()

    for layer_name in _ALL_LAYER_NAMES:
        if layer_name not in available_layers:
            assert lv[layer_name] == 0, (
                f"Absent layer {layer_name!r} has non-zero quota {lv[layer_name]} "
                f"(mode={mode!r}, available={sorted(available_layers)})"
            )
