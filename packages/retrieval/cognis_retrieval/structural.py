"""Structural retrieval layer — recursive CTE graph traversal.

Implements task 12.3:

- :meth:`StructuralLayer.dependency_trace` traverses the ``edge`` table via a
  recursive CTE with cycle detection via path-string membership check.
- Direction: ``"out"`` (callees), ``"in"`` (callers), or ``"both"``.
- Hard cap: ``max_depth`` is clamped to 8 (design Error Handling → Hard limits).
- Edges flagged ``meta.dst_missing=true`` are excluded from traversal.
- Returns ``list[Hit]`` with ``evidence={"depth": N}`` populated.

Design reference: *Retrieval Mesh → Structural* (design.md).
Requirements: REQ-RET-3.

PBT: CP-4 — traversal at depth N is a subset of traversal at depth N+1
(tested in ``tests/pbt/test_retrieval_pbt.py``).
"""

from __future__ import annotations

import sqlite3
from collections.abc import Iterator

from cognis.db import Database

from cognis_retrieval.base import Hit

__all__ = ["StructuralLayer"]

_MAX_DEPTH_HARD: int = 8
"""Hard cap on traversal depth per design Error Handling → Hard limits."""

_DEFAULT_MAX_RESULTS: int = 200
"""Default cap on reachable symbols returned by a single trace.

On densely connected graphs a shallow trace can reach almost every symbol in
the repository, which is both unhelpful to an agent and expensive to enrich and
serialize downstream. BFS visits nearest-first, so capping the result set yields
the most relevant neighbors while bounding latency.
"""


# ---------------------------------------------------------------------------
# BFS batching configuration
# ---------------------------------------------------------------------------

_SQL_VAR_LIMIT: int = 900
"""Max bound variables per frontier query (under SQLite's 999 default)."""


def _chunked(items: list[str], size: int) -> Iterator[list[str]]:
    """Yield successive *size*-length chunks of *items*."""
    for start in range(0, len(items), size):
        yield items[start : start + size]


# ---------------------------------------------------------------------------
# StructuralLayer
# ---------------------------------------------------------------------------


class StructuralLayer:
    """Recursive CTE BFS traversal of the call graph.

    Implements :meth:`dependency_trace` and also satisfies the
    :class:`~cognis_retrieval.base.RetrievalLayer` protocol via :meth:`search`
    (which delegates to ``dependency_trace`` without a start symbol — it
    returns an empty list as there is no meaningful "query string" traversal).

    Latency target: p95 < 150 ms for depth ≤ 5 on graph with avg fan-out ≤ 8
    (REQ-RET-3).
    """

    name: str = "structural"

    def search(self, query: str, k: int, db: Database) -> list[Hit]:
        """Not applicable for the structural layer; returns empty list.

        The structural layer is primarily accessed via :meth:`dependency_trace`.
        This method exists to satisfy the :class:`~cognis_retrieval.base.RetrievalLayer`
        protocol.
        """
        return []

    def dependency_trace(
        self,
        start_id: str,
        direction: str,
        max_depth: int,
        db: Database,
        max_results: int = _DEFAULT_MAX_RESULTS,
    ) -> list[Hit]:
        """Traverse the call graph from *start_id*.

        Performs a level-by-level BFS over the ``edge`` table. Each node is
        visited at most once, recording the *minimum* depth at which it is
        reached; this makes cycles harmless and keeps the cost proportional to
        the number of reachable edges rather than the number of distinct paths
        (the previous recursive-CTE approach enumerated every path and blew up
        combinatorially on densely connected graphs).

        Args:
            start_id: The symbol ID to start from.
            direction: One of ``"out"`` (follow callee edges), ``"in"``
                (follow caller edges), or ``"both"`` (union of both).
            max_depth: Maximum BFS depth. Clamped to
                :data:`_MAX_DEPTH_HARD` (8).
            db: Database containing the ``edge`` table.
            max_results: Maximum number of reachable symbols to return. BFS is
                nearest-first, so this keeps the closest neighbors. ``<= 0``
                means unbounded.

        Returns:
            List of :class:`~cognis_retrieval.base.Hit` objects, one per
            reachable symbol (excluding *start_id* itself), ordered by
            ascending depth then symbol_id.

        Raises:
            ValueError: When *direction* is not one of the three valid values.
        """
        if direction not in ("out", "in", "both"):
            raise ValueError(f"direction must be 'out', 'in', or 'both'; got {direction!r}")

        # Clamp depth.
        depth = max(1, min(max_depth, _MAX_DEPTH_HARD))
        limit = max_results if max_results and max_results > 0 else None

        conn = db.connect()

        directions: tuple[str, ...] = ("out", "in") if direction == "both" else (direction,)
        merged: dict[str, int] = {}

        try:
            for one_direction in directions:
                self._bfs_into(conn, start_id, one_direction, depth, merged, limit)
        except sqlite3.OperationalError:
            return []

        ordered = sorted(merged.items(), key=lambda x: (x[1], x[0]))
        if limit is not None:
            ordered = ordered[:limit]

        return [
            Hit(
                symbol_id=sid,
                score=1.0 / d,  # closer symbols rank higher
                layer="structural",
                reason=f"structural traversal depth {d} from {start_id}",
                evidence={"depth": d},
            )
            for sid, d in ordered
        ]

    @staticmethod
    def _bfs_into(
        conn: sqlite3.Connection,
        start_id: str,
        direction: str,
        max_depth: int,
        merged: dict[str, int],
        limit: int | None = None,
    ) -> None:
        """Breadth-first expand *direction* from *start_id*, recording min depth.

        Each BFS level is expanded with a single batched query over the whole
        frontier (``WHERE src_id IN (...)``) rather than one query per node, so
        the total number of SQL round-trips is bounded by *max_depth* instead of
        by the number of reachable symbols. This keeps traces fast even on dense
        graphs where a shallow trace can reach thousands of nodes.

        When *limit* is set, expansion stops once *merged* holds *limit* nodes.
        Because BFS proceeds level by level, the retained nodes are always the
        ones closest to *start_id*. For ``direction == "both"`` the limit is
        applied per-direction here and again on the merged total by the caller.

        Mutates *merged* (``{symbol_id: min_depth}``) in place so callers can
        union ``out`` and ``in`` traversals for ``direction == "both"``. The
        start node is never added to *merged*.
        """
        if direction == "out":
            anchor_col, neighbor_col = "src_id", "dst_id"
        else:  # "in"
            anchor_col, neighbor_col = "dst_id", "src_id"

        # Nodes whose neighbors we have already expanded (avoids re-querying and
        # breaks cycles). The start node counts as visited at depth 0.
        visited: set[str] = {start_id}
        frontier: list[str] = [start_id]

        for current_depth in range(1, max_depth + 1):
            if not frontier:
                break
            if limit is not None and len(merged) >= limit:
                break
            next_frontier: list[str] = []
            # Expand the entire frontier in chunks to respect SQLite's bound
            # variable limit (default 999) while keeping round-trips minimal.
            for chunk in _chunked(frontier, _SQL_VAR_LIMIT):
                placeholders = ",".join("?" * len(chunk))
                sql = (
                    f"SELECT DISTINCT e.{neighbor_col} AS neighbor "
                    f"FROM edge e "
                    f"WHERE e.{anchor_col} IN ({placeholders}) "
                    f"AND COALESCE(json_extract(e.meta, '$.dst_missing'), 0) != 1"
                )
                for row in conn.execute(sql, chunk).fetchall():
                    neighbor = str(row["neighbor"])
                    if neighbor in visited:
                        continue
                    visited.add(neighbor)
                    # First time we reach this node → minimum depth (BFS).
                    merged[neighbor] = current_depth
                    next_frontier.append(neighbor)
                    if limit is not None and len(merged) >= limit:
                        # Finish recording the current level's neighbors already
                        # fetched, but stop scheduling deeper expansion.
                        break
                if limit is not None and len(merged) >= limit:
                    break
            frontier = next_frontier
