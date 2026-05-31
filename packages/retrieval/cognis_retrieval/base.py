"""Base types for the cognis retrieval mesh.

Defines the :class:`Hit` dataclass and :class:`RetrievalLayer` Protocol that
all three MVP layers (lexical, semantic, structural) implement.

Design reference: *Retrieval Mesh* section of design.md.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable

from cognis.db import Database

__all__ = ["Hit", "RetrievalLayer"]


@dataclass
class Hit:
    """A single retrieval result from any layer.

    Attributes:
        symbol_id: The ID of the matching symbol (``<lang>:<path>:<qname>@<hash>``).
        score: Layer-specific relevance score (higher is better).
        layer: Which layer produced this hit: ``"lexical"``, ``"semantic"``,
            or ``"structural"``.
        reason: Short human-readable explanation of why this symbol matched.
        evidence: Layer-specific payload — e.g. ``{"snippet": "..."}`` for
            lexical, ``{"score": 0.87}`` for semantic, ``{"depth": 2}`` for
            structural.
    """

    symbol_id: str
    score: float
    layer: str
    reason: str
    evidence: dict[str, Any] = field(default_factory=dict)


@runtime_checkable
class RetrievalLayer(Protocol):
    """Protocol every retrieval layer must satisfy.

    Any object with a ``name`` attribute and a ``search`` method of the correct
    signature satisfies this protocol (structural subtyping — no inheritance
    required).
    """

    name: str
    """Layer identifier: ``"lexical"`` | ``"semantic"`` | ``"structural"``."""

    def search(self, query: str, k: int, db: Database) -> list[Hit]:
        """Search for the top-*k* symbols matching *query*.

        Args:
            query: Natural-language or structured query string.
            k: Maximum number of hits to return.
            db: The :class:`~cognis.db.Database` to query.

        Returns:
            List of :class:`Hit` objects ordered by descending score.
        """
        ...
