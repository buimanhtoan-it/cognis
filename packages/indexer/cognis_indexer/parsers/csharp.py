"""C# language parser using ``tree-sitter-c-sharp``.

Covered node types:
- ``class_declaration`` / ``struct_declaration`` / ``record_declaration`` → ``kind="class"``
- ``interface_declaration`` → ``kind="interface"``
- ``enum_declaration`` → ``kind="class"`` (no dedicated enum kind in the model)
- ``method_declaration`` / ``constructor_declaration`` → ``kind="method"``

Types nest: a method inside ``Outer.Inner`` is qualified ``Outer.Inner.method``.
Namespaces are walked through but do not prefix the qualified name (mirrors the
``ClassName.method`` convention used by the TypeScript parser).

Usage::

    from cognis_indexer.parsers.csharp import CSharpParser

    parser = CSharpParser()
    symbols = parser.parse(source_code, "src/Auth/JwtValidator.cs")

Requires ``tree-sitter>=0.22`` and ``tree-sitter-c-sharp>=0.21``.
"""

from __future__ import annotations

import os
from typing import Any

from cognis.models import SymbolKind

from cognis_indexer.parsers._normalize import content_hash, make_symbol_id
from cognis_indexer.parsers.base import ParsedSymbol

_BODY_EXCERPT_LIMIT = 1500
_LANG = "cs"

# Type-declaration node → emitted symbol kind + the keyword shown in signatures.
_TYPE_DECLS: dict[str, tuple[SymbolKind, str]] = {
    "class_declaration": ("class", "class"),
    "struct_declaration": ("class", "struct"),
    "record_declaration": ("class", "record"),
    "record_struct_declaration": ("class", "record struct"),
    "interface_declaration": ("interface", "interface"),
    "enum_declaration": ("class", "enum"),
}
_METHOD_DECLS: frozenset[str] = frozenset({"method_declaration", "constructor_declaration"})
_COMMENT_TYPES: frozenset[str] = frozenset({"comment"})


def _lazy_load() -> Any:
    try:
        import tree_sitter_c_sharp as ts_cs
        from tree_sitter import Language, Parser
    except ImportError as exc:
        raise ImportError(
            "C# parser requires 'tree-sitter' and 'tree-sitter-c-sharp'. "
            "Install them with: pip install tree-sitter>=0.22 tree-sitter-c-sharp>=0.21"
        ) from exc

    return Parser(Language(ts_cs.language()))


def _node_text(node: Any, source_bytes: bytes) -> str:
    return source_bytes[node.start_byte : node.end_byte].decode("utf-8", errors="replace")


def _module_from_path(file_path: str) -> str:
    """Convert ``src/Auth/JwtValidator.cs`` → ``src/Auth/JwtValidator``."""
    parts = file_path.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    return "/".join([*parts[:-1], stem]) if len(parts) > 1 else stem


def _name_of(node: Any, source_bytes: bytes) -> str | None:
    name_node = node.child_by_field_name("name")
    if name_node is None:
        return None
    return _node_text(name_node, source_bytes)


def _extract_signature(node: Any, source_bytes: bytes) -> str:
    """Return the declaration text up to the body (``{``) or terminator (``;``)."""
    full_text = _node_text(node, source_bytes)
    cut = len(full_text)
    for delimiter in ("{", "=>", ";"):
        idx = full_text.find(delimiter)
        if idx != -1:
            cut = min(cut, idx)
    return full_text[:cut].strip()[:256]


def _extract_docstring(node: Any, source_bytes: bytes) -> str | None:
    """Return the contiguous ``///`` / ``/* */`` comment block directly above *node*."""
    parent = node.parent
    if parent is None:
        return None
    siblings = parent.children
    try:
        idx = siblings.index(node)
    except ValueError:
        return None
    doc_lines: list[str] = []
    j = idx - 1
    while j >= 0 and siblings[j].type in _COMMENT_TYPES:
        text = _node_text(siblings[j], source_bytes).strip()
        if text.startswith("///"):
            doc_lines.insert(0, text[3:].strip())
        elif text.startswith("//"):
            doc_lines.insert(0, text[2:].strip())
        elif text.startswith("/*"):
            inner = text[2:].rstrip("/").rstrip("*").strip("*").strip()
            cleaned = [ln.lstrip(" *").strip() for ln in inner.splitlines()]
            doc_lines[0:0] = [ln for ln in cleaned if ln]
        j -= 1
    return "\n".join(doc_lines) if doc_lines else None


class CSharpParser:
    """Parser for C# ``.cs`` files.

    Instantiating this class eagerly loads ``tree-sitter-c-sharp``.
    Wrap in a ``try/except ImportError`` to handle the missing optional dep.
    """

    language: str = "csharp"

    def __init__(self) -> None:
        self._parser = _lazy_load()

    def parse(self, source: str, file_path: str) -> list[ParsedSymbol]:
        """Parse C# *source* and return a flat list of symbols (never raises)."""
        source_bytes = source.encode("utf-8")
        tree = self._parser.parse(source_bytes)
        module = _module_from_path(file_path)
        symbols: list[ParsedSymbol] = []
        self._walk(tree.root_node, source_bytes, file_path, module, [], symbols)
        return symbols

    def _walk(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        scope: list[str],
        symbols: list[ParsedSymbol],
    ) -> None:
        node_type = node.type

        if node_type in _TYPE_DECLS:
            kind, keyword = _TYPE_DECLS[node_type]
            name = _name_of(node, source_bytes)
            if name:
                self._emit(
                    node,
                    source_bytes,
                    file_path,
                    module,
                    scope,
                    name,
                    kind,
                    signature=f"{keyword} {name}",
                    symbols=symbols,
                )
                scope = [*scope, name]
            for child in node.children:
                self._walk(child, source_bytes, file_path, module, scope, symbols)
            return

        if node_type in _METHOD_DECLS:
            name = _name_of(node, source_bytes)
            if name:
                self._emit(
                    node,
                    source_bytes,
                    file_path,
                    module,
                    scope,
                    name,
                    "method",
                    signature=_extract_signature(node, source_bytes),
                    symbols=symbols,
                )
            return  # do not descend into method bodies

        for child in node.children:
            self._walk(child, source_bytes, file_path, module, scope, symbols)

    def _emit(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        scope: list[str],
        name: str,
        kind: SymbolKind,
        signature: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        qual = ".".join([*scope, name])
        body_text = _node_text(node, source_bytes)
        symbols.append(
            ParsedSymbol(
                id=make_symbol_id(_LANG, file_path, qual, body_text),
                kind=kind,
                name=name,
                qualified_name=f"{_LANG}:{file_path}:{qual}",
                language=self.language,
                module=module,
                file_path=file_path,
                line_start=node.start_point[0] + 1,
                line_end=node.end_point[0] + 1,
                signature=signature,
                docstring=_extract_docstring(node, source_bytes),
                content_hash=content_hash(body_text),
                body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
            )
        )
