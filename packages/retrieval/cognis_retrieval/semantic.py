"""Semantic retrieval layer -- KNN against ``symbol_vec`` using sqlite-vec.

Implements task 12.2:

- Embeds the query using the same :class:`~cognis_indexer.embedder.Embedder`
  backend as the indexer, with an LRU cache of size 1 000 to avoid redundant
  model calls for repeated or similar queries.
- Performs KNN search against ``symbol_vec`` when sqlite-vec (``vec0``) is
  available; returns an empty list when only the plain-table fallback is
  present (no KNN support).
- Hydrates matching ``symbol`` rows and returns ``list[Hit]`` with
  ``evidence={"score": <cosine_distance>}``.

Design reference: *Retrieval Mesh -> Semantic* (design.md).
Requirements: REQ-RET-2.

PBT: CP-5 -- search by a symbol's own embedding text returns that symbol in
top-1 with high consistency (tested in ``tests/pbt/test_retrieval_pbt.py``).
"""

from __future__ import annotations

import sqlite3
import struct
from collections.abc import Callable
from functools import lru_cache
from typing import TYPE_CHECKING, Any

from cognis.db import Database

from cognis_retrieval.base import Hit, QueryEmbedder

# ``numpy`` ships under the ``embed-local`` extra. Import it lazily so importing
# ``cognis_retrieval`` (which re-exports this module) stays safe on a
# lexical+structural-only install. ``np`` is only used at runtime when actually
# embedding/serialising vectors, a path that requires an embedder (hence numpy)
# to be reachable in the first place.
if TYPE_CHECKING:
    import numpy as np
    from numpy.typing import NDArray
else:
    try:
        import numpy as np
    except ImportError:  # pragma: no cover - exercised only without embed-local
        np = None

__all__ = ["SemanticLayer", "populate_vec"]


# ---------------------------------------------------------------------------
# Vector serialisation helpers
# ---------------------------------------------------------------------------


def _vec_to_bytes(arr: NDArray[np.float32]) -> bytes:
    """Serialise a float32 numpy array to little-endian bytes for sqlite-vec."""
    flat = np.asarray(arr, dtype=np.float32).ravel()
    return struct.pack(f"{len(flat)}f", *flat.tolist())


# ---------------------------------------------------------------------------
# Helper: populate symbol_vec (tests + indexer writer)
# ---------------------------------------------------------------------------


def populate_vec(db: Database, symbol_id: str, embedding: NDArray[np.float32]) -> None:
    """Insert or replace one row in ``symbol_vec``.

    Works for both the ``vec0`` virtual table (when sqlite-vec is loaded) and
    the plain-BLOB fallback table.

    Args:
        db: Database to write to.
        symbol_id: The ``symbol.id`` this embedding belongs to.
        embedding: float32 numpy array of shape ``(dim,)``.
    """
    blob = _vec_to_bytes(embedding)

    with db.write() as conn:
        conn.execute(
            "INSERT OR REPLACE INTO symbol_vec(symbol_id, embedding) VALUES (?, ?)",
            (symbol_id, blob),
        )


# ---------------------------------------------------------------------------
# LRU-cached embed function factory
# ---------------------------------------------------------------------------


def _make_query_embed_fn(
    embedder: QueryEmbedder,
) -> Callable[[str], NDArray[np.float32]]:
    """Return a per-embedder LRU-cached embed function with capacity 1 000."""

    @lru_cache(maxsize=1_000)
    def _cached_embed(query: str) -> NDArray[np.float32]:
        return embedder.embed_text(query)

    return _cached_embed


# ---------------------------------------------------------------------------
# SemanticLayer
# ---------------------------------------------------------------------------


class SemanticLayer:
    """Semantic KNN retrieval using sqlite-vec.

    When sqlite-vec (``vec0``) is available the layer performs an approximate
    nearest-neighbour search. When only the plain fallback table is present the
    layer returns an empty list because BLOB storage has no KNN index.

    The query embedding is cached via an LRU with capacity 1 000 keyed on the
    raw query string, so repeated queries within the same process incur only
    one model call.

    Latency target: p95 < 100 ms on 500k vectors (REQ-RET-2).
    """

    name: str = "semantic"

    def __init__(self, embedder: Any) -> None:
        """Bind *embedder* and create per-instance LRU cache.

        Args:
            embedder: Any object satisfying the :class:`Embedder` protocol
                (``embed_text`` method + ``embedding_dim`` attribute).
        """
        self._embedder: QueryEmbedder = embedder
        self._embed_cached: Callable[[str], NDArray[np.float32]] = _make_query_embed_fn(embedder)

    def search(self, query: str, k: int, db: Database) -> list[Hit]:
        """Return top-*k* semantic hits for *query*.

        Args:
            query: Natural-language query string (embedded, then KNN-queried).
            k: Maximum number of results to return.
            db: Database containing ``symbol_vec`` and ``symbol`` tables.

        Returns:
            List of :class:`Hit` objects ordered by ascending KNN distance
            (closest first, i.e. descending cosine similarity) or empty list
            when vec0 is unavailable.
        """
        if not db.vec_enabled:
            # No KNN support without sqlite-vec.
            return []

        conn = db.connect()

        # Check whether symbol_vec is a vec0 virtual table.
        row_check = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type IN ('table','shadow') AND name='symbol_vec'"
        ).fetchone()
        if row_check is None or "USING vec0" not in str(row_check["sql"] or ""):
            return []

        query_vec = self._embed_cached(query)
        query_blob = _vec_to_bytes(query_vec)

        try:
            rows = conn.execute(
                """
                SELECT symbol_id, distance
                FROM symbol_vec
                WHERE embedding MATCH ?
                  AND k = ?
                """,
                (query_blob, k),
            ).fetchall()
        except sqlite3.OperationalError:
            # sqlite-vec not loaded or table shape unexpected.
            return []

        if not rows:
            return []

        id_distance: dict[str, float] = {
            str(row["symbol_id"]): float(row["distance"]) for row in rows
        }

        # Hydrate symbol rows.
        placeholders = ",".join("?" * len(id_distance))
        symbol_rows = conn.execute(
            f"SELECT id, kind, qualified_name FROM symbol WHERE id IN ({placeholders})",
            list(id_distance.keys()),
        ).fetchall()

        hydrated: set[str] = set()
        hits: list[Hit] = []
        for sym_row in symbol_rows:
            sid = str(sym_row["id"])
            dist = id_distance[sid]
            # Convert distance to a score: smaller distance -> higher score.
            # For cosine distance: score = 1 - distance.
            score = max(0.0, 1.0 - dist)
            hits.append(
                Hit(
                    symbol_id=sid,
                    score=score,
                    layer="semantic",
                    reason=f"KNN cosine distance {dist:.4f}: {sym_row['qualified_name']}",
                    evidence={"score": dist},
                )
            )
            hydrated.add(sid)

        # Include non-hydrated hits (symbol row absent) with whatever score we
        # can compute.
        for sid, dist in id_distance.items():
            if sid not in hydrated:
                score = max(0.0, 1.0 - dist)
                hits.append(
                    Hit(
                        symbol_id=sid,
                        score=score,
                        layer="semantic",
                        reason=f"KNN cosine distance {dist:.4f} (symbol row not found)",
                        evidence={"score": dist},
                    )
                )

        hits.sort(key=lambda h: h.score, reverse=True)
        return hits[:k]
