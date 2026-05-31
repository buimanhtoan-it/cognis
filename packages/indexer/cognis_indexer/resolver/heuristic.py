"""Heuristic edge resolver for the cognis indexer pipeline.

Resolves caller/callee edges without a running language server by inspecting
``ParsedSymbol.body_excerpt`` for identifier references.

Resolution phases (in priority order):

1. **Same-file** — callee is in the same ``file_path`` as caller.
   Confidence: **1.0** (unambiguous within module scope).
2. **Cross-module name match** — callee is in a different file but shares the
   same ``language``.  Confidence: **0.6**.
3. **Fuzzy match** — callee ``name`` is a prefix of an identifier found in the
   caller body (``startswith``), any language.  Confidence: **0.4**.

Design reference: *Indexer Pipeline → Resolver* (design.md) and task 8.1.
"""

from __future__ import annotations

import os
import re
from typing import Any

from cognis_indexer.resolver.base import ResolvedEdge

# Regex that extracts Python-style identifiers from source text.
# Matches token boundaries so ``foo`` inside ``foobar`` is NOT a match.
_IDENTIFIER_RE: re.Pattern[str] = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\b")

# Confidence constants per task spec.
_CONF_SAME_FILE: float = 1.0
_CONF_CROSS_MODULE: float = 0.6
_CONF_FUZZY: float = 0.4
_FUZZY_PREFIX_MIN_LEN: int = max(
    1, int(os.environ.get("COGNIS_RESOLVER_FUZZY_PREFIX_MIN_LEN", "3"))
)
_FUZZY_NAME_SCAN_LIMIT: int = max(
    0, int(os.environ.get("COGNIS_RESOLVER_FUZZY_NAME_SCAN_LIMIT", "5000"))
)

# Threshold below which an edge is flagged ambiguous (design Resolved Open
# Questions → "Edge confidence threshold").
_AMBIGUOUS_THRESHOLD: float = 0.6


def _is_ambiguous(confidence: float) -> bool:
    return confidence < _AMBIGUOUS_THRESHOLD


class HeuristicResolver:
    """Resolve edges using three-phase heuristic name matching.

    The resolver builds a name-to-symbol index from the full parse batch,
    then for each symbol scans its ``body_excerpt`` for identifiers that match
    another symbol's ``name``.

    Self-loops are always suppressed.  When multiple phases would produce an
    edge for the same ``(src_id, dst_id, kind)`` triple, only the highest-
    confidence edge is kept (deduplicated by PK).

    Usage::

        resolver = HeuristicResolver()
        edges = resolver.resolve(parsed_symbols)
    """

    def resolve(self, symbols: list[Any]) -> list[ResolvedEdge]:
        """Return resolved edges for the batch of *symbols*.

        Args:
            symbols: List of :class:`cognis_indexer.parsers.base.ParsedSymbol`
                instances (typed as ``Any`` to avoid circular imports; duck-
                typed by attribute access).

        Returns:
            Deduplicated list of :class:`ResolvedEdge` with confidence set per
            the three-phase scoring rules.
        """
        if not symbols:
            return []

        # Build lookup indices -------------------------------------------------
        # name → list[symbol]  (multiple symbols can share a short name)
        name_to_symbols: dict[str, list[Any]] = {}
        for sym in symbols:
            name_to_symbols.setdefault(sym.name, []).append(sym)

        # Best edge per (src_id, dst_id, kind) — keep highest confidence only.
        # Key: (src_id, dst_id, kind), Value: ResolvedEdge
        best: dict[tuple[str, str, str], ResolvedEdge] = {}
        name_items = tuple(name_to_symbols.items())
        fuzzy_enabled = _FUZZY_NAME_SCAN_LIMIT == 0 or len(name_items) <= _FUZZY_NAME_SCAN_LIMIT

        for caller in symbols:
            excerpt: str = caller.body_excerpt or ""
            if not excerpt:
                continue

            identifiers: set[str] = set(_IDENTIFIER_RE.findall(excerpt))
            if not identifiers:
                continue

            # --- Phase 1 & 2: exact name match --------------------------------
            for ident in identifiers:
                candidates = name_to_symbols.get(ident)
                if not candidates:
                    continue
                for callee in candidates:
                    if callee.id == caller.id:
                        continue  # no self-loops

                    confidence = _score_exact(caller, callee)
                    _merge_edge(best, caller.id, callee.id, "calls", confidence)

            # --- Phase 3: fuzzy (startswith) match ----------------------------
            if fuzzy_enabled:
                for ident in identifiers:
                    if len(ident) < _FUZZY_PREFIX_MIN_LEN:
                        continue
                    for name, candidates in name_items:
                        # Skip if the identifier IS the name (already handled above)
                        # or if the name doesn't start with the identifier.
                        if name == ident or not name.startswith(ident):
                            continue
                        for callee in candidates:
                            if callee.id == caller.id:
                                continue
                            # Only add fuzzy if no better edge already present.
                            key = (caller.id, callee.id, "calls")
                            if key not in best:
                                _merge_edge(
                                    best,
                                    caller.id,
                                    callee.id,
                                    "calls",
                                    _CONF_FUZZY,
                                )

        return list(best.values())


def _score_exact(caller: Any, callee: Any) -> float:
    """Return confidence for an exact name match between *caller* and *callee*."""
    if caller.file_path == callee.file_path:
        return _CONF_SAME_FILE
    # Cross-module: require same language for the 0.6 tier; fall to fuzzy
    # if languages differ (rare, but possible in polyglot repos).
    if caller.language == callee.language:
        return _CONF_CROSS_MODULE
    return _CONF_FUZZY


def _merge_edge(
    best: dict[tuple[str, str, str], ResolvedEdge],
    src_id: str,
    dst_id: str,
    kind: str,
    confidence: float,
) -> None:
    """Insert or upgrade the best edge for *(src_id, dst_id, kind)*."""
    key = (src_id, dst_id, kind)
    existing = best.get(key)
    if existing is None or confidence > existing.confidence:
        best[key] = ResolvedEdge(
            src_id=src_id,
            dst_id=dst_id,
            kind="calls",
            confidence=confidence,
            ambiguous=_is_ambiguous(confidence),
        )


__all__ = ["HeuristicResolver"]
