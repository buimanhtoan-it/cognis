"""TypeScript language parser using ``tree-sitter-typescript``.

Covered node types:
- ``function_declaration`` (exported / non-exported)
- ``variable_declaration`` / ``lexical_declaration`` containing an
  ``arrow_function`` or a ``function`` value (``const foo = () => ...``)
- ``class_declaration`` and nested ``method_definition``
  (constructor, methods, getters, setters)
- ``interface_declaration``
- Default exports (``export default function ...`` / ``export default class ...``)

Usage::

    from cognis_indexer.parsers.typescript import TypeScriptParser

    parser = TypeScriptParser()
    symbols = parser.parse(source_code, "src/auth/jwt.ts")

Requires ``tree-sitter>=0.22`` and ``tree-sitter-typescript>=0.21``.
If the packages are not installed an :class:`ImportError` with an actionable
message is raised at class instantiation time.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any

from cognis_indexer.parsers._normalize import content_hash, make_symbol_id
from cognis_indexer.parsers.base import ParsedSymbol

if TYPE_CHECKING:
    pass

_BODY_EXCERPT_LIMIT = 1500
_LANG = "ts"


def _lazy_load() -> Any:
    """Import tree-sitter lazily and return (Language, parser) for TypeScript."""
    try:
        import tree_sitter_typescript as ts_ts
        from tree_sitter import Language, Parser
    except ImportError as exc:
        raise ImportError(
            "TypeScript parser requires 'tree-sitter' and 'tree-sitter-typescript'. "
            "Install them with: pip install tree-sitter>=0.22 tree-sitter-typescript>=0.21"
        ) from exc

    # tree-sitter-typescript exposes separate TypeScript and TSX grammars
    ts_language = Language(ts_ts.language_typescript())
    parser = Parser(ts_language)
    return parser


def _node_text(node: Any, source_bytes: bytes) -> str:
    return source_bytes[node.start_byte : node.end_byte].decode("utf-8", errors="replace")


def _module_from_path(file_path: str) -> str:
    """Convert ``src/auth/jwt.ts`` → ``src/auth/jwt``."""
    parts = file_path.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    return "/".join([*parts[:-1], stem]) if len(parts) > 1 else stem


def _is_exported(node: Any, source_bytes: bytes) -> bool:
    """Return True if *node*'s parent is an ``export_statement``."""
    parent = node.parent
    if parent is None:
        return False
    if parent.type == "export_statement":
        return True
    # Handle: export default function ...
    return bool(parent.type == "export_default_declaration")


def _find_child(node: Any, *types: str) -> Any | None:
    """Return the first direct child whose type is in *types*."""
    for child in node.children:
        if child.type in types:
            return child
    return None


def _find_all_children(node: Any, *types: str) -> list[Any]:
    return [c for c in node.children if c.type in types]


def _get_identifier(node: Any, source_bytes: bytes) -> str | None:
    """Extract the name identifier from a declaration node."""
    ident = _find_child(node, "identifier", "type_identifier", "property_identifier")
    if ident:
        return _node_text(ident, source_bytes)
    return None


def _extract_signature(node: Any, source_bytes: bytes) -> str:
    """Extract a human-readable signature from a function/method node."""
    # Grab text up to the opening brace or arrow
    full_text = _node_text(node, source_bytes)
    # Find the body delimiters
    for delimiter in ["{", "=>", ";"]:
        idx = full_text.find(delimiter)
        if idx != -1:
            sig = full_text[:idx].strip()
            if delimiter == "=>":
                sig = sig + " =>"
            return sig[:256]
    return full_text[:256]


def _extract_docstring(node: Any, source_bytes: bytes) -> str | None:
    """Return the leading JSDoc comment if present."""
    # Walk backwards from node to find a comment sibling
    parent = node.parent
    if parent is None:
        return None
    siblings = parent.children
    for i, child in enumerate(siblings):
        if child == node and i > 0:
            prev = siblings[i - 1]
            if prev.type == "comment":
                text = _node_text(prev, source_bytes).strip()
                # Strip /* */ and leading *
                if text.startswith("/**") or text.startswith("/*"):
                    text = text[2:] if text.startswith("/*") else text[3:]
                    text = text.rstrip("*/").strip()
                    lines = [line.lstrip(" *").strip() for line in text.splitlines()]
                    return "\n".join(line for line in lines if line)
                if text.startswith("//"):
                    return text[2:].strip()
    return None


def _is_all_caps_ts(name: str) -> bool:
    """Return True if name is ALL_CAPS (at least 2 chars, all upper or _/digit)."""
    if len(name) < 2:
        return False
    return name == name.upper() and any(c.isalpha() for c in name)


class TypeScriptParser:
    """Parser for TypeScript and TSX files.

    Instantiating this class eagerly loads ``tree-sitter-typescript``.
    Wrap in a ``try/except ImportError`` to handle missing optional dep.
    """

    language: str = "typescript"

    def __init__(self) -> None:
        self._parser = _lazy_load()

    def parse(self, source: str, file_path: str) -> list[ParsedSymbol]:
        """Parse TypeScript *source* and return a flat list of symbols."""
        source_bytes = source.encode("utf-8")
        tree = self._parser.parse(source_bytes)
        module = _module_from_path(file_path)
        symbols: list[ParsedSymbol] = []

        self._walk(tree.root_node, source_bytes, file_path, module, symbols)
        return symbols

    # ------------------------------------------------------------------
    # Tree walking
    # ------------------------------------------------------------------

    def _walk(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
        class_name: str | None = None,
    ) -> None:
        """Recursive visitor. Only descends into class bodies for methods."""
        if node.type == "function_declaration":
            sym = self._handle_function(node, source_bytes, file_path, module)
            if sym:
                symbols.append(sym)
            return  # no need to recurse into function bodies

        if node.type in ("class_declaration", "abstract_class_declaration"):
            syms = self._handle_class(node, source_bytes, file_path, module)
            symbols.extend(syms)
            return  # class handler recurses into methods itself

        if node.type == "interface_declaration":
            sym = self._handle_interface(node, source_bytes, file_path, module)
            if sym:
                symbols.append(sym)
            return

        if node.type in ("variable_declaration", "lexical_declaration"):
            syms = self._handle_variable_declaration(node, source_bytes, file_path, module)
            symbols.extend(syms)
            return

        if node.type == "export_statement":
            self._handle_export_statement(node, source_bytes, file_path, module, symbols)
            return

        if node.type == "export_default_declaration":
            self._handle_export_default(node, source_bytes, file_path, module, symbols)
            return

        # Recurse into all other nodes (program, namespace, module bodies)
        for child in node.children:
            self._walk(child, source_bytes, file_path, module, symbols, class_name)

    def _handle_export_statement(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        """Handle ``export ...`` statements by processing the inner declaration."""
        for child in node.children:
            if child.type == "function_declaration":
                sym = self._handle_function(child, source_bytes, file_path, module, exported=True)
                if sym:
                    symbols.append(sym)
            elif child.type in ("class_declaration", "abstract_class_declaration"):
                syms = self._handle_class(child, source_bytes, file_path, module, exported=True)
                symbols.extend(syms)
            elif child.type == "interface_declaration":
                sym = self._handle_interface(child, source_bytes, file_path, module, exported=True)
                if sym:
                    symbols.append(sym)
            elif child.type in ("variable_declaration", "lexical_declaration"):
                syms = self._handle_variable_declaration(
                    child, source_bytes, file_path, module, exported=True
                )
                symbols.extend(syms)

    def _handle_export_default(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        """Handle ``export default function/class``."""
        for child in node.children:
            if child.type == "function_declaration":
                sym = self._handle_function(child, source_bytes, file_path, module, exported=True)
                if sym:
                    symbols.append(sym)
            elif child.type in ("class_declaration", "abstract_class_declaration"):
                syms = self._handle_class(child, source_bytes, file_path, module, exported=True)
                symbols.extend(syms)

    def _handle_function(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        exported: bool = False,
    ) -> ParsedSymbol | None:
        name = _get_identifier(node, source_bytes)
        if not name:
            return None
        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
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
            signature=_extract_signature(node, source_bytes),
            docstring=_extract_docstring(node, source_bytes),
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )

    def _handle_class(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        exported: bool = False,
    ) -> list[ParsedSymbol]:
        symbols: list[ParsedSymbol] = []
        name = _get_identifier(node, source_bytes)
        if not name:
            return symbols

        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
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
                docstring=_extract_docstring(node, source_bytes),
                content_hash=content_hash(body_text),
                body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
            )
        )

        # Extract methods from the class body
        class_body = _find_child(node, "class_body")
        if class_body:
            for method_node in class_body.children:
                if method_node.type == "method_definition":
                    method_sym = self._handle_method(
                        method_node, source_bytes, file_path, module, name
                    )
                    if method_sym:
                        symbols.append(method_sym)

        return symbols

    def _handle_method(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        class_name: str,
    ) -> ParsedSymbol | None:
        name = _get_identifier(node, source_bytes)
        if not name:
            return None
        body_text = _node_text(node, source_bytes)
        qual = f"{class_name}.{name}"
        qualified_name = f"{_LANG}:{file_path}:{qual}"
        sym_id = make_symbol_id(_LANG, file_path, qual, body_text)
        return ParsedSymbol(
            id=sym_id,
            kind="method",
            name=name,
            qualified_name=qualified_name,
            language=self.language,
            module=module,
            file_path=file_path,
            line_start=node.start_point[0] + 1,
            line_end=node.end_point[0] + 1,
            signature=_extract_signature(node, source_bytes),
            docstring=_extract_docstring(node, source_bytes),
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )

    def _handle_interface(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        exported: bool = False,
    ) -> ParsedSymbol | None:
        name = _get_identifier(node, source_bytes)
        if not name:
            return None
        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
        return ParsedSymbol(
            id=sym_id,
            kind="interface",
            name=name,
            qualified_name=qualified_name,
            language=self.language,
            module=module,
            file_path=file_path,
            line_start=node.start_point[0] + 1,
            line_end=node.end_point[0] + 1,
            signature=f"interface {name}",
            docstring=_extract_docstring(node, source_bytes),
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )

    def _handle_variable_declaration(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        exported: bool = False,
    ) -> list[ParsedSymbol]:
        """Handle ``const foo = () => ...``, arrow-function variables, and ALL_CAPS consts."""
        symbols: list[ParsedSymbol] = []

        # Determine if this is a `const` declaration (for ALL_CAPS detection)
        is_const_kw = any(child.type == "const" for child in node.children)

        for declarator in node.children:
            if declarator.type != "variable_declarator":
                continue
            # Get the variable name
            name_node = _find_child(declarator, "identifier")
            if not name_node:
                continue
            name = _node_text(name_node, source_bytes)

            # Check if the value is an arrow_function or function_expression
            value = None
            for child in declarator.children:
                if child.type in ("arrow_function", "function_expression", "function"):
                    value = child
                    break

            body_text = _node_text(node, source_bytes)
            qualified_name = f"{_LANG}:{file_path}:{name}"
            sym_id = make_symbol_id(_LANG, file_path, name, body_text)

            if value is not None:
                # Arrow function / function expression
                symbols.append(
                    ParsedSymbol(
                        id=sym_id,
                        kind="function",
                        name=name,
                        qualified_name=qualified_name,
                        language=self.language,
                        module=module,
                        file_path=file_path,
                        line_start=node.start_point[0] + 1,
                        line_end=node.end_point[0] + 1,
                        signature=_extract_signature(declarator, source_bytes),
                        docstring=_extract_docstring(node, source_bytes),
                        content_hash=content_hash(body_text),
                        body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
                    )
                )
            elif is_const_kw and _is_all_caps_ts(name):
                # ALL_CAPS const → kind="const"
                symbols.append(
                    ParsedSymbol(
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
                        docstring=_extract_docstring(node, source_bytes),
                        content_hash=content_hash(body_text),
                        body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
                    )
                )

        return symbols
