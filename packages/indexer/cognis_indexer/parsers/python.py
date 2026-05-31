"""Python language parser using ``tree-sitter-python``.

Covered node types:
- ``function_definition`` (sync functions) — top-level and nested in classes
- ``decorated_definition`` containing a ``function_definition`` or
  ``class_definition`` — decorators are extracted
- ``class_definition`` and nested method definitions
- Top-level ``expression_statement`` whose value is an ``assignment`` where the
  target name is ``ALL_CAPS`` → ``kind="const"``

Usage::

    from cognis_indexer.parsers.python import PythonParser

    parser = PythonParser()
    symbols = parser.parse(source_code, "src/app/main.py")

Requires ``tree-sitter>=0.22`` and ``tree-sitter-python>=0.21``.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any

from cognis_indexer.parsers._normalize import content_hash, make_symbol_id
from cognis_indexer.parsers.base import ParsedSymbol

if TYPE_CHECKING:
    pass

_BODY_EXCERPT_LIMIT = 1500
_LANG = "py"


def _lazy_load() -> Any:
    try:
        import tree_sitter_python as ts_py
        from tree_sitter import Language, Parser
    except ImportError as exc:
        raise ImportError(
            "Python parser requires 'tree-sitter' and 'tree-sitter-python'. "
            "Install them with: pip install tree-sitter>=0.22 tree-sitter-python>=0.21"
        ) from exc

    py_language = Language(ts_py.language())
    parser = Parser(py_language)
    return parser


def _node_text(node: Any, source_bytes: bytes) -> str:
    return source_bytes[node.start_byte : node.end_byte].decode("utf-8", errors="replace")


def _module_from_path(file_path: str) -> str:
    """Convert ``src/app/main.py`` → ``src.app.main`` style module path."""
    parts = file_path.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    return "/".join([*parts[:-1], stem]) if len(parts) > 1 else stem


def _find_child(node: Any, *types: str) -> Any | None:
    for child in node.children:
        if child.type in types:
            return child
    return None


def _find_named_child(node: Any, field_name: str) -> Any | None:
    """Get a child by field name."""
    return node.child_by_field_name(field_name)


def _get_name(node: Any, source_bytes: bytes) -> str | None:
    """Extract identifier name from a def/class node."""
    # Try field-based access first
    name_node = node.child_by_field_name("name") if hasattr(node, "child_by_field_name") else None
    if name_node:
        return _node_text(name_node, source_bytes)
    # Fall back to direct child identifier
    ident = _find_child(node, "identifier")
    if ident:
        return _node_text(ident, source_bytes)
    return None


def _extract_docstring(node: Any, source_bytes: bytes) -> str | None:
    """Extract a Python docstring from the first statement in a function/class body."""
    body = node.child_by_field_name("body") if hasattr(node, "child_by_field_name") else None
    if body is None:
        body = _find_child(node, "block")
    if body is None:
        return None

    for stmt in body.children:
        if stmt.type == "expression_statement":
            expr = _find_child(stmt, "string")
            if expr:
                text = _node_text(expr, source_bytes)
                # Strip quotes
                for triple in ('"""', "'''"):
                    if text.startswith(triple) and text.endswith(triple):
                        return text[3:-3].strip()
                if text.startswith('"') and text.endswith('"'):
                    return text[1:-1].strip()
                if text.startswith("'") and text.endswith("'"):
                    return text[1:-1].strip()
                return text.strip()
        # The docstring must be the first real statement; stop at anything else
        if stmt.type not in ("comment", "\n", "pass_statement"):
            break
    return None


def _extract_decorators(node: Any, source_bytes: bytes) -> list[str]:
    """Extract decorator texts from a ``decorated_definition`` node."""
    decorators = []
    for child in node.children:
        if child.type == "decorator":
            decorators.append(_node_text(child, source_bytes).strip())
    return decorators


def _is_async_function(node: Any) -> bool:
    """Detect if a function_definition node is async by checking for 'async' keyword child."""
    if node.type == "async_function_definition":
        return True
    # In some tree-sitter-python versions, async def is still function_definition
    # with an 'async' child token
    return any(child.type == "async" for child in node.children)


def _extract_signature(node: Any, source_bytes: bytes, name: str) -> str:
    """Build a signature string for a function node."""
    params_node = (
        node.child_by_field_name("parameters") if hasattr(node, "child_by_field_name") else None
    )
    if params_node is None:
        params_node = _find_child(node, "parameters")

    is_async = _is_async_function(node)
    prefix = "async def" if is_async else "def"

    if params_node:
        params_text = _node_text(params_node, source_bytes)
        return f"{prefix} {name}{params_text}"
    return f"{prefix} {name}()"


def _is_all_caps(name: str) -> bool:
    """Return True if name is ALL_CAPS (at least 2 chars, all upper or underscore/digit)."""
    if len(name) < 2:
        return False
    return name == name.upper() and any(c.isalpha() for c in name)


class PythonParser:
    """Parser for Python ``.py`` files.

    Instantiating this class eagerly loads ``tree-sitter-python``.
    """

    language: str = "python"

    def __init__(self) -> None:
        self._parser = _lazy_load()

    def parse(self, source: str, file_path: str) -> list[ParsedSymbol]:
        """Parse Python *source* and return a flat list of symbols."""
        source_bytes = source.encode("utf-8")
        tree = self._parser.parse(source_bytes)
        module = _module_from_path(file_path)
        symbols: list[ParsedSymbol] = []
        self._visit_module(tree.root_node, source_bytes, file_path, module, symbols)
        return symbols

    # ------------------------------------------------------------------
    # Visitors
    # ------------------------------------------------------------------

    def _visit_module(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        """Visit top-level statements in a module."""
        for child in node.children:
            self._visit_top_level(child, source_bytes, file_path, module, symbols)

    def _visit_top_level(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        if node.type in ("function_definition", "async_function_definition"):
            sym = self._handle_function(node, source_bytes, file_path, module)
            if sym:
                symbols.append(sym)
        elif node.type == "decorated_definition":
            self._handle_decorated(node, source_bytes, file_path, module, symbols)
        elif node.type == "class_definition":
            syms = self._handle_class(node, source_bytes, file_path, module)
            symbols.extend(syms)
        elif node.type == "expression_statement":
            # Top-level assignment: check for ALL_CAPS constants
            # expression_statement -> assignment -> identifier = ...
            for child in node.children:
                if child.type == "assignment":
                    sym = self._handle_assignment(child, source_bytes, file_path, module)
                    if sym:
                        symbols.append(sym)
                        break

    def _handle_function(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        class_prefix: str | None = None,
        decorators: list[str] | None = None,
    ) -> ParsedSymbol | None:
        name = _get_name(node, source_bytes)
        if not name:
            return None

        body_text = _node_text(node, source_bytes)
        qual = f"{class_prefix}.{name}" if class_prefix else name
        qualified_name = f"{_LANG}:{file_path}:{qual}"
        sym_id = make_symbol_id(_LANG, file_path, qual, body_text)
        docstring = _extract_docstring(node, source_bytes)
        sig = _extract_signature(node, source_bytes, name)

        return ParsedSymbol(
            id=sym_id,
            kind="function",
            name=name,
            qualified_name=qualified_name,
            language=self.language,
            module=module,
            file_path=file_path,
            line_start=node.start_point[0] + 1,
            line_end=node.end_point[0] + 1,
            signature=sig,
            docstring=docstring,
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )

    def _handle_class(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        decorators: list[str] | None = None,
    ) -> list[ParsedSymbol]:
        symbols: list[ParsedSymbol] = []
        name = _get_name(node, source_bytes)
        if not name:
            return symbols

        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
        docstring = _extract_docstring(node, source_bytes)

        symbols.append(
            ParsedSymbol(
                id=sym_id,
                kind="class",
                name=name,
                qualified_name=qualified_name,
                language=self.language,
                module=module,
                file_path=file_path,
                line_start=node.start_point[0] + 1,
                line_end=node.end_point[0] + 1,
                signature=f"class {name}",
                docstring=docstring,
                content_hash=content_hash(body_text),
                body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
            )
        )

        # Extract methods from class body
        body = node.child_by_field_name("body") if hasattr(node, "child_by_field_name") else None
        if body is None:
            body = _find_child(node, "block")
        if body:
            for stmt in body.children:
                if stmt.type in ("function_definition", "async_function_definition"):
                    sym = self._handle_function(
                        stmt, source_bytes, file_path, module, class_prefix=name
                    )
                    if sym:
                        # Methods have kind="method"
                        sym = ParsedSymbol(
                            id=sym.id,
                            kind="method",
                            name=sym.name,
                            qualified_name=sym.qualified_name,
                            language=sym.language,
                            module=sym.module,
                            file_path=sym.file_path,
                            line_start=sym.line_start,
                            line_end=sym.line_end,
                            signature=sym.signature,
                            docstring=sym.docstring,
                            content_hash=sym.content_hash,
                            body_excerpt=sym.body_excerpt,
                        )
                        symbols.append(sym)
                elif stmt.type == "decorated_definition":
                    # Decorated methods
                    inner_decos = _extract_decorators(stmt, source_bytes)
                    for inner_child in stmt.children:
                        if inner_child.type in ("function_definition", "async_function_definition"):
                            sym = self._handle_function(
                                inner_child,
                                source_bytes,
                                file_path,
                                module,
                                class_prefix=name,
                                decorators=inner_decos,
                            )
                            if sym:
                                sym = ParsedSymbol(
                                    id=sym.id,
                                    kind="method",
                                    name=sym.name,
                                    qualified_name=sym.qualified_name,
                                    language=sym.language,
                                    module=sym.module,
                                    file_path=sym.file_path,
                                    line_start=sym.line_start,
                                    line_end=sym.line_end,
                                    signature=sym.signature,
                                    docstring=sym.docstring,
                                    content_hash=sym.content_hash,
                                    body_excerpt=sym.body_excerpt,
                                )
                                symbols.append(sym)

        return symbols

    def _handle_decorated(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        decorators = _extract_decorators(node, source_bytes)
        for child in node.children:
            if child.type in ("function_definition", "async_function_definition"):
                sym = self._handle_function(
                    child, source_bytes, file_path, module, decorators=decorators
                )
                if sym:
                    symbols.append(sym)
            elif child.type == "class_definition":
                syms = self._handle_class(
                    child, source_bytes, file_path, module, decorators=decorators
                )
                symbols.extend(syms)

    def _handle_assignment(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
    ) -> ParsedSymbol | None:
        """Handle top-level ``NAME = value`` where NAME is ALL_CAPS."""
        # assignment: left = right
        # The left side should be a plain identifier
        left = node.child_by_field_name("left") if hasattr(node, "child_by_field_name") else None
        if left is None:
            left = _find_child(node, "identifier")
        if left is None or left.type != "identifier":
            return None

        name = _node_text(left, source_bytes)
        if not _is_all_caps(name):
            return None

        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
        return ParsedSymbol(
            id=sym_id,
            kind="const",
            name=name,
            qualified_name=qualified_name,
            language=self.language,
            module=module,
            file_path=file_path,
            line_start=node.start_point[0] + 1,
            line_end=node.end_point[0] + 1,
            signature=body_text[:256],
            docstring=None,
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )
