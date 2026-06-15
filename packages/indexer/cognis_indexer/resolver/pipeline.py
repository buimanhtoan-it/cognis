"""Edge resolution pipeline: orchestrate heuristic + LSP resolvers.

Implements task 8.4:

- :func:`resolve_edges` — run heuristic resolver (always) then LSP resolver
  (when detected), merge results keeping highest confidence per
  ``(src_id, dst_id, kind)``.
- :func:`persist_edges` — convert :class:`ResolvedEdge` instances to
  :class:`cognis.models.Edge` rows and call :func:`cognis.db.upsert_edges`.

Design reference: *Indexer Pipeline → Resolver* and *Resolved Open Questions →
"Edge confidence threshold"* ("Keep edges with confidence < 0.6 and flag
ambiguous=true.").
"""

from __future__ import annotations

import os
from typing import Any

from cognis.db import Database, upsert_edges
from cognis.models import Edge

from cognis_indexer.resolver.base import ResolvedEdge
from cognis_indexer.resolver.heuristic import HeuristicResolver
from cognis_indexer.resolver.lsp import LspResolver
from cognis_indexer.resolver.lsp import detect as lsp_detect
from cognis_indexer.resolver.oop import OOPRelationshipResolver

# Threshold below which an edge is flagged ambiguous in ``Edge.meta``.
# Aligns with design *Resolved Open Questions → "Edge confidence threshold"*.
_AMBIGUOUS_THRESHOLD: float = 0.6


def resolve_edges(
    symbols: list[Any],
    repo_root: str | os.PathLike[str] | None = None,
) -> list[ResolvedEdge]:
    """Resolve call/import edges for a parsed symbol batch.

    Always runs the :class:`~cognis_indexer.resolver.heuristic.HeuristicResolver`.
    Additionally runs the :class:`~cognis_indexer.resolver.lsp.LspResolver` when
    :func:`~cognis_indexer.resolver.lsp.detect` finds language-server config
    files under *repo_root*.

    When both resolvers emit an edge for the same ``(src_id, dst_id, kind)``
    triple the one with higher confidence is kept (LSP edges are preferred
    because they typically have ``confidence=1.0``).

    Args:
        symbols: List of :class:`cognis_indexer.parsers.base.ParsedSymbol`
            instances produced by the parser stage.
        repo_root: Root directory of the repository; used by LSP detection.
            When ``None`` (e.g. in tests), LSP detection is skipped.

    Returns:
        Deduplicated list of :class:`ResolvedEdge` ordered by
        ``(src_id, dst_id, kind)`` for deterministic output.
    """
    # Phase 1 — heuristic (always)
    heuristic = HeuristicResolver()
    heuristic_edges = heuristic.resolve(symbols)

    # Phase 1b — OOP relationships (C#/Java inherits/implements; no-op otherwise)
    oop_edges = OOPRelationshipResolver().resolve(symbols)

    # Phase 2 — LSP (when detected)
    lsp_edges: list[ResolvedEdge] = []
    if repo_root is not None and lsp_detect(repo_root):
        lsp_resolver = LspResolver()
        lsp_edges = lsp_resolver.resolve(symbols)

    # Merge: keep highest confidence per (src_id, dst_id, kind).
    best: dict[tuple[str, str, str], ResolvedEdge] = {}
    for edge in heuristic_edges + oop_edges + lsp_edges:
        key = (edge.src_id, edge.dst_id, edge.kind)
        existing = best.get(key)
        if existing is None or edge.confidence > existing.confidence:
            best[key] = edge

    return sorted(best.values(), key=lambda e: (e.src_id, e.dst_id, e.kind))


def persist_edges(db: Database, edges: list[ResolvedEdge]) -> None:
    """Persist *edges* to the ``edge`` table via :func:`cognis.db.upsert_edges`.

    Converts each :class:`ResolvedEdge` to a :class:`cognis.models.Edge`,
    setting ``meta["ambiguous"] = True`` when ``confidence < 0.6`` per the
    design *Resolved Open Questions → "Edge confidence threshold"* decision.

    Args:
        db: Open :class:`cognis.db.Database` handle.
        edges: Edges to persist.  Empty list is a no-op.
    """
    if not edges:
        return

    db_edges: list[Edge] = []
    for resolved in edges:
        meta = dict(resolved.meta)  # copy so we don't mutate the input
        if resolved.confidence < _AMBIGUOUS_THRESHOLD:
            meta["ambiguous"] = True
        db_edges.append(
            Edge(
                src_id=resolved.src_id,
                dst_id=resolved.dst_id,
                kind=resolved.kind,
                confidence=resolved.confidence,
                meta=meta,
            )
        )

    upsert_edges(db, db_edges)


__all__ = ["persist_edges", "resolve_edges"]
