"""Language-aware OOP relationship resolver for C# and Java.

The heuristic resolver only emits ``calls`` edges from identifier scans. C# and
Java additionally carry explicit type relationships in the class/interface
header — ``: Base, IFoo`` (C#) and ``extends Base implements IFoo`` (Java).
Those become ``inherits`` / ``implements`` edges here, which enrich the code
graph CSAR diffuses over (``build_code_graph`` includes all edge kinds) and the
``dependency_trace`` traversal.

Resolution is name-based and conservative: an edge is only emitted when the
base/super type name resolves to a ``class`` or ``interface`` symbol present in
the same parse batch. External types (``System.Object``, ``java.lang.Object``,
third-party libraries) are not in the batch, so they produce no edges — no
noise. When a base name matches more than one in-repo type the edge is kept but
flagged ambiguous (confidence < 0.6).

Edge-kind decision:
- Java ``extends`` → ``inherits``; ``implements`` → ``implements``.
- C# (single ``:`` list, no keyword distinction) → ``implements`` when the
  resolved target is an ``interface``, otherwise ``inherits``.
"""

from __future__ import annotations

import re
from typing import Any

from cognis_indexer.resolver.base import ResolvedEdge

_LANGS: frozenset[str] = frozenset({"csharp", "java"})
_TYPE_KINDS: frozenset[str] = frozenset({"class", "interface"})

_CONF_UNIQUE: float = 0.9
_CONF_AMBIGUOUS: float = 0.5
_AMBIGUOUS_THRESHOLD: float = 0.6

_IDENT_RE: re.Pattern[str] = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def _header(text: str) -> str:
    """Return the declaration header — everything before the body ``{``."""
    idx = text.find("{")
    return text[:idx] if idx != -1 else text


def _strip_groups(text: str) -> str:
    """Remove balanced ``<...>`` (generics) and ``(...)`` (record params).

    Done iteratively from the innermost group so commas inside generic argument
    lists or primary-constructor parameters never split a base type entry.
    """
    pattern = re.compile(r"<[^<>]*>|\([^()]*\)")
    prev = None
    while prev != text:
        prev = text
        text = pattern.sub(" ", text)
    return text


def _trailing_name(entry: str) -> str | None:
    """Return the simple type name from a (possibly qualified) base entry."""
    idents = _IDENT_RE.findall(entry)
    return idents[-1] if idents else None


def _bases(header: str, language: str) -> list[tuple[str, str]]:
    """Return ``(simple_type_name, keyword)`` pairs from a type *header*.

    ``keyword`` is ``"extends"`` / ``"implements"`` (Java) or ``":"`` (C#).
    """
    cleaned = _strip_groups(header)
    out: list[tuple[str, str]] = []

    if language == "java":
        ext = re.search(r"\bextends\b(.*?)(\bimplements\b|$)", cleaned, re.DOTALL)
        if ext:
            for entry in ext.group(1).split(","):
                name = _trailing_name(entry)
                if name:
                    out.append((name, "extends"))
        impl = re.search(r"\bimplements\b(.*)$", cleaned, re.DOTALL)
        if impl:
            for entry in impl.group(1).split(","):
                name = _trailing_name(entry)
                if name:
                    out.append((name, "implements"))
        return out

    # C#: the base list is whatever follows the first ':' up to a 'where' clause.
    colon = cleaned.find(":")
    if colon != -1:
        base_part = cleaned[colon + 1 :]
        where = re.search(r"\bwhere\b", base_part)
        if where:
            base_part = base_part[: where.start()]
        for entry in base_part.split(","):
            name = _trailing_name(entry)
            if name:
                out.append((name, ":"))
    return out


def _edge_kind(keyword: str, dst: Any) -> str:
    """Map a base-list keyword + resolved target to an edge kind."""
    if keyword == "extends":
        return "inherits"
    if keyword == "implements":
        return "implements"
    # C# ':' — disambiguate by what the target actually is.
    return "implements" if dst.kind == "interface" else "inherits"


class OOPRelationshipResolver:
    """Resolve ``inherits`` / ``implements`` edges for C# and Java symbols.

    Usage::

        resolver = OOPRelationshipResolver()
        edges = resolver.resolve(parsed_symbols)
    """

    def resolve(self, symbols: list[Any]) -> list[ResolvedEdge]:
        """Return inheritance/implementation edges for the *symbols* batch.

        Never raises; returns an empty list when no relationships are found.
        """
        if not symbols:
            return []

        type_by_name: dict[str, list[Any]] = {}
        for sym in symbols:
            if sym.kind in _TYPE_KINDS:
                type_by_name.setdefault(sym.name, []).append(sym)

        best: dict[tuple[str, str, str], ResolvedEdge] = {}
        for sym in symbols:
            if sym.language not in _LANGS or sym.kind not in _TYPE_KINDS:
                continue
            header = _header(sym.body_excerpt or sym.signature or "")
            for base_name, keyword in _bases(header, sym.language):
                candidates = [c for c in type_by_name.get(base_name, []) if c.id != sym.id]
                if not candidates:
                    continue
                confidence = _CONF_UNIQUE if len(candidates) == 1 else _CONF_AMBIGUOUS
                for dst in candidates:
                    kind = _edge_kind(keyword, dst)
                    key = (sym.id, dst.id, kind)
                    existing = best.get(key)
                    if existing is None or confidence > existing.confidence:
                        best[key] = ResolvedEdge(
                            src_id=sym.id,
                            dst_id=dst.id,
                            kind=kind,  # type: ignore[arg-type]
                            confidence=confidence,
                            ambiguous=confidence < _AMBIGUOUS_THRESHOLD,
                        )
        return list(best.values())


__all__ = ["OOPRelationshipResolver"]
