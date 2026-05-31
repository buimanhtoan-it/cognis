"""Base dataclass and protocol for edge resolvers.

Defines:

- :class:`ResolvedEdge` — intermediate edge representation produced by a
  resolver before persistence.
- :class:`EdgeResolver` — structural-subtyping protocol; any object with a
  ``resolve(symbols) -> list[ResolvedEdge]`` method satisfies it.

Design reference: *Indexer Pipeline → Resolver* (design.md) and task 8
implementation spec.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable

from cognis.models import EdgeKind


@dataclass
class ResolvedEdge:
    """Intermediate edge representation produced by a resolver stage.

    This is the output of the *Resolver* stage of the indexer pipeline.  The
    *Writer* stage (via :func:`cognis_indexer.resolver.pipeline.persist_edges`)
    converts these into :class:`cognis.models.Edge` rows in the database.

    Attributes:
        src_id: ID of the calling / importing symbol.
        dst_id: ID of the called / imported symbol.
        kind: Edge type from :data:`cognis.models.EdgeKind`.
        confidence: Float in [0.0, 1.0].  ``1.0`` = unambiguous same-file
            resolution; ``0.6`` = cross-module name match; ``0.4`` = fuzzy.
        ambiguous: ``True`` when ``confidence < 0.6``.  Persisted in
            ``Edge.meta["ambiguous"]`` per design *Resolved Open Questions*
            ("Edge confidence threshold").
        meta: Free-form payload for extra resolver annotations.
    """

    src_id: str
    dst_id: str
    kind: EdgeKind
    confidence: float
    ambiguous: bool
    meta: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not 0.0 <= self.confidence <= 1.0:
            raise ValueError(f"confidence must be in [0.0, 1.0], got {self.confidence!r}")


@runtime_checkable
class EdgeResolver(Protocol):
    """Structural protocol for all edge resolvers.

    Any class with a ``resolve`` method satisfies this protocol — no
    inheritance required.
    """

    def resolve(self, symbols: list[Any]) -> list[ResolvedEdge]:
        """Resolve call/import edges from *symbols*.

        Args:
            symbols: A list of :class:`cognis_indexer.parsers.base.ParsedSymbol`
                instances (typed as ``Any`` to avoid a circular import; callers
                pass real ``ParsedSymbol`` objects).

        Returns:
            List of :class:`ResolvedEdge` instances.  Empty list when no edges
            can be determined.  Never raises.
        """
        ...


__all__ = ["EdgeResolver", "ResolvedEdge"]
