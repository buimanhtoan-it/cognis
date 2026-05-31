"""Unit tests for packages/core/cognis/planner.py.

Covers tasks 13.1-13.4 (and 13.6 specifically):

- 30 example queries covering all 6 task modes; classifier must hit the
  expected mode on ≥ 27/30.
- ``layer_plan`` returns correct weights matching the design table.
- ``allocate_budget``: sum ≤ max_tokens; absent layers absorbed correctly.
- Latency: 1 000 classify+plan+budget iterations complete in < 30s
  (≈ 30ms/call, task 13.4).
"""

from __future__ import annotations

import time

import pytest
from cognis.planner import RESERVED_TOKENS, Planner, SectionQuotas, TaskMode

# ---------------------------------------------------------------------------
# Test data — 30 example queries, 5 per mode
# ---------------------------------------------------------------------------

# Each entry: (query_string, expected_mode)
EXAMPLE_QUERIES: list[tuple[str, TaskMode]] = [
    # ── bugfix (5) ─────────────────────────────────────────────────────────
    ("Why is the login endpoint timing out?", "bugfix"),
    ("Getting a NullPointerException in auth.py at line 42", "bugfix"),
    ("Traceback (most recent call last): File 'main.py'", "bugfix"),
    ("The service is broken after the last deploy", "bugfix"),
    ("JWT validation fails with an exception on refresh", "bugfix"),
    # ── feature (5) ────────────────────────────────────────────────────────
    ("Add OAuth2 support to the API", "feature"),
    ("Implement a rate-limiting middleware for all routes", "feature"),
    ("Build a CSV export endpoint for the reports page", "feature"),
    ("Create a background job to send email digests", "feature"),
    ("Integrate Stripe payment processing into checkout", "feature"),
    # ── refactor (5) ───────────────────────────────────────────────────────
    ("Refactor the auth module to use dependency injection", "refactor"),
    ("Extract the DB connection logic into a separate class", "refactor"),
    ("Rename all snake_case functions to camelCase", "refactor"),
    ("Split the monolithic router into feature-based modules", "refactor"),
    ("Consolidate duplicate error-handling across services", "refactor"),
    # ── explain (5) ────────────────────────────────────────────────────────
    ("How does the JWT refresh token flow work?", "explain"),
    ("Why is the cache invalidated on every write?", "explain"),
    ("What does the middleware pipeline do with requests?", "explain"),
    ("Explain the authentication architecture", "explain"),
    ("What is the design of the session management module?", "explain"),
    # ── migrate (5) ────────────────────────────────────────────────────────
    ("Migrate the database from MySQL to PostgreSQL", "migrate"),
    ("Upgrade Flask to FastAPI", "migrate"),
    ("Port the authentication module from Node.js to Python", "migrate"),
    ("Convert to async/await from callback style", "migrate"),
    ("Migrate from REST to GraphQL for the public API", "migrate"),
    # ── review (5) ─────────────────────────────────────────────────────────
    ("Review the security impact of the new auth changes", "review"),
    ("What is the risk of deploying this migration?", "review"),
    ("Assess the breaking changes in the new API version", "review"),
    ("Review the impact of removing the legacy login endpoint", "review"),
    ("Identify high-risk areas before the next release", "review"),
]

assert len(EXAMPLE_QUERIES) == 30, "Must have exactly 30 example queries"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def planner() -> Planner:
    return Planner()


# ---------------------------------------------------------------------------
# 13.6 — 30 queries, ≥ 27/30 hit expected mode
# ---------------------------------------------------------------------------


def test_classifier_30_queries_27_correct(planner: Planner) -> None:
    """Classifier must hit expected mode on at least 27 out of 30 examples."""
    correct = 0
    failures: list[str] = []

    for query, expected_mode in EXAMPLE_QUERIES:
        mode, confidence = planner.classify(query)
        if mode == expected_mode:
            correct += 1
        else:
            failures.append(
                f"  FAIL: {query!r} → got {mode!r} (conf={confidence:.2f}), "
                f"expected {expected_mode!r}"
            )

    failure_report = "\n".join(failures) if failures else "(none)"
    assert correct >= 27, f"Classifier hit {correct}/30 (need ≥ 27).\nFailures:\n{failure_report}"


# ---------------------------------------------------------------------------
# 13.1 — classify: specific behaviour tests
# ---------------------------------------------------------------------------


class TestClassify:
    def test_empty_string_returns_feature(self, planner: Planner) -> None:
        mode, conf = planner.classify("")
        assert mode == "feature"
        assert conf == 1.0

    def test_stack_trace_forces_bugfix(self, planner: Planner) -> None:
        """Stack-trace substring forces bugfix with confidence 1.0."""
        mode, conf = planner.classify("Traceback (most recent call last): ...")
        assert mode == "bugfix"
        assert conf == 1.0

    def test_at_line_number_forces_bugfix(self, planner: Planner) -> None:
        mode, _ = planner.classify("Error at line 99 in utils.py")
        assert mode == "bugfix"

    def test_hash_line_number_forces_bugfix(self, planner: Planner) -> None:
        mode, _ = planner.classify("Exception in #42")
        assert mode == "bugfix"

    def test_no_keyword_defaults_to_feature(self, planner: Planner) -> None:
        mode, conf = planner.classify("Add a new payment provider")
        assert mode == "feature"
        assert conf == 1.0

    def test_confidence_is_between_0_and_1(self, planner: Planner) -> None:
        for query, _ in EXAMPLE_QUERIES:
            _, conf = planner.classify(query)
            assert 0.0 <= conf <= 1.0, f"Confidence out of range for: {query!r}"

    def test_case_insensitive_bugfix(self, planner: Planner) -> None:
        assert planner.classify("TIMEOUT in the server")[0] == "bugfix"
        assert planner.classify("BROKEN pipe")[0] == "bugfix"

    def test_case_insensitive_refactor(self, planner: Planner) -> None:
        assert planner.classify("REFACTOR the DB layer")[0] == "refactor"

    def test_case_insensitive_explain(self, planner: Planner) -> None:
        assert planner.classify("HOW does routing work")[0] == "explain"

    def test_all_six_modes_reachable(self, planner: Planner) -> None:
        triggers: dict[TaskMode, str] = {
            "bugfix": "error in payment handler",
            "feature": "add caching layer",
            "refactor": "refactor this module",
            "explain": "explain the auth flow",
            "migrate": "migrate to postgres",
            "review": "review the security impact",
        }
        for expected, query in triggers.items():
            mode, _ = planner.classify(query)
            assert mode == expected, f"Expected {expected!r} for {query!r}, got {mode!r}"


# ---------------------------------------------------------------------------
# 13.2 — layer_plan: correct weights per design table
# ---------------------------------------------------------------------------

# Expected layer plans from design.md
_EXPECTED_PLANS: dict[TaskMode, dict[str, float]] = {
    "bugfix": {
        "structural": 35.0,
        "temporal": 20.0,
        "behavioral": 20.0,
        "semantic": 15.0,
        "lexical": 10.0,
    },
    "feature": {
        "semantic": 40.0,
        "structural": 30.0,
        "temporal": 20.0,
        "lexical": 10.0,
    },
    "refactor": {
        "structural": 50.0,
        "semantic": 20.0,
        "temporal": 20.0,
        "lexical": 10.0,
    },
    "explain": {
        "semantic": 40.0,
        "structural": 30.0,
        "lexical": 20.0,
        "temporal": 10.0,
    },
    "migrate": {
        "structural": 40.0,
        "semantic": 40.0,
        "temporal": 20.0,
    },
    "review": {
        "structural": 40.0,
        "temporal": 30.0,
        "behavioral": 30.0,
    },
}


class TestLayerPlan:
    @pytest.mark.parametrize("mode", list(_EXPECTED_PLANS.keys()))
    def test_weights_match_design_table(self, planner: Planner, mode: TaskMode) -> None:
        plan = planner.layer_plan(mode)
        expected = _EXPECTED_PLANS[mode]
        assert plan == expected, f"Mode {mode!r}: got {plan}, expected {expected}"

    @pytest.mark.parametrize("mode", list(_EXPECTED_PLANS.keys()))
    def test_weights_sum_to_100(self, planner: Planner, mode: TaskMode) -> None:
        plan = planner.layer_plan(mode)
        total = sum(plan.values())
        assert abs(total - 100.0) < 1e-9, f"Mode {mode!r}: weights sum to {total}, expected 100"

    def test_plan_preserves_priority_order(self, planner: Planner) -> None:
        """Weights dict preserves priority order (Python 3.7+ insertion order)."""
        plan = planner.layer_plan("bugfix")
        layers_in_order = list(plan.keys())
        # bugfix: structural → temporal → behavioral → semantic → lexical
        assert layers_in_order[0] == "structural"
        assert layers_in_order[-1] == "lexical"


# ---------------------------------------------------------------------------
# 13.3 — allocate_budget: correctness invariants
# ---------------------------------------------------------------------------


class TestAllocateBudget:
    def test_sum_le_max_tokens_all_layers(self, planner: Planner) -> None:
        plan = planner.layer_plan("bugfix")
        available = {"lexical", "semantic", "structural", "temporal", "behavioral"}
        q = planner.allocate_budget(8000, plan, available)
        assert sum(q.values()) <= 8000

    def test_sum_equals_max_tokens_all_available(self, planner: Planner) -> None:
        """When all layers are available, every token is allocated."""
        plan = planner.layer_plan("bugfix")
        available = {"lexical", "semantic", "structural", "temporal", "behavioral"}
        q = planner.allocate_budget(8000, plan, available)
        assert sum(q.values()) == 8000

    def test_reserved_is_500(self, planner: Planner) -> None:
        plan = planner.layer_plan("feature")
        q = planner.allocate_budget(8000, plan, {"semantic", "structural"})
        assert q.reserved == RESERVED_TOKENS

    def test_absent_layers_have_zero_quota(self, planner: Planner) -> None:
        plan = planner.layer_plan("bugfix")
        # Only structural available; others absent.
        q = planner.allocate_budget(8000, plan, {"structural"})
        assert q.temporal == 0
        assert q.behavioral == 0
        assert q.semantic == 0
        assert q.lexical == 0

    def test_absent_layers_reabsorbed_no_budget_loss(self, planner: Planner) -> None:
        """Absent-layer budget must not be lost; it flows to available layers."""
        plan = planner.layer_plan("bugfix")
        # Only structural available; all other weights (temporal 20 + behavioral 20
        # + semantic 15 + lexical 10 = 65%) must be absorbed into structural.
        q = planner.allocate_budget(8000, plan, {"structural"})
        assert sum(q.values()) == 8000
        assert q.structural == 8000 - RESERVED_TOKENS

    def test_two_layers_available(self, planner: Planner) -> None:
        # feature plan: semantic(40) → structural(30) → temporal(20) → lexical(10)
        # With only semantic and lexical available:
        #   - semantic fires first: absorbs its own 40%
        #   - structural(30) absent: carry forward
        #   - temporal(20) absent: carry forward (now 50% total carry)
        #   - lexical: 10% + 50% carry = 60%
        # So lexical ends up with MORE tokens than semantic.
        plan = planner.layer_plan("feature")
        q = planner.allocate_budget(4000, plan, {"semantic", "lexical"})
        assert sum(q.values()) <= 4000
        assert sum(q.values()) == 4000  # no budget lost
        # lexical gets semantic's absent-layer carry
        assert q.lexical > q.semantic

    def test_no_layers_available_returns_reserved_only(self, planner: Planner) -> None:
        plan = planner.layer_plan("bugfix")
        q = planner.allocate_budget(8000, plan, set())
        assert q.lexical == 0
        assert q.semantic == 0
        assert q.structural == 0
        assert q.temporal == 0
        assert q.behavioral == 0
        assert q.reserved == RESERVED_TOKENS

    def test_max_tokens_equals_reserved(self, planner: Planner) -> None:
        """When max_tokens == RESERVED_TOKENS, no tokens for layers."""
        plan = planner.layer_plan("feature")
        q = planner.allocate_budget(RESERVED_TOKENS, plan, {"semantic"})
        assert q.semantic == 0
        assert q.reserved == RESERVED_TOKENS
        assert sum(q.values()) <= RESERVED_TOKENS

    def test_max_tokens_less_than_reserved_clamped(self, planner: Planner) -> None:
        """When max_tokens < RESERVED_TOKENS, reserved is clamped."""
        plan = planner.layer_plan("feature")
        q = planner.allocate_budget(100, plan, {"semantic"})
        assert sum(q.values()) <= 100

    def test_sum_exactly_equals_max_tokens_for_all_modes(self, planner: Planner) -> None:
        """For every mode with all layers available, total == max_tokens."""
        all_layers = {"lexical", "semantic", "structural", "temporal", "behavioral"}
        all_modes: list[TaskMode] = [
            "bugfix",
            "feature",
            "refactor",
            "explain",
            "migrate",
            "review",
        ]
        for mode in all_modes:
            plan = planner.layer_plan(mode)
            q = planner.allocate_budget(8000, plan, all_layers)
            total = sum(q.values())
            assert total == 8000, (
                f"Mode {mode!r}: total={total} != 8000 (layers: {list(q.layer_values().items())})"
            )

    def test_section_quotas_values_length(self, planner: Planner) -> None:
        q = SectionQuotas(lexical=10, semantic=20, structural=30, reserved=40)
        assert len(q.values()) == 6  # 5 layers + reserved

    def test_layer_values_excludes_reserved(self, planner: Planner) -> None:
        q = SectionQuotas(lexical=10, semantic=20, structural=30, reserved=500)
        lv = q.layer_values()
        assert "reserved" not in lv
        assert len(lv) == 5

    @pytest.mark.parametrize("max_tokens", [500, 1000, 4000, 8000, 16000, 32000])
    def test_various_max_tokens(self, planner: Planner, max_tokens: int) -> None:
        plan = planner.layer_plan("feature")
        q = planner.allocate_budget(max_tokens, plan, {"semantic", "structural", "lexical"})
        assert sum(q.values()) <= max_tokens


# ---------------------------------------------------------------------------
# 13.4 — Latency: 1 000 iterations < 30s (≈ 30ms/call)
# ---------------------------------------------------------------------------


class TestLatency:
    def test_1000_iterations_under_30s(self, planner: Planner) -> None:
        """Full classify + plan + budget pipeline must average < 30ms/call."""
        available = {"lexical", "semantic", "structural"}
        queries = [q for q, _ in EXAMPLE_QUERIES]
        iterations = 1000

        start = time.perf_counter()
        for i in range(iterations):
            query = queries[i % len(queries)]
            mode, _ = planner.classify(query)
            plan = planner.layer_plan(mode)
            planner.allocate_budget(8000, plan, available)
        elapsed = time.perf_counter() - start

        assert elapsed < 30.0, (
            f"1 000 iterations took {elapsed:.3f}s (> 30s budget; ≈{elapsed:.1f}ms/call avg)"
        )
