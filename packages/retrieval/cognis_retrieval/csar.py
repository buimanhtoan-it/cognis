"""CSAR — Code Spreading-Activation Retrieval.

A retrieval operator that unifies cognis's semantic and structural layers by
diffusing a cheap *seed* relevance distribution (FTS5 + a small semantic top-k)
over the Unified Code Knowledge Graph (UCKG) using **Personalized PageRank**
(random walk with restart).

Mathematical model
-------------------
Graph ``G = (V, E)`` with ``n = |V|`` symbols, column-stochastic transition
matrix ``P``, restart probability ``alpha ∈ (0, 1]``, and a seed probability
vector ``s`` (``s >= 0``, ``‖s‖₁ = 1``). The CSAR score vector ``r`` is the
unique fixed point of::

    r = alpha * s + (1 - alpha) * P @ r                      (PPR equation)
    r = alpha * (I - (1 - alpha) * P)^{-1} @ s               (closed form)

See ``docs/csar.md`` for the full statements and proofs of:

- **T1** existence/uniqueness (Neumann series, ``rho((1-alpha)P) < 1``),
- **T2** geometric convergence of power iteration (rate ``1 - alpha``),
- **T3** mass conservation (``‖r‖₁ = ‖s‖₁``),
- **T4** endpoint limits (``alpha→1`` ⇒ ``r=s``; ``alpha→0`` ⇒ stationary),
- **T5** Andersen-Chung-Lang forward-push correctness and the
  *repo-size-independent* work bound ``Σ d_u ≤ 1/(alpha·eps)``.

This module implements the graph builder, three PPR solvers (exact, power
iteration, and forward-push), and :class:`CSARLayer`, which satisfies the
:class:`~cognis_retrieval.base.RetrievalLayer` protocol.

Design reference: ``docs/csar.md``. Requirements: extends REQ-RET-2/REQ-RET-3.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field

import numpy as np
from cognis.db import Database
from numpy.typing import NDArray

from cognis_retrieval.base import Hit

__all__ = [
    "CSARLayer",
    "CodeGraph",
    "PushResult",
    "approximate_ppr_push",
    "build_code_graph",
    "build_seed_distribution",
    "diffuse_seed_hits",
    "personalized_pagerank_exact",
    "personalized_pagerank_power",
    "transition_matrix",
]

# Default diffusion parameters. ``alpha`` interpolates semantic (->1) and
# structural (->0); ``eps`` trades approximation accuracy for the push work
# bound 1/(alpha*eps).
DEFAULT_ALPHA: float = 0.15
DEFAULT_EPS: float = 1e-5
DEFAULT_TOL: float = 1e-10
DEFAULT_MAX_ITER: int = 1000


# ---------------------------------------------------------------------------
# Graph model
# ---------------------------------------------------------------------------


@dataclass
class CodeGraph:
    """A symmetrized, weighted code graph extracted from the UCKG.

    Attributes:
        node_ids: Ordered list of symbol ids; index ``i`` is node ``i``.
        index: Inverse map ``symbol_id -> node index``.
        adjacency: ``adjacency[u]`` is a list of ``(v, weight)`` neighbor pairs
            for node ``u``. The graph is undirected (symmetric): if ``(v, w)``
            is in ``adjacency[u]`` then ``(u, w)`` is in ``adjacency[v]``.
            Isolated nodes carry a single self-loop ``(u, 1.0)`` so the
            transition matrix stays column-stochastic and mass is conserved.
        degree: ``degree[u]`` is the (weighted) sum of incident edge weights,
            i.e. the column sum of the adjacency matrix.
    """

    node_ids: list[str]
    index: dict[str, int]
    adjacency: list[list[tuple[int, float]]]
    degree: NDArray[np.float64] = field(repr=False)

    @property
    def n(self) -> int:
        """Number of nodes."""
        return len(self.node_ids)


def build_code_graph(
    db: Database,
    *,
    edge_kinds: Sequence[str] | None = None,
) -> CodeGraph:
    """Build a symmetrized, weighted :class:`CodeGraph` from the UCKG.

    Nodes are every row in ``symbol``. Edges are ``edge`` rows whose
    ``meta.dst_missing`` flag is not set (matching the structural layer's
    traversal filter), weighted by ``edge.confidence``. The directed graph is
    symmetrized so diffusion reaches both callers and callees of a seed; this is
    what lets CSAR recover the *full flow* around a relevant region. Edge
    weights between the same pair accumulate (parallel ``calls`` + ``imports``
    edges reinforce each other). Isolated nodes get a self-loop.

    Args:
        db: The database to read ``symbol`` and ``edge`` from.
        edge_kinds: Optional whitelist of edge kinds to include. ``None`` uses
            all kinds.

    Returns:
        A :class:`CodeGraph`.
    """
    conn = db.connect()

    node_ids: list[str] = [str(row["id"]) for row in conn.execute("SELECT id FROM symbol")]
    index: dict[str, int] = {sid: i for i, sid in enumerate(node_ids)}
    n = len(node_ids)

    # Accumulate symmetric weights in a per-node dict to coalesce parallel edges.
    acc: list[dict[int, float]] = [dict() for _ in range(n)]

    kind_filter = set(edge_kinds) if edge_kinds is not None else None

    rows = conn.execute(
        "SELECT src_id, dst_id, kind, confidence, "
        "COALESCE(json_extract(meta, '$.dst_missing'), 0) AS dst_missing FROM edge"
    ).fetchall()

    for row in rows:
        if int(row["dst_missing"] or 0) == 1:
            continue
        if kind_filter is not None and str(row["kind"]) not in kind_filter:
            continue
        u = index.get(str(row["src_id"]))
        v = index.get(str(row["dst_id"]))
        if u is None or v is None or u == v:
            # Skip dangling endpoints and self-edges (self-loops are added below
            # only for genuinely isolated nodes).
            continue
        w = float(row["confidence"]) if row["confidence"] is not None else 1.0
        if w <= 0.0:
            continue
        acc[u][v] = acc[u].get(v, 0.0) + w
        acc[v][u] = acc[v].get(u, 0.0) + w

    adjacency: list[list[tuple[int, float]]] = []
    degree = np.zeros(n, dtype=np.float64)
    for u in range(n):
        neighbors = acc[u]
        if not neighbors:
            # Isolated node: self-loop keeps P column-stochastic (mass stays put).
            adjacency.append([(u, 1.0)])
            degree[u] = 1.0
        else:
            items = sorted(neighbors.items())
            adjacency.append([(v, w) for v, w in items])
            degree[u] = float(sum(w for _, w in items))

    return CodeGraph(node_ids=node_ids, index=index, adjacency=adjacency, degree=degree)


def transition_matrix(graph: CodeGraph) -> NDArray[np.float64]:
    """Return the dense column-stochastic transition matrix ``P = A·D⁻¹``.

    Column ``j`` holds the out-distribution of node ``j`` and sums to 1. Dense;
    intended for exact/power solvers and verification on small graphs.

    Args:
        graph: The code graph.

    Returns:
        ``(n, n)`` float64 array; ``P[i, j] = A[i, j] / degree[j]``.
    """
    n = graph.n
    matrix = np.zeros((n, n), dtype=np.float64)
    for u in range(n):
        d_u = graph.degree[u]
        if d_u <= 0.0:
            continue
        for v, w in graph.adjacency[u]:
            # Edge (u, v) with weight w contributes to column u (mass leaving u
            # toward v) -> row v, col u. Symmetric graph, so also handled when
            # iterating node v, but we set both ends explicitly for clarity.
            matrix[v, u] += w / d_u
    return matrix


# ---------------------------------------------------------------------------
# Exact and power-iteration solvers
# ---------------------------------------------------------------------------


def personalized_pagerank_exact(
    matrix: NDArray[np.float64],
    seed: NDArray[np.float64],
    alpha: float,
) -> NDArray[np.float64]:
    """Solve the PPR equation exactly: ``r = alpha·(I - (1-alpha)P)⁻¹·s``.

    Args:
        matrix: Column-stochastic transition matrix ``P`` (``(n, n)``).
        seed: Seed vector ``s`` (``(n,)``).
        alpha: Restart probability in ``(0, 1]``.

    Returns:
        The exact score vector ``r`` (``(n,)``).

    Raises:
        ValueError: If ``alpha`` is outside ``(0, 1]``.
    """
    if not 0.0 < alpha <= 1.0:
        raise ValueError(f"alpha must be in (0, 1]; got {alpha}")
    n = matrix.shape[0]
    identity = np.eye(n, dtype=np.float64)
    operator = identity - (1.0 - alpha) * matrix
    solution: NDArray[np.float64] = np.linalg.solve(operator, alpha * seed).astype(
        np.float64, copy=False
    )
    return solution


def personalized_pagerank_power(
    matrix: NDArray[np.float64],
    seed: NDArray[np.float64],
    alpha: float,
    *,
    tol: float = DEFAULT_TOL,
    max_iter: int = DEFAULT_MAX_ITER,
) -> tuple[NDArray[np.float64], int]:
    """Solve the PPR equation by power iteration ``r ← alpha·s + (1-alpha)P·r``.

    Converges geometrically at rate ``1 - alpha`` (Theorem 2), independent of
    ``n``.

    Args:
        matrix: Column-stochastic transition matrix ``P``.
        seed: Seed vector ``s``.
        alpha: Restart probability in ``(0, 1]``.
        tol: L1 convergence threshold on successive iterates.
        max_iter: Hard iteration cap.

    Returns:
        ``(r, iterations)``.

    Raises:
        ValueError: If ``alpha`` is outside ``(0, 1]``.
    """
    if not 0.0 < alpha <= 1.0:
        raise ValueError(f"alpha must be in (0, 1]; got {alpha}")
    r = seed.astype(np.float64).copy()
    iterations = 0
    for step in range(1, max_iter + 1):
        iterations = step
        nxt = alpha * seed + (1.0 - alpha) * (matrix @ r)
        if float(np.abs(nxt - r).sum()) <= tol:
            r = nxt
            break
        r = nxt
    return r, iterations


# ---------------------------------------------------------------------------
# Forward-push (Andersen-Chung-Lang) — the size-independent solver
# ---------------------------------------------------------------------------


@dataclass
class PushResult:
    """Result of :func:`approximate_ppr_push`.

    Attributes:
        estimate: ``{node -> p_node}`` approximate PPR mass (sparse).
        residual: ``{node -> r_node}`` leftover residual at termination.
        work: Total work ``Σ d_u`` summed over pushes (weighted degree). Bounded
            by ``1/(alpha·eps)`` (Theorem 5c), independent of graph size.
        pushes: Number of push operations performed.
    """

    estimate: dict[int, float]
    residual: dict[int, float]
    work: float
    pushes: int


def approximate_ppr_push(
    graph: CodeGraph,
    seed: Mapping[int, float],
    alpha: float,
    eps: float,
) -> PushResult:
    """Approximate PPR via Andersen-Chung-Lang forward push.

    Maintains an estimate ``p`` and residual ``r`` (both sparse), starting from
    ``p = 0``, ``r = seed``. Repeatedly pushes from any node ``u`` whose residual
    exceeds ``eps·degree[u]``::

        p[u] += alpha * r[u]
        for v, w in neighbors(u):
            r[v] += (1 - alpha) * r[u] * w / degree[u]
        r[u] = 0

    Guarantees (proved in ``docs/csar.md``):

    - **Invariant** ``ppr(seed) = p + ppr(residual)`` after every push (T5a).
    - **Termination bound** ``‖ppr(seed) - p‖₁ = ‖residual‖₁`` (T5b).
    - **Work bound** ``Σ d_u ≤ 1/(alpha·eps)`` — *independent of n* (T5c).

    Args:
        graph: The code graph (adjacency + degree).
        seed: Sparse seed vector ``{node -> mass}`` (``mass >= 0``).
        alpha: Restart probability in ``(0, 1]``.
        eps: Residual threshold; smaller is more accurate but more work.

    Returns:
        A :class:`PushResult`.

    Raises:
        ValueError: If ``alpha`` not in ``(0, 1]`` or ``eps <= 0``.
    """
    if not 0.0 < alpha <= 1.0:
        raise ValueError(f"alpha must be in (0, 1]; got {alpha}")
    if eps <= 0.0:
        raise ValueError(f"eps must be > 0; got {eps}")

    estimate: dict[int, float] = {}
    residual: dict[int, float] = {u: float(m) for u, m in seed.items() if m != 0.0}
    degree = graph.degree

    # Worklist of nodes whose residual currently exceeds the push threshold.
    active: list[int] = [u for u, r in residual.items() if r >= eps * degree[u]]
    in_active: set[int] = set(active)

    work = 0.0
    pushes = 0

    while active:
        u = active.pop()
        in_active.discard(u)
        r_u = residual.get(u, 0.0)
        if r_u < eps * degree[u]:
            continue

        estimate[u] = estimate.get(u, 0.0) + alpha * r_u
        residual[u] = 0.0
        push_mass = (1.0 - alpha) * r_u
        d_u = degree[u]

        for v, w in graph.adjacency[u]:
            residual[v] = residual.get(v, 0.0) + push_mass * w / d_u
            if v not in in_active and residual[v] >= eps * degree[v]:
                active.append(v)
                in_active.add(v)

        work += d_u
        pushes += 1

    residual = {u: r for u, r in residual.items() if r != 0.0}
    return PushResult(estimate=estimate, residual=residual, work=work, pushes=pushes)


# ---------------------------------------------------------------------------
# CSARLayer — RetrievalLayer protocol implementation
# ---------------------------------------------------------------------------


def build_seed_distribution(
    hits_per_layer: Sequence[list[Hit]],
    graph: CodeGraph,
) -> dict[int, float]:
    """Fuse per-layer hits into a normalized sparse seed distribution.

    Each layer's hit scores are min-max normalized to ``[0, 1]`` (so layers with
    different score scales — BM25 vs cosine — contribute comparably), summed per
    symbol, then the whole vector is L1-normalized over nodes present in the
    graph. Returns ``{}`` when there is no usable seed mass.

    Args:
        hits_per_layer: One list of :class:`Hit` per seed layer.
        graph: The code graph whose node index resolves symbol ids.

    Returns:
        ``{node_index -> probability_mass}`` summing to 1, or ``{}`` if empty.
    """
    raw: dict[int, float] = {}
    for hits in hits_per_layer:
        if not hits:
            continue
        scores = [h.score for h in hits]
        lo = min(scores)
        hi = max(scores)
        span = hi - lo
        for h in hits:
            node = graph.index.get(h.symbol_id)
            if node is None:
                continue
            # Normalize within the layer; constant layers contribute 1.0 each.
            norm = 1.0 if span <= 0.0 else (h.score - lo) / span
            if norm <= 0.0:
                # Keep a small floor so a present-but-lowest hit still seeds mass.
                norm = 1e-3
            raw[node] = raw.get(node, 0.0) + norm

    total = sum(raw.values())
    if total <= 0.0:
        return {}
    return {node: mass / total for node, mass in raw.items()}


def diffuse_seed_hits(
    graph: CodeGraph,
    hits_per_layer: Sequence[list[Hit]],
    *,
    k: int,
    alpha: float = DEFAULT_ALPHA,
    eps: float = DEFAULT_EPS,
) -> list[Hit]:
    """Diffuse seed hits over *graph* and return the top-*k* CSAR hits.

    Shared core used by :class:`CSARLayer` and the MCP ``diffuse_context`` tool.
    It builds a seed distribution from *hits_per_layer*, runs forward-push
    Personalized PageRank, and ranks symbols by diffused mass. Each returned
    :class:`Hit` is tagged in ``evidence`` with whether it was an original seed
    match or recovered via code flow.

    Args:
        graph: The code graph to diffuse over.
        hits_per_layer: One list of seed :class:`Hit` per seed layer.
        k: Maximum number of hits to return.
        alpha: Restart probability in ``(0, 1]``.
        eps: Forward-push residual threshold.

    Returns:
        Top-*k* :class:`Hit` (``layer="csar"``) by descending diffused score.
        Empty when there is no usable seed mass.
    """
    if graph.n == 0:
        return []
    seed = build_seed_distribution(hits_per_layer, graph)
    if not seed:
        return []

    push = approximate_ppr_push(graph, seed, alpha, eps)
    if not push.estimate:
        return []

    seed_nodes = set(seed.keys())
    ranked = sorted(push.estimate.items(), key=lambda kv: (-kv[1], graph.node_ids[kv[0]]))

    hits: list[Hit] = []
    for node, score in ranked[:k]:
        symbol_id = graph.node_ids[node]
        on_path = node not in seed_nodes
        reason = f"CSAR diffusion score {score:.6f}" + (
            " (reached via code flow)" if on_path else " (seed match)"
        )
        hits.append(
            Hit(
                symbol_id=symbol_id,
                score=float(score),
                layer="csar",
                reason=reason,
                evidence={"ppr": float(score), "seed": not on_path, "alpha": alpha},
            )
        )
    return hits


class CSARLayer:
    """Spreading-activation retrieval layer (Personalized PageRank over the UCKG).

    Seeds a relevance distribution from one or more *seed layers* (typically
    :class:`~cognis_retrieval.lexical.LexicalLayer` and
    :class:`~cognis_retrieval.semantic.SemanticLayer`), then diffuses it across
    the code graph so structurally adjacent symbols (callers/callees on the same
    flow) are surfaced even when they are weak lexical/semantic matches.

    By default it uses the forward-push solver, whose work is bounded by
    ``1/(alpha·eps)`` *independent of repository size* (Theorem 5c).

    Args:
        seed_layers: Layers used to build the seed distribution. Each must
            satisfy the ``search(query, k, db) -> list[Hit]`` protocol.
        alpha: Restart probability in ``(0, 1]``. Lower diffuses farther
            (more structural); higher stays near the seeds (more semantic).
        eps: Forward-push residual threshold.
        seed_k: Per-layer top-k used to build seeds (kept small to stay cheap).
    """

    name: str = "csar"

    def __init__(
        self,
        seed_layers: Sequence[object],
        *,
        alpha: float = DEFAULT_ALPHA,
        eps: float = DEFAULT_EPS,
        seed_k: int = 20,
    ) -> None:
        if not 0.0 < alpha <= 1.0:
            raise ValueError(f"alpha must be in (0, 1]; got {alpha}")
        if eps <= 0.0:
            raise ValueError(f"eps must be > 0; got {eps}")
        self._seed_layers = list(seed_layers)
        self._alpha = alpha
        self._eps = eps
        self._seed_k = max(1, seed_k)

    def search(self, query: str, k: int, db: Database) -> list[Hit]:
        """Return the top-*k* symbols by diffused (CSAR) relevance.

        Args:
            query: Natural-language or structured query.
            k: Maximum number of hits.
            db: Database providing the UCKG.

        Returns:
            List of :class:`Hit` (``layer="csar"``) ordered by descending score.
            Empty when no seed layer produces a usable match.
        """
        hits_per_layer: list[list[Hit]] = []
        for layer in self._seed_layers:
            search_fn = getattr(layer, "search", None)
            if search_fn is None:
                continue
            try:
                layer_hits = search_fn(query, self._seed_k, db)
            except Exception:
                layer_hits = []
            if layer_hits:
                hits_per_layer.append(layer_hits)

        if not hits_per_layer:
            return []

        graph = build_code_graph(db)
        return diffuse_seed_hits(
            graph,
            hits_per_layer,
            k=k,
            alpha=self._alpha,
            eps=self._eps,
        )
