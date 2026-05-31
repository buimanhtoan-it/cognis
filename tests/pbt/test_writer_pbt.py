"""Property-based test for IndexWriter — CP-3 file-level consistency.

**Validates: Requirements REQ-IDX-1, REQ-IDX-2, NFR Reliability** via
correctness property **CP-3** from ``.kiro/specs/cognis/design.md``.

This test encodes property **CP-3 (task 11.6)**:

    A random sequence of file insert / modify / delete operations leaves the
    DB in a consistent state:

    1. No orphan edges — an edge ``(src_id, dst_id, kind)`` exists where
       *both* src and dst are alive (not flagged ``dst_missing`` and not absent)
       only when both symbols are actually in the DB.
    2. Every symbol in the DB has a corresponding ``file`` row.
    3. An edge with ``meta.dst_missing = true`` MUST have its ``dst_id`` absent
       from the live symbol set (it was legitimately deleted).

Orphan-free definition used here (conservative):
    For every edge in the DB that does NOT have ``meta.dst_missing = true``:
    - ``src_id`` MUST be present in the live symbol set.
    - ``dst_id`` MUST be present in the live symbol set.

Edges with ``meta.dst_missing = true`` are archaeology records (design CP-3);
they are expected to have a missing destination — that is their purpose.

The generator produces a sequence of operations against a small set of
deterministic file paths and symbol names to keep the state space manageable
while still exercising all three operation types (write, modify, delete).
"""

from __future__ import annotations

import asyncio
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

import pytest
from cognis.db import (
    Database,
    list_edges,
    list_symbols,
    now_epoch,
)
from cognis_indexer.parsers.base import ParsedSymbol
from cognis_indexer.resolver.base import ResolvedEdge
from cognis_indexer.writer import FileWritePayload, IndexWriter
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# DB context manager
# ---------------------------------------------------------------------------


@contextmanager
def _fresh_db() -> Iterator[Database]:
    with tempfile.TemporaryDirectory() as td:
        db = Database(Path(td) / "uckg.db", vec_enabled=False)
        try:
            yield db
        finally:
            db.close_thread_connection()


# ---------------------------------------------------------------------------
# Deterministic helpers — fixed small universe of names/paths
# ---------------------------------------------------------------------------

_FILE_PATHS = [f"src/module_{c}.py" for c in "abcde"]
_SYMBOL_NAMES = [f"fn_{c}" for c in "abcde"]
_LANGUAGES = ["python", "typescript", "go"]


def _make_sym(file_path: str, name: str, variant: int = 0) -> ParsedSymbol:
    """Create a ParsedSymbol for the given file / name / variant.

    The *variant* encodes different "versions" of the same symbol to simulate
    content_hash changes (modify operations).
    """
    # Use a predictable but variant-dependent hash so modify produces a new node.
    chash = f"{hash(name + str(variant)) & 0xFFFFFFFF:08x}"
    qname = f"src.{Path(file_path).stem}.{name}"
    sid = f"py:{file_path}:{qname}@{chash}"
    return ParsedSymbol(
        id=sid,
        kind="function",
        name=name,
        qualified_name=qname,
        language="python",
        module=f"src.{Path(file_path).stem}",
        file_path=file_path,
        line_start=1,
        line_end=10 + variant,
        signature=f"def {name}(v{variant}) -> None",
        docstring=f"{name} variant {variant}",
        content_hash=chash,
        body_excerpt="pass",
    )


def _make_edge(src: ParsedSymbol, dst: ParsedSymbol) -> ResolvedEdge:
    return ResolvedEdge(
        src_id=src.id,
        dst_id=dst.id,
        kind="calls",
        confidence=1.0,
        ambiguous=False,
    )


# ---------------------------------------------------------------------------
# Operation strategies
# ---------------------------------------------------------------------------

# An "operation" is a 3-tuple: (op_type, file_path, symbol_names_present)
#   op_type ∈ {"write", "delete"}
# For "write" operations the symbol_names_present list is the new symbol set.
# For "delete" operations symbol_names_present is ignored.

_FILE_PATH_ST = st.sampled_from(_FILE_PATHS)
_SYMBOL_NAME_ST = st.sampled_from(_SYMBOL_NAMES)
_SYMBOL_SUBSET_ST = st.lists(_SYMBOL_NAME_ST, min_size=0, max_size=3, unique=True)
_VARIANT_ST = st.integers(min_value=0, max_value=3)


@st.composite
def _write_op(draw: st.DrawFn) -> tuple[str, str, list[str], int]:
    """Generate a write (insert/modify) operation.

    Returns (op_type="write", file_path, symbol_names, variant).
    """
    return (
        "write",
        draw(_FILE_PATH_ST),
        draw(_SYMBOL_SUBSET_ST),
        draw(_VARIANT_ST),
    )


@st.composite
def _delete_op(draw: st.DrawFn) -> tuple[str, str, list[str], int]:
    """Generate a delete operation.

    Returns (op_type="delete", file_path, [], 0).
    """
    return ("delete", draw(_FILE_PATH_ST), [], 0)


_OP_ST = st.one_of(_write_op(), _delete_op())
_OP_SEQUENCE_ST = st.lists(_OP_ST, min_size=1, max_size=20)


# ---------------------------------------------------------------------------
# Consistency checker
# ---------------------------------------------------------------------------


def _assert_db_consistent(db: Database) -> None:
    """Assert CP-3 FK-equivalent invariants after any sequence of operations.

    1. Every edge without ``meta.dst_missing`` has both src and dst in the live
       symbol set.
    2. Every symbol has a corresponding ``file`` row.
    3. Every ``meta.dst_missing`` edge has its dst_id absent from live symbols.
    """
    live_syms = {s.id for s in list_symbols(db)}
    edges = list_edges(db)

    conn = db.connect()
    live_files = {row[0] for row in conn.execute("SELECT path FROM file").fetchall()}
    live_sym_to_file = {s.id: s.file_path for s in list_symbols(db)}

    # Invariant 1 + 3 — edge consistency
    for edge in edges:
        dst_missing = edge.meta.get("dst_missing") is True
        if dst_missing:
            # 3: dst_id must be absent from live symbols.
            assert edge.dst_id not in live_syms, (
                f"Edge {edge.src_id!r} → {edge.dst_id!r} has dst_missing=true "
                f"but dst_id is still in live symbols"
            )
        else:
            # 1: both endpoints must be in live symbols.
            assert edge.src_id in live_syms, (
                f"Orphan edge: src_id {edge.src_id!r} not in live symbols"
            )
            assert edge.dst_id in live_syms, (
                f"Orphan edge: dst_id {edge.dst_id!r} not in live symbols"
            )

    # Invariant 2 — every symbol has a file row.
    for sym_id, file_path in live_sym_to_file.items():
        assert file_path in live_files, (
            f"Symbol {sym_id!r} references file {file_path!r} but no file row exists"
        )


# ---------------------------------------------------------------------------
# The property
# ---------------------------------------------------------------------------


def _run_operation(
    writer: IndexWriter,
    op: tuple[str, str, list[str], int],
    loop: asyncio.AbstractEventLoop,
) -> None:
    op_type, file_path, sym_names, variant = op
    if op_type == "delete":
        loop.run_until_complete(writer.delete_file(file_path))
    else:
        symbols = [_make_sym(file_path, name, variant) for name in sym_names]

        # Build intra-file edges between consecutive symbol pairs.
        edges: list[ResolvedEdge] = []
        for i in range(len(symbols) - 1):
            edges.append(_make_edge(symbols[i], symbols[i + 1]))

        payload = FileWritePayload(
            file_path=file_path,
            language="python",
            file_size_bytes=100,
            content_hash=f"ch{hash(file_path + str(variant)) & 0xFFFFFF:06x}",
            parsed_at=now_epoch(),
            parse_status="ok",
            symbols=symbols,
            edges=edges,
            attributes=[],
            embeddings={},
        )
        loop.run_until_complete(writer.write_file(payload))


@pytest.mark.pbt
@settings(
    max_examples=50,
    deadline=None,
    suppress_health_check=[HealthCheck.large_base_example, HealthCheck.too_slow],
)
@given(ops=_OP_SEQUENCE_ST)
def test_random_operations_leave_db_consistent(
    ops: list[tuple[str, str, list[str], int]],
) -> None:
    """**Validates: Requirements REQ-IDX-1, REQ-IDX-2, NFR Reliability** (CP-3).

    A random sequence of file write / delete operations must leave the DB in
    a consistent state — no orphan edges, all symbols have file rows, and
    dst_missing edges correctly point to absent symbols.
    """
    loop = asyncio.new_event_loop()
    try:
        with _fresh_db() as db:
            writer = IndexWriter(db)
            try:
                for op in ops:
                    _run_operation(writer, op, loop)
            finally:
                writer.close()

            _assert_db_consistent(db)
    finally:
        loop.close()
