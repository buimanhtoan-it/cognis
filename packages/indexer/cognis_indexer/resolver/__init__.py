"""Edge resolver subpackage for the cognis indexer pipeline.

Resolves caller/callee edges from parsed symbols using:

1. **Heuristic resolver** (always active):
   - Phase 1 — same-file scope walk → confidence 1.0
   - Phase 2 — cross-module name match → confidence 0.6
   - Phase 3 — fuzzy (startswith) match → confidence 0.4

2. **LSP resolver** (activated when a running language server is detected):
   - Sends ``textDocument/definition`` / ``textDocument/references`` requests.
   - MVP stub: returns an empty list; wiring deferred to post-MVP.

The main entry point is :func:`cognis_indexer.resolver.pipeline.resolve_edges`.

Public exports::

    from cognis_indexer.resolver import ResolvedEdge, EdgeResolver
    from cognis_indexer.resolver.pipeline import resolve_edges, persist_edges
"""

from cognis_indexer.resolver.base import EdgeResolver, ResolvedEdge

__all__ = ["EdgeResolver", "ResolvedEdge"]
