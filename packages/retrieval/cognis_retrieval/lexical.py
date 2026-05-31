"""Lexical retrieval layer — FTS5 BM25 search against ``symbol_fts``.

Implements task 12.1:

- :func:`rewrite_query` (re-exported from :mod:`.query_rewriter`) extracts
  identifiers, error tokens, TODO markers, and file-glob hints from a
  natural-language query.
- :class:`LexicalLayer` runs the resulting FTS5 query against ``symbol_fts``,
  hydrates matching ``symbol`` rows, and returns ``list[Hit]`` with
  ``evidence={"snippet": ...}`` populated by the FTS5 ``snippet()`` function.
- :func:`populate_fts` is a test/indexer helper that inserts rows into the
  contentless ``symbol_fts`` table (the main writer normally does this; here
  it is exposed for unit tests).

Design reference: *Retrieval Mesh → Lexical* (design.md).
Requirements: REQ-RET-1.
"""

from __future__ import annotations

import sqlite3
from typing import TYPE_CHECKING

from cognis.db import Database

from cognis_retrieval.base import Hit
from cognis_retrieval.query_rewriter import rewrite_query

if TYPE_CHECKING:
    from cognis.models import SymbolNode

__all__ = ["LexicalLayer", "populate_fts", "rewrite_query"]


# ---------------------------------------------------------------------------
# FTS helper — populate symbol_fts (used by tests and writer)
# ---------------------------------------------------------------------------


def populate_fts(db: Database, symbols: list[SymbolNode]) -> None:
    """Insert or replace *symbols* into the ``symbol_fts`` contentless table.

    The Writer normally calls this as part of the per-file transaction. This
    function is exposed for test fixtures and ad-hoc population.

    Args:
        db: The database to write to.
        symbols: Symbols to index in FTS5.
    """
    rows = [
        (
            s.id,
            s.name,
            s.qualified_name,
            s.signature or "",
            s.docstring or "",
            s.body_excerpt or "",
        )
        for s in symbols
    ]
    if not rows:
        return
    with db.write() as conn:
        conn.executemany(
            "INSERT OR REPLACE INTO symbol_fts"
            "(id, name, qualified_name, signature, docstring, body_excerpt) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            rows,
        )


# ---------------------------------------------------------------------------
# LexicalLayer
# ---------------------------------------------------------------------------


class LexicalLayer:
    """FTS5 BM25 retrieval layer.

    Searches the ``symbol_fts`` virtual table, hydrates ``symbol`` rows, and
    returns :class:`~cognis_retrieval.base.Hit` objects with a snippet in the
    evidence dict.

    Latency target: p95 < 50 ms on 500k-symbol fixture (REQ-RET-1).
    """

    name: str = "lexical"

    def search(self, query: str, k: int, db: Database) -> list[Hit]:
        """Return top-*k* lexical hits for *query*.

        The query is rewritten via :func:`rewrite_query` before being sent to
        FTS5.  An empty rewritten query (no useful tokens) returns an empty
        list immediately.

        Args:
            query: Natural-language query string.
            k: Maximum number of results to return.
            db: Database containing ``symbol_fts`` and ``symbol`` tables.

        Returns:
            List of :class:`Hit` objects ordered by BM25 rank (best first).
        """
        fts_query = rewrite_query(query)
        if not fts_query:
            return []

        conn = db.connect()
        try:
            rows = conn.execute(
                """
                SELECT
                    f.id,
                    snippet(symbol_fts, 1, '«', '»', '…', 20) AS snip,
                    f.rank
                FROM symbol_fts f
                WHERE symbol_fts MATCH ?
                ORDER BY rank
                LIMIT ?
                """,
                (fts_query, k),
            ).fetchall()
        except sqlite3.OperationalError:
            # FTS5 table missing or query syntax error — degrade gracefully.
            return []

        if not rows:
            return []

        # Build a set of symbol ids for the batch hydration query.
        id_map: dict[str, tuple[str, float]] = {}
        for row in rows:
            symbol_id = str(row["id"])
            snippet = str(row["snip"]) if row["snip"] else ""
            # FTS5 rank values are negative (lower = more relevant); invert for
            # our convention where higher score = better.
            rank = float(row["rank"]) if row["rank"] is not None else 0.0
            id_map[symbol_id] = (snippet, -rank)

        # Hydrate symbol rows in one query.
        placeholders = ",".join("?" * len(id_map))
        symbol_rows = conn.execute(
            f"SELECT id, kind, name, qualified_name FROM symbol WHERE id IN ({placeholders})",
            list(id_map.keys()),
        ).fetchall()

        hydrated: set[str] = set()
        hits: list[Hit] = []
        for sym_row in symbol_rows:
            sid = str(sym_row["id"])
            snippet, score = id_map[sid]
            hits.append(
                Hit(
                    symbol_id=sid,
                    score=score,
                    layer="lexical",
                    reason=f"FTS5 BM25 match: {sym_row['qualified_name']}",
                    evidence={"snippet": snippet},
                )
            )
            hydrated.add(sid)

        # Include any FTS hits whose symbol row is absent (edge case during
        # incremental index gaps) — still return them without hydration.
        for sid, (snippet, score) in id_map.items():
            if sid not in hydrated:
                hits.append(
                    Hit(
                        symbol_id=sid,
                        score=score,
                        layer="lexical",
                        reason="FTS5 BM25 match (symbol row not found)",
                        evidence={"snippet": snippet},
                    )
                )

        # Sort by descending score (best first) to maintain contract.
        hits.sort(key=lambda h: h.score, reverse=True)
        return hits[:k]
