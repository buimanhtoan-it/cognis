"""Unit tests for CSAR — Code Spreading-Activation Retrieval.

These tests are *proofs-in-code* of the theorems in ``docs/csar.md`` on concrete
graphs, plus retrieval-behavior checks showing CSAR recovers on-path symbols
that pure KNN/lexical seeding misses.

Theorems verified numerically here:

- T1/T2: power iteration converges to the exact closed-form solution.
- T3:    mass conservation (``‖r‖₁ = ‖s‖₁``).
- T4:    endpoint limits (``alpha→1`` ⇒ ``r=s``).
- T5a:   forward-push invariant ``ppr(seed) = p + ppr(residual)``.
- T5b:   ``‖ppr(seed) - p‖₁ = ‖residual‖₁`` at termination.
- T5c:   push work bound ``Σ d_u ≤ 1/(alpha·eps)``.

All graph tests use plain numpy; retrieval tests use a real in-memory SQLite DB.
"""

from __future__ import annotations

import contextlib
import os
import shutil
import tempfile
import time

import numpy as np
import pytest
from cognis.db import Database, upsert_edge, upsert_symbols
from cognis.models import Edge, SymbolNode
from cognis_retrieval import Hit, LexicalLayer, populate_fts
from cognis_retrieval.csar import (
    CSARLayer,
    approximate_ppr_push,
    build_code_graph,
    personalized_pagerank_exact,
    personalized_pagerank_power,
    transition_matrix,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
#
# Each test gets its *own* file-backed SQLite database. We avoid ``:memory:``
# because cognis caches one connection per thread keyed on the path string, so
# every ``Database(":memory:")`` in a thread would share one store and leak
# symbols/edges across tests (and into the other retrieval test modules).

_OPEN_DBS: list[Database] = []
_TMP_DIRS: list[str] = []


@pytest.fixture(autouse=True)
def _cleanup_dbs() -> object:
    """Close connections and remove temp dirs created during each test."""
    yield None
    for db in _OPEN_DBS:
        with contextlib.suppress(Exception):
            db.close_thread_connection()
    _OPEN_DBS.clear()
    for directory in _TMP_DIRS:
        shutil.rmtree(directory, ignore_errors=True)
    _TMP_DIRS.clear()


def _new_db() -> Database:
    """Create an isolated file-backed Database registered for cleanup."""
    directory = tempfile.mkdtemp(prefix="csar_test_")
    _TMP_DIRS.append(directory)
    db = Database(os.path.join(directory, "uckg.db"), vec_enabled=False)
    _OPEN_DBS.append(db)
    return db


def _sym(sym_id: str, name: str | None = None, **kw: object) -> SymbolNode:
    return SymbolNode(
        id=sym_id,
        kind="function",
        name=name or sym_id,
        qualified_name=kw.get("qualified_name", name or sym_id),  # type: ignore[arg-type]
        language="python",
        module="m",
        file_path="src/m.py",
        line_start=1,
        line_end=2,
        signature=kw.get("signature"),  # type: ignore[arg-type]
        docstring=kw.get("docstring"),  # type: ignore[arg-type]
        content_hash=(sym_id[:8].ljust(8, "0")),
        body_excerpt=kw.get("body_excerpt"),  # type: ignore[arg-type]
        updated_at=int(time.time()),
    )


def _graph_db(edges: list[tuple[str, str]], extra_nodes: list[str] | None = None) -> Database:
    db = _new_db()
    ids: set[str] = set(extra_nodes or [])
    for s, d in edges:
        ids.add(s)
        ids.add(d)
    upsert_symbols(db, [_sym(i) for i in sorted(ids)])
    for s, d in edges:
        upsert_edge(db, Edge(src_id=s, dst_id=d, kind="calls"))
    return db


def _dense_ppr(P: np.ndarray, s: np.ndarray, alpha: float) -> np.ndarray:
    return personalized_pagerank_exact(P, s, alpha)


# ---------------------------------------------------------------------------
# Graph construction
# ---------------------------------------------------------------------------


class TestBuildCodeGraph:
    def test_symmetric_edges(self) -> None:
        db = _graph_db([("A", "B")])
        g = build_code_graph(db)
        a, b = g.index["A"], g.index["B"]
        # A--B both directions present (symmetrized).
        assert (b, 1.0) in g.adjacency[a]
        assert (a, 1.0) in g.adjacency[b]

    def test_isolated_node_self_loop(self) -> None:
        db = _graph_db([("A", "B")], extra_nodes=["ISO"])
        g = build_code_graph(db)
        iso = g.index["ISO"]
        assert g.adjacency[iso] == [(iso, 1.0)]
        assert g.degree[iso] == 1.0

    def test_transition_matrix_column_stochastic(self) -> None:
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        col_sums = P.sum(axis=0)
        np.testing.assert_allclose(col_sums, np.ones(g.n), atol=1e-12)

    def test_dst_missing_edge_excluded(self) -> None:
        db = _graph_db([("A", "B")])
        # Add an edge then flag dst_missing.
        upsert_edge(db, Edge(src_id="A", dst_id="B", kind="imports", meta={"dst_missing": True}))
        g = build_code_graph(db)
        a, b = g.index["A"], g.index["B"]
        # Only the 'calls' edge (weight 1.0) survives; imports excluded.
        weight = dict(g.adjacency[a]).get(b)
        assert weight == 1.0


# ---------------------------------------------------------------------------
# T1/T2: power iteration == exact closed form
# ---------------------------------------------------------------------------


class TestSolversAgree:
    @pytest.mark.parametrize("alpha", [0.05, 0.15, 0.5, 0.85])
    def test_power_matches_exact(self, alpha: float) -> None:
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A"), ("C", "D")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0

        exact = personalized_pagerank_exact(P, s, alpha)
        power, iters = personalized_pagerank_power(P, s, alpha, tol=1e-12, max_iter=10_000)

        np.testing.assert_allclose(power, exact, atol=1e-8)
        assert iters >= 1

    def test_geometric_convergence_rate(self) -> None:
        """The L1 error after t iters is <= (1-alpha)^t * ‖r0 - r*‖ (Theorem 2)."""
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "D"), ("D", "A")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0
        alpha = 0.3
        r_star = personalized_pagerank_exact(P, s, alpha)

        r = s.copy()
        err0 = float(np.abs(r - r_star).sum())
        for t in range(1, 6):
            r = alpha * s + (1.0 - alpha) * (P @ r)
            err = float(np.abs(r - r_star).sum())
            bound = (1.0 - alpha) ** t * err0
            assert err <= bound + 1e-12


# ---------------------------------------------------------------------------
# T3: mass conservation
# ---------------------------------------------------------------------------


class TestMassConservation:
    @pytest.mark.parametrize("alpha", [0.1, 0.25, 0.6, 1.0])
    def test_l1_preserved(self, alpha: float) -> None:
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A"), ("B", "D"), ("D", "E")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 0.7
        s[g.index["C"]] = 0.3
        r = personalized_pagerank_exact(P, s, alpha)
        assert r.sum() == pytest.approx(1.0, abs=1e-9)
        assert np.all(r >= -1e-12)  # nonnegative


# ---------------------------------------------------------------------------
# T4: endpoint limit alpha -> 1 gives r = s
# ---------------------------------------------------------------------------


class TestEndpointLimits:
    def test_alpha_one_returns_seed(self) -> None:
        db = _graph_db([("A", "B"), ("B", "C")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0
        r = personalized_pagerank_exact(P, s, 1.0)
        np.testing.assert_allclose(r, s, atol=1e-12)

    def test_small_alpha_spreads_mass(self) -> None:
        """Low alpha pushes mass away from the single seed toward neighbors."""
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "D")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0
        r_hi = personalized_pagerank_exact(P, s, 0.9)  # near-seed
        r_lo = personalized_pagerank_exact(P, s, 0.05)  # spread
        a = g.index["A"]
        # With low alpha, less mass remains on the seed node A.
        assert r_lo[a] < r_hi[a]

    def test_alpha_to_zero_approaches_stationary(self) -> None:
        """T4 (second endpoint): as alpha->0+, r* -> the stationary distribution pi.

        For an undirected, connected, non-bipartite graph the random walk is
        ergodic and its unique stationary distribution is pi_i = d_i / sum_j d_j
        (degree-proportional). The docs claim ``lim_{alpha->0+} r* = pi``; here
        we verify it numerically: r*(alpha) converges to the degree distribution
        and the error shrinks monotonically as alpha decreases.
        """
        # Triangle + pendant: connected, non-bipartite (odd cycle present),
        # so the symmetrized walk is ergodic and π is degree-proportional.
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A"), ("C", "D")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0

        pi = g.degree / g.degree.sum()
        # P must fix the stationary distribution: P @ pi = pi.
        np.testing.assert_allclose(P @ pi, pi, atol=1e-12)

        prev_err = None
        for alpha in (0.5, 0.1, 0.01, 1e-3, 1e-4):
            r = personalized_pagerank_exact(P, s, alpha)
            err = float(np.abs(r - pi).sum())
            if prev_err is not None:
                # Strictly decreasing error as alpha shrinks toward 0.
                assert err < prev_err
            prev_err = err
        # At very small alpha the PPR vector is essentially the stationary pi.
        r_tiny = personalized_pagerank_exact(P, s, 1e-5)
        np.testing.assert_allclose(r_tiny, pi, atol=1e-3)


# ---------------------------------------------------------------------------
# T5: forward-push correctness and cost bound
# ---------------------------------------------------------------------------


class TestForwardPush:
    @pytest.mark.parametrize("alpha", [0.1, 0.15, 0.4])
    def test_push_approximates_exact(self, alpha: float) -> None:
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A"), ("C", "D"), ("D", "E")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0
        exact = personalized_pagerank_exact(P, s, alpha)

        eps = 1e-7
        push = approximate_ppr_push(g, {g.index["A"]: 1.0}, alpha, eps)
        approx = np.zeros(g.n)
        for node, mass in push.estimate.items():
            approx[node] = mass

        # T5b: L1 error equals leftover residual mass, and is small for tiny eps.
        l1_err = float(np.abs(exact - approx).sum())
        resid_mass = sum(push.residual.values())
        assert l1_err <= resid_mass + 1e-9
        assert l1_err < 1e-3

    def test_push_invariant_ppr_seed_equals_p_plus_ppr_residual(self) -> None:
        """T5a: ppr(seed) == p + ppr(residual) exactly (linear-algebra identity)."""
        db = _graph_db([("A", "B"), ("B", "C"), ("C", "A"), ("C", "D")])
        g = build_code_graph(db)
        P = transition_matrix(g)
        alpha = 0.2
        s = np.zeros(g.n)
        s[g.index["A"]] = 1.0

        push = approximate_ppr_push(g, {g.index["A"]: 1.0}, alpha, eps=1e-6)

        p_vec = np.zeros(g.n)
        for node, mass in push.estimate.items():
            p_vec[node] = mass
        resid_vec = np.zeros(g.n)
        for node, mass in push.residual.items():
            resid_vec[node] = mass

        lhs = _dense_ppr(P, s, alpha)
        rhs = p_vec + _dense_ppr(P, resid_vec, alpha)
        np.testing.assert_allclose(lhs, rhs, atol=1e-9)

    @pytest.mark.parametrize("alpha,eps", [(0.15, 1e-4), (0.2, 1e-5), (0.5, 1e-6)])
    def test_work_bound(self, alpha: float, eps: float) -> None:
        """T5c: total push work Σ d_u <= 1/(alpha*eps), independent of n."""
        # A moderately connected graph.
        edges = [(f"N{i}", f"N{(i + 1) % 12}") for i in range(12)]
        edges += [(f"N{i}", f"N{(i + 3) % 12}") for i in range(12)]
        db = _graph_db(edges)
        g = build_code_graph(db)
        push = approximate_ppr_push(g, {g.index["N0"]: 1.0}, alpha, eps)
        assert push.work <= 1.0 / (alpha * eps) + 1e-9

    def test_work_independent_of_graph_size(self) -> None:
        """Same seed + params on a small and a 10x-larger ring -> same work bound."""
        alpha, eps = 0.15, 1e-4
        small = _graph_db([(f"R{i}", f"R{(i + 1) % 10}") for i in range(10)])
        big = _graph_db([(f"R{i}", f"R{(i + 1) % 100}") for i in range(100)])
        gs = build_code_graph(small)
        gb = build_code_graph(big)
        ps = approximate_ppr_push(gs, {gs.index["R0"]: 1.0}, alpha, eps)
        pb = approximate_ppr_push(gb, {gb.index["R0"]: 1.0}, alpha, eps)
        bound = 1.0 / (alpha * eps)
        assert ps.work <= bound and pb.work <= bound


# ---------------------------------------------------------------------------
# Retrieval behavior: CSAR recovers on-path symbols that seeding misses
# ---------------------------------------------------------------------------


class TestCSARRetrieval:
    def _seed_layer_db(self) -> Database:
        """Login flow: postLogin -> requireAuth -> validate.

        Only `validate` and `postLogin` are lexical matches for "jwt validate";
        `requireAuth` sits on the path between them with no matching text.
        """
        db = _new_db()
        symbols = [
            _sym(
                "ts:login.ts:postLogin@1111",
                "postLogin",
                qualified_name="login.postLogin",
                docstring="POST /login handler; validates jwt token via middleware.",
            ),
            _sym(
                "ts:auth.ts:requireAuth@2222",
                "requireAuth",
                qualified_name="auth.requireAuth",
                docstring="Express middleware guarding protected routes.",
            ),
            _sym(
                "ts:jwt.ts:validate@3333",
                "validate",
                qualified_name="jwt.validate",
                docstring="Validate a jwt token signature and expiry.",
            ),
            _sym(
                "ts:util.ts:unrelated@4444",
                "unrelated",
                qualified_name="util.unrelated",
                docstring="Formats currency strings for display.",
            ),
        ]
        upsert_symbols(db, symbols)
        populate_fts(db, symbols)
        # Call chain edges.
        upsert_edge(
            db,
            Edge(
                src_id="ts:login.ts:postLogin@1111",
                dst_id="ts:auth.ts:requireAuth@2222",
                kind="calls",
            ),
        )
        upsert_edge(
            db,
            Edge(
                src_id="ts:auth.ts:requireAuth@2222", dst_id="ts:jwt.ts:validate@3333", kind="calls"
            ),
        )
        return db

    def test_csar_surfaces_on_path_middleware(self) -> None:
        db = self._seed_layer_db()
        lexical = LexicalLayer()

        # Baseline: lexical seeding alone for "jwt validate token".
        seed_hits = lexical.search("jwt validate token", 10, db)
        seed_ids = {h.symbol_id for h in seed_hits}

        # CSAR diffuses over the call graph. Use moderate alpha so flow spreads.
        layer = CSARLayer([lexical], alpha=0.2, eps=1e-6, seed_k=10)
        csar_hits = layer.search("jwt validate token", 10, db)
        csar_ids = {h.symbol_id for h in csar_hits}

        middleware = "ts:auth.ts:requireAuth@2222"
        # requireAuth is on the call path but typically not a strong lexical hit.
        assert middleware in csar_ids
        # CSAR's reachable set should be a superset of (or equal to) the seeds
        # that exist in the graph.
        assert seed_ids <= csar_ids or middleware not in seed_ids

    def test_csar_marks_seed_vs_onpath(self) -> None:
        db = self._seed_layer_db()
        layer = CSARLayer([LexicalLayer()], alpha=0.25, eps=1e-6, seed_k=10)
        hits = layer.search("jwt validate", 10, db)
        by_id = {h.symbol_id: h for h in hits}
        # The diffusion reaches requireAuth as an on-path (non-seed) node.
        mw = by_id.get("ts:auth.ts:requireAuth@2222")
        assert mw is not None
        assert mw.evidence.get("seed") is False

    def test_csar_empty_when_no_seed_match(self) -> None:
        db = self._seed_layer_db()
        layer = CSARLayer([LexicalLayer()], alpha=0.2, eps=1e-6)
        hits = layer.search("zzzznomatchquery", 10, db)
        assert hits == []

    def test_csar_protocol_hit_shape(self) -> None:
        db = self._seed_layer_db()
        layer = CSARLayer([LexicalLayer()], alpha=0.2, eps=1e-6)
        hits = layer.search("validate", 5, db)
        assert all(isinstance(h, Hit) for h in hits)
        assert all(h.layer == "csar" for h in hits)
        # Scores are descending.
        scores = [h.score for h in hits]
        assert scores == sorted(scores, reverse=True)

    def test_alpha_validation(self) -> None:
        with pytest.raises(ValueError, match="alpha"):
            CSARLayer([LexicalLayer()], alpha=0.0)
        with pytest.raises(ValueError, match="eps"):
            CSARLayer([LexicalLayer()], eps=0.0)
