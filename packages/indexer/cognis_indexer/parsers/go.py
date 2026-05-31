"""Go language parser using ``tree-sitter-go``.

Covered node types:
- ``function_declaration`` — top-level functions
- ``method_declaration`` — methods with receivers → ``ReceiverType.method_name``
- ``type_declaration`` → ``type_spec`` for structs and interfaces → ``kind="class"``
  (structs) or ``kind="interface"`` (interfaces)

Exported detection: first letter of identifier is uppercase (Go convention).

Usage::

    from cognis_indexer.parsers.go import GoParser

    parser = GoParser()
    symbols = parser.parse(source_code, "internal/auth/jwt.go")

Requires ``tree-sitter>=0.22`` and ``tree-sitter-go>=0.21``.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any, Literal

from cognis_indexer.parsers._normalize import content_hash, make_symbol_id
from cognis_indexer.parsers.base import ParsedSymbol

if TYPE_CHECKING:
    pass

_BODY_EXCERPT_LIMIT = 1500
_LANG = "go"


def _lazy_load() -> Any:
    try:
        import tree_sitter_go as ts_go
        from tree_sitter import Language, Parser
    except ImportError as exc:
        raise ImportError(
            "Go parser requires 'tree-sitter' and 'tree-sitter-go'. "
            "Install them with: pip install tree-sitter>=0.22 tree-sitter-go>=0.21"
        ) from exc

    go_language = Language(ts_go.language())
    parser = Parser(go_language)
    return parser


def _node_text(node: Any, source_bytes: bytes) -> str:
    return source_bytes[node.start_byte : node.end_byte].decode("utf-8", errors="replace")


def _module_from_path(file_path: str) -> str:
    """Convert ``internal/auth/jwt.go`` → ``internal/auth/jwt``."""
    parts = file_path.replace("\\", "/").split("/")
    stem = os.path.splitext(parts[-1])[0]
    return "/".join([*parts[:-1], stem]) if len(parts) > 1 else stem


def _find_child(node: Any, *types: str) -> Any | None:
    for child in node.children:
        if child.type in types:
            return child
    return None


def _find_all_children(node: Any, *types: str) -> list[Any]:
    return [c for c in node.children if c.type in types]


def _is_exported(name: str) -> bool:
    """Go exports anything whose first letter is uppercase."""
    return bool(name) and name[0].isupper()


def _extract_docstring(node: Any, source_bytes: bytes) -> str | None:
    """Extract a leading line-comment block from the node's preceding siblings."""
    parent = node.parent
    if parent is None:
        return None
    siblings = parent.children
    doc_lines: list[str] = []
    for i, child in enumerate(siblings):
        if child == node:
            # Collect preceding comment siblings
            j = i - 1
            while j >= 0 and siblings[j].type in ("comment", "\n"):
                if siblings[j].type == "comment":
                    text = _node_text(siblings[j], source_bytes).strip()
                    if text.startswith("//"):
                        doc_lines.insert(0, text[2:].strip())
                j -= 1
            break
    return "\n".join(doc_lines) if doc_lines else None


def _extract_signature(node: Any, source_bytes: bytes) -> str:
    """Extract signature up to the opening brace."""
    full_text = _node_text(node, source_bytes)
    idx = full_text.find("{")
    if idx != -1:
        return full_text[:idx].strip()[:256]
    return full_text[:256]


def _get_receiver_type(method_node: Any, source_bytes: bytes) -> str | None:
    """Extract the receiver type name from a method declaration.

    Go methods look like: ``func (r ReceiverType) MethodName(...) ...``
    The parameter_list for the receiver contains a parameter_declaration.
    """
    # method_declaration has a 'receiver' field
    receiver = (
        method_node.child_by_field_name("receiver")
        if hasattr(method_node, "child_by_field_name")
        else None
    )
    if receiver is None:
        # Fall back: look for parameter_list as first child
        receiver = _find_child(method_node, "parameter_list")
    if receiver is None:
        return None

    # Inside the receiver: parameter_declaration -> type_identifier or pointer_type
    for child in receiver.children:
        if child.type == "parameter_declaration":
            # Could be: `r ReceiverType` or `r *ReceiverType`
            type_node = (
                child.child_by_field_name("type") if hasattr(child, "child_by_field_name") else None
            )
            if type_node is None:
                # Walk children to find type_identifier or pointer_type
                for subchild in child.children:
                    if subchild.type in ("type_identifier", "pointer_type"):
                        type_node = subchild
                        break
            if type_node is None:
                continue
            if type_node.type == "pointer_type":
                # Dereference: *ReceiverType
                inner = _find_child(type_node, "type_identifier")
                if inner:
                    return _node_text(inner, source_bytes)
            elif type_node.type == "type_identifier":
                return _node_text(type_node, source_bytes)
    return None


class GoParser:
    """Parser for Go ``.go`` files.

    Instantiating this class eagerly loads ``tree-sitter-go``.
    """

    language: str = "go"

    def __init__(self) -> None:
        self._parser = _lazy_load()

    def parse(self, source: str, file_path: str) -> list[ParsedSymbol]:
        """Parse Go *source* and return a flat list of symbols."""
        source_bytes = source.encode("utf-8")
        tree = self._parser.parse(source_bytes)
        module = _module_from_path(file_path)
        symbols: list[ParsedSymbol] = []

        for node in tree.root_node.children:
            self._visit_top_level(node, source_bytes, file_path, module, symbols)

        return symbols

    def _visit_top_level(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
        symbols: list[ParsedSymbol],
    ) -> None:
        if node.type == "function_declaration":
            sym = self._handle_function(node, source_bytes, file_path, module)
            if sym:
                symbols.append(sym)

        elif node.type == "method_declaration":
            sym = self._handle_method(node, source_bytes, file_path, module)
            if sym:
                symbols.append(sym)

        elif node.type == "type_declaration":
            syms = self._handle_type_declaration(node, source_bytes, file_path, module)
            symbols.extend(syms)

    def _handle_function(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
    ) -> ParsedSymbol | None:
        name_node = (
            node.child_by_field_name("name") if hasattr(node, "child_by_field_name") else None
        )
        if name_node is None:
            name_node = _find_child(node, "identifier")
        if name_node is None:
            return None
        name = _node_text(name_node, source_bytes)

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

    def _handle_method(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
    ) -> ParsedSymbol | None:
        name_node = (
            node.child_by_field_name("name") if hasattr(node, "child_by_field_name") else None
        )
        if name_node is None:
            name_node = _find_child(node, "field_identifier", "identifier")
        if name_node is None:
            return None
        method_name = _node_text(name_node, source_bytes)
        receiver_type = _get_receiver_type(node, source_bytes)

        qual = f"{receiver_type}.{method_name}" if receiver_type else method_name

        body_text = _node_text(node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{qual}"
        sym_id = make_symbol_id(_LANG, file_path, qual, body_text)
        return ParsedSymbol(
            id=sym_id,
            kind="method",
            name=method_name,
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

    def _handle_type_declaration(
        self,
        node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
    ) -> list[ParsedSymbol]:
        symbols: list[ParsedSymbol] = []
        # type_declaration contains one or more type_spec nodes
        for child in node.children:
            if child.type == "type_spec":
                sym = self._handle_type_spec(child, node, source_bytes, file_path, module)
                if sym:
                    symbols.append(sym)
        return symbols

    def _handle_type_spec(
        self,
        spec_node: Any,
        decl_node: Any,
        source_bytes: bytes,
        file_path: str,
        module: str,
    ) -> ParsedSymbol | None:
        name_node = (
            spec_node.child_by_field_name("name")
            if hasattr(spec_node, "child_by_field_name")
            else None
        )
        if name_node is None:
            name_node = _find_child(spec_node, "type_identifier")
        if name_node is None:
            return None
        name = _node_text(name_node, source_bytes)

        # Determine kind: struct_type or interface_type
        kind: Literal["class", "interface"] = "class"
        type_val = (
            spec_node.child_by_field_name("type")
            if hasattr(spec_node, "child_by_field_name")
            else None
        )
        if type_val is None:
            # find the type by looking at children after the name
            found_name = False
            for child in spec_node.children:
                if child == name_node:
                    found_name = True
                    continue
                if found_name and child.type not in ("=", " "):
                    type_val = child
                    break
        if type_val is not None and type_val.type == "interface_type":
            kind = "interface"

        # Use the parent type_declaration for the body text (includes the `type` keyword)
        body_text = _node_text(decl_node, source_bytes)
        qualified_name = f"{_LANG}:{file_path}:{name}"
        sym_id = make_symbol_id(_LANG, file_path, name, body_text)
        return ParsedSymbol(
            id=sym_id,
            kind=kind,
            name=name,
            qualified_name=qualified_name,
            language=self.language,
            module=module,
            file_path=file_path,
            line_start=decl_node.start_point[0] + 1,
            line_end=decl_node.end_point[0] + 1,
            signature=f"type {name}",
            docstring=_extract_docstring(decl_node, source_bytes),
            content_hash=content_hash(body_text),
            body_excerpt=body_text[:_BODY_EXCERPT_LIMIT],
        )
