"""Property-based tests for CSAR (Code Spreading-Activation Retrieval).

**Validates: docs/csar.md Theorems 1-5** on *randomly generated* graphs and
seed distributions, so the guarantees hold beyond the hand-picked unit cases.

CP-CSAR-1 — Mass conservation (Theorem 3)
    ``‖ppr(s)‖₁ = ‖s‖₁`` and ``ppr(s) >= 0`` for every column-stochastic ``P``.

CP-CSAR-2 — Solver agreement + geometric bound (Theorems 1, 2)
    Power iteration converges to the exact closed form; the per-iteration L1
    error never exceeds the ``(1-alpha)^t`` contraction bound.

CP-CSAR-3 — Forward-push invariant (Theorem 5a, 5b)
    ``ppr(s) = p + ppr(residual)`` exactly, and the L1 estimate error equals the
    leftover residual mass at termination.

CP-CSAR-4 — Push work bound (Theorem 5c)
    Total push work ``Σ d_u ≤ 1/(alpha·eps)`` — independent of graph size.

CP-CSAR-5 — Endpoint limit (Theorem 4)
    ``alpha = 1`` ⇒ ``ppr(s) = s``.
"""

from __future__ import annotations

import numpy as np
import pytest
from cognis_retrieval.csar import (
    CodeGraph,
    approximate_ppr_push,
    personalized_pagerank_exact,
    transition_matrix,
)
from hypothesis import assume, given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Helpers / strategies
# ---------------------------------------------------------------------------
#
# These properties verify the *graph-math* theorems, so we build CodeGraph
# objects directly from random edge lists (no SQLite round-trip per example).
# This keeps the suite fast while still exercising the real symmetrization /
# self-loop / column-stochastic logic used in production. ``build_code_graph``
# (the DB extraction path) is covered separately in ``tests/unit/test_csar.py``.


def _make_graph(edges: list[tuple[str, str]]) -> CodeGraph:
    """Build a symmetrized, column-stochastic CodeGraph from random edges.

    Mirrors :func:`cognis_retrieval.csar.build_code_graph` semantics: drop
    self-edges, coalesce + symmetrize weights (confidence 1.0 each), and give
    isolated nodes a self-loop.
    """
    ids = sorted({n for e in edges for n in e})
    index = {sid: i for i, sid in enumerate(ids)}
    n = len(ids)
    acc: list[dict[int, float]] = [dict() for _ in range(n)]
    for s, d in edges:
        if s == d:
            continue
        u, v = index[s], index[d]
        acc[u][v] = acc[u].get(v, 0.0) + 1.0
        acc[v][u] = acc[v].get(u, 0.0) + 1.0

    adjacency: list[list[tuple[int, float]]] = []
    degree = np.zeros(n, dtype=np.float64)
    for u in range(n):
        nb = acc[u]
        if not nb:
            adjacency.append([(u, 1.0)])
            degree[u] = 1.0
        else:
            items = sorted(nb.items())
            adjacency.append([(v, w) for v, w in items])
            degree[u] = float(sum(w for _, w in items))
    return CodeGraph(node_ids=ids, index=index, adjacency=adjacency, degree=degree)


_node_st = st.text(alphabet="ABCDEFGHIJKLMNOP", min_size=1, max_size=3)
_edges_st = st.lists(st.tuples(_node_st, _node_st), min_size=1, max_size=25)
_alpha_st = st.floats(min_value=0.05, max_value=0.95)


# ---------------------------------------------------------------------------
# CP-CSAR-1 — Mass conservation (Theorem 3)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(edges=_edges_st, alpha=_alpha_st, seed_idx=st.integers(min_value=0, max_value=15))
@settings(max_examples=150, deadline=None)
def test_cp_csar_1_mass_conservation(
    edges: list[tuple[str, str]], alpha: float, seed_idx: int
) -> None:
    """Validates Theorem 3: ‖ppr(s)‖₁ = ‖s‖₁ and ppr(s) >= 0."""
    g = _make_graph(edges)
    assume(g.n > 0)
    P = transition_matrix(g)

    s = np.zeros(g.n)
    s[seed_idx % g.n] = 1.0
    r = personalized_pagerank_exact(P, s, alpha)

    assert r.sum() == pytest.approx(1.0, abs=1e-7)
    assert np.all(r >= -1e-9)


# ---------------------------------------------------------------------------
# CP-CSAR-2 — Power iteration == exact, with geometric contraction (T1, T2)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(edges=_edges_st, alpha=_alpha_st)
@settings(max_examples=120, deadline=None)
def test_cp_csar_2_power_matches_exact_and_contracts(
    edges: list[tuple[str, str]], alpha: float
) -> None:
    """Validates Theorems 1 & 2: power iteration -> exact; error <= (1-alpha)^t·E0."""
    g = _make_graph(edges)
    assume(g.n > 0)
    P = transition_matrix(g)

    s = np.zeros(g.n)
    s[0] = 1.0
    r_star = personalized_pagerank_exact(P, s, alpha)

    # Iterate manually and check the contraction bound each step.
    r = s.copy()
    err0 = float(np.abs(r - r_star).sum())
    for t in range(1, 8):
        r = alpha * s + (1.0 - alpha) * (P @ r)
        err = float(np.abs(r - r_star).sum())
        bound = (1.0 - alpha) ** t * err0
        assert err <= bound + 1e-9

    # After enough iterations the power solution matches exact. The contraction
    # rate is (1-alpha); for the smallest alpha (0.05) we need ~1.4k iterations
    # to drive (0.95)^t·err0 well below the tolerance, so iterate generously.
    for _ in range(2000):
        r = alpha * s + (1.0 - alpha) * (P @ r)
    np.testing.assert_allclose(r, r_star, atol=1e-6)


# ---------------------------------------------------------------------------
# CP-CSAR-3 — Forward-push invariant (Theorem 5a, 5b)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(
    edges=_edges_st,
    alpha=_alpha_st,
    eps=st.floats(min_value=1e-6, max_value=1e-3),
    seed_idx=st.integers(min_value=0, max_value=15),
)
@settings(max_examples=150, deadline=None)
def test_cp_csar_3_push_invariant(
    edges: list[tuple[str, str]], alpha: float, eps: float, seed_idx: int
) -> None:
    """Validates Theorem 5a/5b: ppr(s) = p + ppr(residual); ‖ppr(s)-p‖₁ = ‖residual‖₁."""
    g = _make_graph(edges)
    assume(g.n > 0)
    P = transition_matrix(g)

    seed_node = seed_idx % g.n
    s = np.zeros(g.n)
    s[seed_node] = 1.0

    push = approximate_ppr_push(g, {seed_node: 1.0}, alpha, eps)

    p_vec = np.zeros(g.n)
    for node, mass in push.estimate.items():
        p_vec[node] = mass
    resid_vec = np.zeros(g.n)
    for node, mass in push.residual.items():
        resid_vec[node] = mass

    exact = personalized_pagerank_exact(P, s, alpha)

    # T5a: exact invariant.
    rhs = p_vec + personalized_pagerank_exact(P, resid_vec, alpha)
    np.testing.assert_allclose(exact, rhs, atol=1e-7)

    # T5b: estimate error equals residual mass (ppr preserves L1).
    l1_err = float(np.abs(exact - p_vec).sum())
    resid_mass = float(resid_vec.sum())
    assert l1_err == pytest.approx(resid_mass, abs=1e-7)

    # Termination: every residual is below threshold eps*degree.
    for node, mass in push.residual.items():
        assert mass < eps * g.degree[node] + 1e-12


# ---------------------------------------------------------------------------
# CP-CSAR-4 — Push work bound (Theorem 5c)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(
    edges=_edges_st,
    alpha=_alpha_st,
    eps=st.floats(min_value=1e-5, max_value=1e-2),
    seed_idx=st.integers(min_value=0, max_value=15),
)
@settings(max_examples=150, deadline=None)
def test_cp_csar_4_work_bound(
    edges: list[tuple[str, str]], alpha: float, eps: float, seed_idx: int
) -> None:
    """Validates Theorem 5c: Σ d_u <= 1/(alpha·eps)."""
    g = _make_graph(edges)
    assume(g.n > 0)
    seed_node = seed_idx % g.n
    push = approximate_ppr_push(g, {seed_node: 1.0}, alpha, eps)
    assert push.work <= 1.0 / (alpha * eps) + 1e-6


# ---------------------------------------------------------------------------
# CP-CSAR-5 — Endpoint limit (Theorem 4)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(edges=_edges_st, seed_idx=st.integers(min_value=0, max_value=15))
@settings(max_examples=80, deadline=None)
def test_cp_csar_5_alpha_one_is_identity(edges: list[tuple[str, str]], seed_idx: int) -> None:
    """Validates Theorem 4: alpha = 1 ⇒ ppr(s) = s."""
    g = _make_graph(edges)
    assume(g.n > 0)
    P = transition_matrix(g)
    s = np.zeros(g.n)
    s[seed_idx % g.n] = 1.0
    r = personalized_pagerank_exact(P, s, 1.0)
    np.testing.assert_allclose(r, s, atol=1e-12)
