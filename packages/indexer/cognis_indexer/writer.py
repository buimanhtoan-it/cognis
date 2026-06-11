"""IndexWriter — single-writer transaction orchestration for the indexer pipeline.

Implements task 11 of ``.kiro/specs/cognis/tasks.md``:

- 11.1  Dedicated writer logic, asyncio-friendly with one DB connection in WAL mode.
- 11.2  Per-file transaction: upsert all ``symbol``, ``edge``,
        ``symbol_attribute``, ``symbol_vec`` rows for that file atomically.
- 11.3  Deletion cascade: when a symbol disappears, ``delete_symbol``
        removes outbound edges and marks inbound edges with
        ``meta.dst_missing=true``.
- 11.4  Update ``file.parsed_at`` and ``file.parse_status`` per file.

Design references:

- *Indexer Pipeline → Writer* (design.md): "single dedicated thread …
  per-file transaction … cascade-update inverse edges on deletion".
- *Correctness Properties → CP-3*: deletion cascade invariants.

Conversion contract
-------------------
``ParsedSymbol`` (from the parser stage) is converted to a :class:`SymbolNode`
by mapping fields 1:1.  The enricher's ``untrusted_flags`` arrive already
attached to the ``ParsedSymbol`` if an enricher ran; this writer does not
re-run enrichment.

``ResolvedEdge`` (from the resolver stage) is converted to an :class:`Edge`
by forwarding ``src_id``, ``dst_id``, ``kind``, ``confidence`` and merging
``meta`` from the resolved edge (marking ``meta["ambiguous"]=true`` when
``ambiguous`` is set).

Embedding upsert
----------------
If the caller provides a non-empty ``embeddings`` dict
``{content_hash: np.ndarray}``, the writer upserts the vector for each symbol
whose ``content_hash`` has an entry.

For the ``vec0`` backend (sqlite-vec loaded), we use the native ``INSERT OR
REPLACE`` on the ``symbol_vec`` virtual table.  For the fallback table the
vector is serialised as ``numpy.ndarray.tobytes()``.

Thread safety
-------------
The :class:`Database` writer mutex (``_WRITER_LOCK``) serialises concurrent
callers through :meth:`Database.write`.  ``IndexWriter`` itself is stateless
between calls and is safe to share across coroutines/threads.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from cognis.db import (
    Database,
    delete_symbol,
    now_epoch,
)
from cognis.models import Edge, FileRecord, SymbolAttribute, SymbolNode

from cognis_indexer.parsers.base import ParsedSymbol
from cognis_indexer.resolver.base import ResolvedEdge

# ``numpy`` ships under the ``embed-local`` extra. Import it lazily so the
# writer (and the indexer pipeline importing it) stays importable without the
# extra. ``np`` is only touched when upserting embedding vectors, which only
# happens when embeddings were produced — i.e. numpy is present.
if TYPE_CHECKING:
    import numpy as np
else:
    try:
        import numpy as np
    except ImportError:  # pragma: no cover - exercised only without embed-local
        np = None

__all__ = ["FileWritePayload", "IndexWriter"]

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Public payload dataclass
# ---------------------------------------------------------------------------


@dataclass
class FileWritePayload:
    """Complete write payload for one file pass through the indexer pipeline.

    All fields are required by the writer for a complete atomic file update.
    ``embeddings`` may be empty when the embedder was skipped (``--skip-embeddings``).
    """

    file_path: str
    """Repo-relative path, forward slashes (matches ``file.path`` PK)."""

    language: str
    """Language identifier, e.g. ``"python"``, ``"typescript"``, ``"go"``."""

    file_size_bytes: int
    """Raw byte count of the source file (not content length)."""

    content_hash: str
    """``sha256(file_content_bytes)[:16]`` — used for file-level dedup."""

    parsed_at: int
    """Unix epoch when parsing finished; use :func:`cognis.db.now_epoch`."""

    parse_status: str  # "ok" | "partial" | "failed"
    """Outcome of the parse pass (mirrors ``FileRecord.parse_status``)."""

    symbols: list[ParsedSymbol] = field(default_factory=list)
    """All symbols extracted from this file in this pass."""

    edges: list[ResolvedEdge] = field(default_factory=list)
    """Call/import/etc. edges resolved for symbols in this file."""

    attributes: list[SymbolAttribute] = field(default_factory=list)
    """Enricher-extracted side-effect attributes (db_table, http_route, …)."""

    embeddings: dict[str, np.ndarray] = field(default_factory=dict)
    """``{content_hash: vector}`` — optional.  Empty → no vec upsert."""


# ---------------------------------------------------------------------------
# Conversion helpers
# ---------------------------------------------------------------------------


def _parsed_to_node(sym: ParsedSymbol) -> SymbolNode:
    """Convert a :class:`ParsedSymbol` to a :class:`SymbolNode` for DB insertion."""
    return SymbolNode(
        id=sym.id,
        kind=sym.kind,
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
        semantic_summary=None,  # filled later by summariser (Phase 2)
        risk_score=0.0,
        ambiguous=False,
        untrusted_flags=list(sym.untrusted_flags),
        updated_at=now_epoch(),
    )


def _symbol_ids_for_file(db: Database, file_path: str) -> set[str]:
    """Return the current symbol ids for *file_path* without scanning the full DB."""
    conn = db.connect()
    rows = conn.execute(
        "SELECT id FROM symbol WHERE file_path = ?",
        (file_path,),
    ).fetchall()
    return {str(row["id"]) for row in rows}


def _resolved_to_edge(re: ResolvedEdge) -> Edge:
    """Convert a :class:`ResolvedEdge` to a :class:`Edge` for DB insertion.

    Merges the ``ambiguous`` flag into ``meta`` so it is preserved in the DB.
    """
    meta: dict[str, Any] = dict(re.meta)
    if re.ambiguous:
        meta["ambiguous"] = True
    return Edge(
        src_id=re.src_id,
        dst_id=re.dst_id,
        kind=re.kind,
        confidence=re.confidence,
        meta=meta,
    )


# ---------------------------------------------------------------------------
# Embedding upsert helper
# ---------------------------------------------------------------------------


def _upsert_embeddings(
    db: Database,
    symbols: list[SymbolNode],
    embeddings: dict[str, np.ndarray],
) -> None:
    """Upsert symbol vectors — handles both vec0 and fallback table.

    Only symbols whose ``content_hash`` appears in *embeddings* are updated.
    This keeps the function a pure side-effect over the mapping; callers
    decide what to embed.
    """
    if not embeddings:
        return

    # Last write wins when the parser emitted duplicate ids for one file batch.
    by_id: dict[str, Any] = {}
    for sym in symbols:
        vec = embeddings.get(sym.content_hash)
        if vec is None:
            continue
        by_id[sym.id] = vec

    if not by_id:
        return

    payload = [(sym_id, vec.astype(np.float32).tobytes()) for sym_id, vec in by_id.items()]
    with db.write() as conn:
        if db.vec_enabled:
            # vec0 does not support INSERT OR REPLACE (sqlite-vec #259).
            # Delete+insert per row avoids UNIQUE failures from soft-deleted rows
            # and duplicate ids in a single executemany batch.
            for sym_id, vec_bytes in payload:
                conn.execute("DELETE FROM symbol_vec WHERE symbol_id = ?", (sym_id,))
                conn.execute(
                    "INSERT INTO symbol_vec(symbol_id, embedding) VALUES (?, ?)",
                    (sym_id, vec_bytes),
                )
        else:
            conn.executemany(
                "INSERT OR REPLACE INTO symbol_vec(symbol_id, embedding) VALUES (?, ?)",
                payload,
            )


# ---------------------------------------------------------------------------
# IndexWriter
# ---------------------------------------------------------------------------


class IndexWriter:
    """Orchestrates atomic per-file writes to the UCKG database.

    Usage::

        writer = IndexWriter(db)
        await writer.write_file(payload)
        await writer.delete_file("src/old.py")
        writer.close()

    The async interface is provided for compatibility with the broader async
    indexer pipeline.  The underlying DB I/O is synchronous (SQLite WAL) but
    the ``await``-able signature allows callers to ``await`` without blocking the
    event loop in environments where the writer runs in a thread executor.

    In practice the writer executes synchronously within the asyncio event loop
    — this is acceptable for the MVP because SQLite WAL writes are fast and the
    design allocates a dedicated indexer process (``cognis-indexd``) so there is
    no MCP query path blocked by writer I/O.
    """

    def __init__(self, db: Database) -> None:
        self._db = db

    # ------------------------------------------------------------------
    # Public async API
    # ------------------------------------------------------------------

    async def write_file(self, payload: FileWritePayload) -> None:
        """Write an entire file payload atomically (one transaction).

        Steps (design 11.2):

        1. Query the DB for symbols currently associated with *file_path*
           (the "old" set from the previous parse).
        2. Upsert all new/changed symbols.
        3. Delete symbols that were in the old set but not the new set
           (cascade per CP-3 / 11.3).
        4. Upsert edges from the resolver output.
        5. Upsert ``symbol_attribute`` rows from the enricher.
        6. Upsert ``symbol_vec`` rows if embeddings provided (11.2f).
        7. Upsert the ``file`` row with updated ``parsed_at`` / ``parse_status``
           (11.4).
        """
        self._write_file_sync(payload)

    async def delete_file(self, file_path: str) -> None:
        """Remove all symbols for *file_path* and apply cascade (11.3).

        Also removes the ``file`` row.  Idempotent — deleting a file that
        never existed is a no-op.
        """
        self._delete_file_sync(file_path)

    def close(self) -> None:
        """Release the writer's DB connection on the calling thread."""
        self._db.close_thread_connection()

    # ------------------------------------------------------------------
    # Sync implementations (separated for testability)
    # ------------------------------------------------------------------

    def _write_file_sync(self, payload: FileWritePayload) -> set[str]:
        """Synchronous core of :meth:`write_file`.

        Returns:
            Set of symbol ids removed from the file during this rewrite.
        """
        db = self._db

        # Step 1: discover old symbol ids for this file without materializing
        # the whole symbol table. Full-DB scans here become prohibitive on large
        # repos because this path runs once per file write.
        old_ids = _symbol_ids_for_file(db, payload.file_path)

        # Build new symbol nodes from ParsedSymbol list.
        new_nodes = [_parsed_to_node(s) for s in payload.symbols]
        new_ids = {n.id for n in new_nodes}

        # Step 2 + 3 + 4 + 5 + 7 — all in one atomic write block.
        with db.write() as conn:
            # 2. Upsert new/changed symbols.
            _upsert_symbols_conn(conn, new_nodes)

            # 3. Delete removed symbols (CP-3 cascade via delete_symbol helper).
            # We call delete_symbol per-symbol because it applies the edge
            # cascade.  To stay within the current open transaction we use
            # the raw SQL operations directly (delete_symbol opens its own
            # transaction).  Instead we inline the cascade SQL here.
            removed_ids = old_ids - new_ids
            for sym_id in removed_ids:
                conn.execute("DELETE FROM symbol WHERE id = ?", (sym_id,))
                conn.execute("DELETE FROM edge WHERE src_id = ?", (sym_id,))
                conn.execute(
                    """
                    UPDATE edge
                       SET meta = json_patch(
                               COALESCE(meta, '{}'),
                               json_object('dst_missing', json('true'))
                           )
                     WHERE dst_id = ?
                    """,
                    (sym_id,),
                )

            # 4. Upsert edges.
            edge_rows = [_resolved_to_edge(re) for re in payload.edges]
            _upsert_edges_conn(conn, edge_rows)

            # 5. Upsert symbol attributes.
            _upsert_attributes_conn(conn, payload.attributes)

            # 7. Upsert file row.
            file_record = FileRecord(
                path=payload.file_path,
                language=payload.language,
                size_bytes=payload.file_size_bytes,
                content_hash=payload.content_hash,
                parsed_at=payload.parsed_at,
                parse_status=payload.parse_status,  # type: ignore[arg-type]
            )
            _upsert_file_conn(conn, file_record)

        # Step 6: embeddings upsert (separate transaction — vector table may
        # be a separate vtable and sqlite-vec doesn't support multi-statement
        # transactions in all versions).
        _upsert_embeddings(db, new_nodes, payload.embeddings)
        return removed_ids

    def _delete_file_sync(self, file_path: str) -> None:
        """Synchronous core of :meth:`delete_file`."""
        db = self._db

        # Delete each symbol applying cascade (outbound edges deleted,
        # inbound edges flagged dst_missing).
        for sym_id in _symbol_ids_for_file(db, file_path):
            delete_symbol(db, sym_id)

        # Remove the file row.
        with db.write() as conn:
            conn.execute("DELETE FROM file WHERE path = ?", (file_path,))


# ---------------------------------------------------------------------------
# Connection-level helpers (inline within an open transaction)
# ---------------------------------------------------------------------------

_SYMBOL_COLUMNS = (
    "id",
    "kind",
    "name",
    "qualified_name",
    "language",
    "module",
    "file_path",
    "line_start",
    "line_end",
    "signature",
    "docstring",
    "content_hash",
    "body_excerpt",
    "semantic_summary",
    "risk_score",
    "ambiguous",
    "untrusted_flags",
    "updated_at",
)


def _symbol_to_row(sym: SymbolNode) -> tuple[Any, ...]:
    return (
        sym.id,
        sym.kind,
        sym.name,
        sym.qualified_name,
        sym.language,
        sym.module,
        sym.file_path,
        sym.line_start,
        sym.line_end,
        sym.signature,
        sym.docstring,
        sym.content_hash,
        sym.body_excerpt,
        sym.semantic_summary,
        sym.risk_score,
        1 if sym.ambiguous else 0,
        json.dumps(sym.untrusted_flags) if sym.untrusted_flags else None,
        sym.updated_at,
    )


def _upsert_symbols_conn(conn: Any, symbols: list[SymbolNode]) -> None:
    """Upsert symbols on an already-open connection (no new transaction)."""
    if not symbols:
        return
    placeholders = ", ".join(["?"] * len(_SYMBOL_COLUMNS))
    update_clause = ", ".join(f"{col} = excluded.{col}" for col in _SYMBOL_COLUMNS if col != "id")
    sql = (
        f"INSERT INTO symbol ({', '.join(_SYMBOL_COLUMNS)}) "
        f"VALUES ({placeholders}) "
        f"ON CONFLICT(id) DO UPDATE SET {update_clause}"
    )
    conn.executemany(sql, [_symbol_to_row(s) for s in symbols])


def _upsert_edges_conn(conn: Any, edges: list[Edge]) -> None:
    """Upsert edges on an already-open connection."""
    if not edges:
        return
    rows = [
        (e.src_id, e.dst_id, e.kind, e.confidence, json.dumps(e.meta) if e.meta else None)
        for e in edges
    ]
    conn.executemany(
        "INSERT INTO edge (src_id, dst_id, kind, confidence, meta) "
        "VALUES (?, ?, ?, ?, ?) "
        "ON CONFLICT(src_id, dst_id, kind) DO UPDATE SET "
        "confidence = excluded.confidence, meta = excluded.meta",
        rows,
    )


def _upsert_attributes_conn(conn: Any, attrs: list[SymbolAttribute]) -> None:
    """Upsert symbol attributes on an already-open connection."""
    if not attrs:
        return
    conn.executemany(
        "INSERT OR REPLACE INTO symbol_attribute (symbol_id, key, value) VALUES (?, ?, ?)",
        [(a.symbol_id, a.key, a.value) for a in attrs],
    )


def _upsert_file_conn(conn: Any, record: FileRecord) -> None:
    """Upsert a file row on an already-open connection."""
    conn.execute(
        "INSERT INTO file (path, language, size_bytes, content_hash, parsed_at, parse_status) "
        "VALUES (?, ?, ?, ?, ?, ?) "
        "ON CONFLICT(path) DO UPDATE SET "
        "language = excluded.language, "
        "size_bytes = excluded.size_bytes, "
        "content_hash = excluded.content_hash, "
        "parsed_at = excluded.parsed_at, "
        "parse_status = excluded.parse_status",
        (
            record.path,
            record.language,
            record.size_bytes,
            record.content_hash,
            record.parsed_at,
            record.parse_status,
        ),
    )
