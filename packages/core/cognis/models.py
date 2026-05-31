"""Typed data models for the Unified Code Knowledge Graph (UCKG).

These mirror the DDL in ``packages/core/cognis/migrations/001_initial.sql`` and
the design document's *Data Models* section. Each model is a :class:`pydantic.BaseModel`
with ``frozen=True`` so instances are hashable and safely shareable across the
indexer / retrieval / planner threads.

Public surface (exported from :mod:`cognis.models`):

- :class:`SymbolNode` — atomic indexable unit (function, class, route, ...).
- :class:`Edge` — directed, typed relationship between two symbols.
- :class:`SymbolAttribute` — side-effect / contract metadata attached to a symbol.
- :class:`FileRecord` — derived per-file row used by the watcher diff path.
- :class:`SymbolKind`, :class:`EdgeKind`, :class:`ParseStatus` — enum-like
  ``Literal`` aliases consumed by the DB layer.

The PBT test in ``tests/pbt/test_db_roundtrip.py`` (CP-3) generates random
:class:`SymbolNode` and :class:`Edge` instances against these schemas, inserts
them through :mod:`cognis.db`, and asserts the roundtrip preserves every field.
"""

from __future__ import annotations

from typing import Any, Final, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

# ---------------------------------------------------------------------------
# Enum-like literal aliases
# ---------------------------------------------------------------------------

SymbolKind = Literal[
    "function",
    "class",
    "method",
    "interface",
    "route",
    "module",
    "var",
    "const",
]
"""Allowed values for :attr:`SymbolNode.kind` (design *ID conventions*)."""

EdgeKind = Literal[
    "calls",
    "imports",
    "inherits",
    "implements",
    "reads",
    "writes",
    "routes_to",
    "tests",
]
"""Allowed values for :attr:`Edge.kind` (design *Data Models*)."""

ParseStatus = Literal["ok", "partial", "failed"]
"""Allowed values for :attr:`FileRecord.parse_status`."""


# A single ConfigDict reused by every model: forbid unknown keys (typos should
# fail loudly), enforce assignment-time validation, and freeze instances so the
# DB layer can hand them across threads without defensive copies.
_MODEL_CONFIG: Final[ConfigDict] = ConfigDict(
    extra="forbid",
    validate_assignment=True,
    frozen=True,
)


# ---------------------------------------------------------------------------
# SymbolNode
# ---------------------------------------------------------------------------


class SymbolNode(BaseModel):
    """Atomic indexable unit per design *Data Models → symbol table*.

    Field semantics mirror the SQL columns 1:1. Lists/JSON columns are
    represented as native Python lists; the DB layer is responsible for the
    JSON serialization on the way to disk.
    """

    model_config = _MODEL_CONFIG

    id: str = Field(min_length=1, max_length=512)
    """Stable ID: ``<lang>:<file_path>:<qualified_name>@<short_hash>``."""

    kind: SymbolKind
    name: str = Field(min_length=1, max_length=512)
    qualified_name: str = Field(min_length=1, max_length=1024)
    language: str = Field(min_length=1, max_length=64)
    module: str = Field(max_length=1024)
    file_path: str = Field(min_length=1, max_length=2048)
    line_start: int = Field(ge=1)
    line_end: int = Field(ge=1)

    signature: str | None = None
    docstring: str | None = None

    content_hash: str = Field(min_length=1, max_length=64)
    """sha256 of the *normalized* AST body (whitespace+comment stripped)."""

    body_excerpt: str | None = None
    semantic_summary: str | None = None
    risk_score: float = Field(default=0.0, ge=0.0, le=1.0)
    ambiguous: bool = False
    untrusted_flags: list[str] = Field(default_factory=list)
    """Taint reasons (e.g. ``["secret_redacted", "untrusted_doc"]``)."""

    updated_at: int = Field(ge=0)
    """Unix epoch seconds when this row was last upserted."""

    @field_validator("line_end")
    @classmethod
    def _line_end_after_start(cls, value: int, info: Any) -> int:
        line_start = info.data.get("line_start")
        if line_start is not None and value < line_start:
            raise ValueError(
                f"line_end ({value}) must be >= line_start ({line_start})",
            )
        return value


# ---------------------------------------------------------------------------
# Edge
# ---------------------------------------------------------------------------


class Edge(BaseModel):
    """Directed, typed relationship between two symbols.

    The composite primary key is ``(src_id, dst_id, kind)`` per design DDL.
    ``meta`` is a free-form JSON payload reserved for small annotations
    (e.g. ``{"dst_missing": true}`` after the destination symbol is deleted —
    see CP-3).
    """

    model_config = _MODEL_CONFIG

    src_id: str = Field(min_length=1, max_length=512)
    dst_id: str = Field(min_length=1, max_length=512)
    kind: EdgeKind
    confidence: float = Field(default=1.0, ge=0.0, le=1.0)
    meta: dict[str, Any] = Field(default_factory=dict)


# ---------------------------------------------------------------------------
# SymbolAttribute
# ---------------------------------------------------------------------------


class SymbolAttribute(BaseModel):
    """Enricher-extracted side-effect metadata (db_table, http_route, ...)."""

    model_config = _MODEL_CONFIG

    symbol_id: str = Field(min_length=1, max_length=512)
    key: Literal["db_table", "http_route", "env_var", "external_call"]
    value: str = Field(min_length=1, max_length=1024)


# ---------------------------------------------------------------------------
# FileRecord
# ---------------------------------------------------------------------------


class FileRecord(BaseModel):
    """Per-file cache row used by the watcher to short-circuit unchanged files."""

    model_config = _MODEL_CONFIG

    path: str = Field(min_length=1, max_length=2048)
    language: str = Field(min_length=1, max_length=64)
    size_bytes: int = Field(ge=0)
    content_hash: str = Field(min_length=1, max_length=64)
    parsed_at: int = Field(ge=0)
    parse_status: ParseStatus


__all__ = [
    "Edge",
    "EdgeKind",
    "FileRecord",
    "ParseStatus",
    "SymbolAttribute",
    "SymbolKind",
    "SymbolNode",
]
