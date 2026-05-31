"""Unit tests for ``cognis_indexer.writer`` (task 11.1-11.5).

Coverage:

- ``write_file`` creates symbols + file row (11.2, 11.4).
- ``write_file`` with embeddings stores to ``symbol_vec`` (11.2f).
- ``delete_file`` removes symbols and cascades edges (11.3).
- Modifying a file (re-write with different symbols) removes old, adds new (11.2).
- Per-file transaction atomic: exception mid-write → no partial state (11.5 crash recovery).
- Multiple concurrent ``write_file`` calls succeed via the writer lock (11.1).
- File parse_status is updated correctly (11.4).
"""

from __future__ import annotations

import asyncio
import threading
from collections.abc import Iterator
from pathlib import Path

import numpy as np
import pytest
from cognis.db import (
    Database,
    get_file,
    get_inbound_edges,
    get_outbound_edges,
    get_symbol,
    get_symbol_attributes,
    list_symbols,
    now_epoch,
)
from cognis.models import SymbolAttribute
from cognis_indexer.parsers.base import ParsedSymbol
from cognis_indexer.resolver.base import ResolvedEdge
from cognis_indexer.writer import FileWritePayload, IndexWriter


def _run(coro):  # type: ignore[no-untyped-def]
    """Run a coroutine in a fresh event loop, avoiding deprecation warnings."""
    loop = asyncio.new_event_loop()
    try:
        return loop.run_until_complete(coro)
    finally:
        loop.close()


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def db(tmp_path: Path) -> Iterator[Database]:
    database = Database(tmp_path / "uckg.db", vec_enabled=False)
    try:
        yield database
    finally:
        database.close_thread_connection()


@pytest.fixture
def writer(db: Database) -> Iterator[IndexWriter]:
    w = IndexWriter(db)
    try:
        yield w
    finally:
        w.close()


# ---------------------------------------------------------------------------
# Helper builders
# ---------------------------------------------------------------------------


def _make_parsed_symbol(
    *,
    sid: str = "py:src/foo.py:foo@aaaa",
    name: str = "foo",
    qname: str = "src.foo.foo",
    file_path: str = "src/foo.py",
    content_hash: str = "abc123",
    line_start: int = 1,
    line_end: int = 5,
) -> ParsedSymbol:
    return ParsedSymbol(
        id=sid,
        kind="function",
        name=name,
        qualified_name=qname,
        language="python",
        module="src.foo",
        file_path=file_path,
        line_start=line_start,
        line_end=line_end,
        signature=f"def {name}() -> None",
        docstring=f"Does {name}.",
        content_hash=content_hash,
        body_excerpt="pass",
    )


def _make_resolved_edge(
    src_id: str,
    dst_id: str,
    kind: str = "calls",
    confidence: float = 1.0,
    ambiguous: bool = False,
) -> ResolvedEdge:
    return ResolvedEdge(
        src_id=src_id,
        dst_id=dst_id,
        kind=kind,  # type: ignore[arg-type]
        confidence=confidence,
        ambiguous=ambiguous,
    )


def _default_payload(
    file_path: str = "src/foo.py",
    symbols: list[ParsedSymbol] | None = None,
    edges: list[ResolvedEdge] | None = None,
    attributes: list[SymbolAttribute] | None = None,
    embeddings: dict[str, np.ndarray] | None = None,
    parse_status: str = "ok",
) -> FileWritePayload:
    return FileWritePayload(
        file_path=file_path,
        language="python",
        file_size_bytes=100,
        content_hash="filehash01",
        parsed_at=now_epoch(),
        parse_status=parse_status,
        symbols=symbols or [],
        edges=edges or [],
        attributes=attributes or [],
        embeddings=embeddings or {},
    )


# ---------------------------------------------------------------------------
# 11.2 + 11.4: write_file creates symbols + file row
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_write_file_creates_symbol_and_file_row(db: Database, writer: IndexWriter) -> None:
    """write_file should persist symbols and a file row atomically."""
    sym = _make_parsed_symbol()
    payload = _default_payload(symbols=[sym])

    _run(writer.write_file(payload))

    fetched = get_symbol(db, sym.id)
    assert fetched is not None
    assert fetched.id == sym.id
    assert fetched.name == sym.name
    assert fetched.file_path == sym.file_path

    file_row = get_file(db, "src/foo.py")
    assert file_row is not None
    assert file_row.language == "python"
    assert file_row.parse_status == "ok"
    assert file_row.parsed_at == payload.parsed_at


@pytest.mark.unit
def test_write_file_updates_parse_status(db: Database, writer: IndexWriter) -> None:
    """parse_status should be updated on subsequent writes."""
    sym = _make_parsed_symbol()

    payload_ok = _default_payload(symbols=[sym], parse_status="ok")
    _run(writer.write_file(payload_ok))
    assert get_file(db, "src/foo.py").parse_status == "ok"  # type: ignore[union-attr]

    payload_partial = _default_payload(symbols=[sym], parse_status="partial")
    _run(writer.write_file(payload_partial))
    assert get_file(db, "src/foo.py").parse_status == "partial"  # type: ignore[union-attr]


@pytest.mark.unit
def test_write_file_multiple_symbols(db: Database, writer: IndexWriter) -> None:
    """Multiple symbols for one file are all upserted."""
    syms = [
        _make_parsed_symbol(
            sid=f"py:src/foo.py:fn{i}@{'a' * 8}{i:04d}", name=f"fn{i}", qname=f"src.foo.fn{i}"
        )
        for i in range(3)
    ]
    payload = _default_payload(symbols=syms)
    _run(writer.write_file(payload))

    all_db = [s for s in list_symbols(db) if s.file_path == "src/foo.py"]
    assert len(all_db) == 3


# ---------------------------------------------------------------------------
# 11.2: edges are persisted
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_write_file_persists_edges(db: Database, writer: IndexWriter) -> None:
    """Edges from the resolver payload are persisted."""
    sym_a = _make_parsed_symbol(
        sid="py:src/a.py:a@aaaa", name="a", qname="src.a.a", file_path="src/a.py"
    )
    sym_b = _make_parsed_symbol(
        sid="py:src/b.py:b@bbbb", name="b", qname="src.b.b", file_path="src/b.py"
    )

    # Write sym_b first so FK constraint is satisfied when edge is inserted.
    _run(writer.write_file(_default_payload(file_path="src/b.py", symbols=[sym_b])))

    edge = _make_resolved_edge(sym_a.id, sym_b.id, kind="calls", confidence=0.6, ambiguous=True)
    payload_a = _default_payload(file_path="src/a.py", symbols=[sym_a], edges=[edge])
    _run(writer.write_file(payload_a))

    out = get_outbound_edges(db, sym_a.id)
    assert len(out) == 1
    assert out[0].dst_id == sym_b.id
    assert out[0].confidence == pytest.approx(0.6)
    # Ambiguous flag forwarded into meta.
    assert out[0].meta.get("ambiguous") is True


# ---------------------------------------------------------------------------
# 11.2: symbol_attributes are persisted
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_write_file_persists_symbol_attributes(db: Database, writer: IndexWriter) -> None:
    """Symbol attributes from the enricher payload are persisted."""
    sym = _make_parsed_symbol()
    attr = SymbolAttribute(symbol_id=sym.id, key="http_route", value="/api/foo")
    payload = _default_payload(symbols=[sym], attributes=[attr])
    _run(writer.write_file(payload))

    attrs = get_symbol_attributes(db, sym.id)
    assert len(attrs) == 1
    assert attrs[0].key == "http_route"
    assert attrs[0].value == "/api/foo"


# ---------------------------------------------------------------------------
# 11.2f: embeddings stored to symbol_vec
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_write_file_with_embeddings_stores_to_symbol_vec(db: Database, writer: IndexWriter) -> None:
    """When embeddings provided, they are stored in the symbol_vec table."""
    sym = _make_parsed_symbol(content_hash="embed01")
    vec = np.ones(384, dtype=np.float32)
    payload = _default_payload(symbols=[sym], embeddings={"embed01": vec})
    _run(writer.write_file(payload))

    conn = db.connect()
    row = conn.execute("SELECT embedding FROM symbol_vec WHERE symbol_id = ?", (sym.id,)).fetchone()
    assert row is not None
    restored = np.frombuffer(row[0], dtype=np.float32)
    assert restored.shape == (384,)
    assert np.allclose(restored, vec)


@pytest.mark.unit
def test_write_file_embeddings_dedupes_duplicate_symbol_ids(
    db: Database, writer: IndexWriter
) -> None:
    """Duplicate symbol ids in one batch must not raise UNIQUE on symbol_vec."""
    shared_id = "py:src/foo.py:fn@abc123"
    sym_a = _make_parsed_symbol(sid=shared_id, content_hash="hash_a")
    sym_b = _make_parsed_symbol(sid=shared_id, content_hash="hash_b")
    vec_a = np.zeros(384, dtype=np.float32)
    vec_b = np.ones(384, dtype=np.float32)
    payload = _default_payload(
        symbols=[sym_a, sym_b],
        embeddings={"hash_a": vec_a, "hash_b": vec_b},
    )
    _run(writer.write_file(payload))

    conn = db.connect()
    row = conn.execute(
        "SELECT embedding FROM symbol_vec WHERE symbol_id = ?", (shared_id,)
    ).fetchone()
    assert row is not None
    restored = np.frombuffer(row[0], dtype=np.float32)
    assert np.allclose(restored, vec_b)


@pytest.mark.unit
def test_write_file_reupsert_same_symbol_vec_row(db: Database, writer: IndexWriter) -> None:
    """Re-writing the same symbol id replaces the vector without UNIQUE errors."""
    sym = _make_parsed_symbol(content_hash="embed01")
    vec1 = np.zeros(384, dtype=np.float32)
    vec2 = np.full(384, 2.0, dtype=np.float32)
    _run(writer.write_file(_default_payload(symbols=[sym], embeddings={"embed01": vec1})))
    _run(writer.write_file(_default_payload(symbols=[sym], embeddings={"embed01": vec2})))

    conn = db.connect()
    row = conn.execute("SELECT embedding FROM symbol_vec WHERE symbol_id = ?", (sym.id,)).fetchone()
    assert row is not None
    restored = np.frombuffer(row[0], dtype=np.float32)
    assert np.allclose(restored, vec2)


@pytest.mark.unit
def test_write_file_no_embeddings_skips_symbol_vec(db: Database, writer: IndexWriter) -> None:
    """When embeddings dict is empty, symbol_vec is not touched."""
    sym = _make_parsed_symbol(content_hash="noembed")
    payload = _default_payload(symbols=[sym], embeddings={})
    _run(writer.write_file(payload))

    conn = db.connect()
    row = conn.execute("SELECT embedding FROM symbol_vec WHERE symbol_id = ?", (sym.id,)).fetchone()
    assert row is None


# ---------------------------------------------------------------------------
# 11.2 + 11.3: modify file (re-write with different symbols)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_write_file_removes_old_symbols_on_modify(db: Database, writer: IndexWriter) -> None:
    """Symbols present in a previous write but absent in the new write are deleted."""
    sym_old = _make_parsed_symbol(
        sid="py:src/foo.py:old_fn@0001", name="old_fn", qname="src.foo.old_fn"
    )
    sym_new = _make_parsed_symbol(
        sid="py:src/foo.py:new_fn@0002", name="new_fn", qname="src.foo.new_fn"
    )

    # First write — old symbol present.
    _run(writer.write_file(_default_payload(symbols=[sym_old])))
    assert get_symbol(db, sym_old.id) is not None

    # Second write — old symbol gone, new symbol present.
    _run(writer.write_file(_default_payload(symbols=[sym_new])))
    assert get_symbol(db, sym_old.id) is None
    assert get_symbol(db, sym_new.id) is not None


@pytest.mark.unit
def test_write_file_cascades_inbound_edges_on_symbol_removal(
    db: Database, writer: IndexWriter
) -> None:
    """When a symbol is removed by a re-write, inbound edges are flagged dst_missing."""
    sym_caller = _make_parsed_symbol(
        sid="py:src/a.py:caller@aaaa", name="caller", qname="src.a.caller", file_path="src/a.py"
    )
    sym_callee = _make_parsed_symbol(
        sid="py:src/foo.py:callee@cccc", name="callee", qname="src.foo.callee"
    )

    # Write caller+callee and an edge caller→callee.
    _run(writer.write_file(_default_payload(file_path="src/foo.py", symbols=[sym_callee])))
    edge = _make_resolved_edge(sym_caller.id, sym_callee.id)
    _run(
        writer.write_file(
            _default_payload(file_path="src/a.py", symbols=[sym_caller], edges=[edge])
        )
    )

    # Re-write foo.py with callee removed.
    _run(writer.write_file(_default_payload(file_path="src/foo.py", symbols=[])))

    # Callee is gone; caller survives.
    assert get_symbol(db, sym_callee.id) is None
    assert get_symbol(db, sym_caller.id) is not None

    # Outbound edge from caller should remain but be flagged dst_missing=true.
    out = get_outbound_edges(db, sym_caller.id)
    assert len(out) == 1
    assert out[0].meta.get("dst_missing") is True


# ---------------------------------------------------------------------------
# 11.3: delete_file
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_delete_file_removes_symbols_and_file_row(db: Database, writer: IndexWriter) -> None:
    """delete_file removes all symbols and the file row."""
    sym = _make_parsed_symbol()
    _run(writer.write_file(_default_payload(symbols=[sym])))
    assert get_symbol(db, sym.id) is not None
    assert get_file(db, "src/foo.py") is not None

    _run(writer.delete_file("src/foo.py"))

    assert get_symbol(db, sym.id) is None
    assert get_file(db, "src/foo.py") is None


@pytest.mark.unit
def test_delete_file_cascades_edges(db: Database, writer: IndexWriter) -> None:
    """Deleting a file cascades edge deletions/flags."""
    sym_a = _make_parsed_symbol(
        sid="py:src/a.py:a@aaaa", name="a", qname="src.a.a", file_path="src/a.py"
    )
    sym_b = _make_parsed_symbol(
        sid="py:src/b.py:b@bbbb", name="b", qname="src.b.b", file_path="src/b.py"
    )

    _run(writer.write_file(_default_payload(file_path="src/b.py", symbols=[sym_b])))
    edge = _make_resolved_edge(sym_a.id, sym_b.id)
    _run(writer.write_file(_default_payload(file_path="src/a.py", symbols=[sym_a], edges=[edge])))

    # Delete the file containing sym_a (the caller).
    _run(writer.delete_file("src/a.py"))

    # sym_a gone; outbound edge from a gone.
    assert get_symbol(db, sym_a.id) is None
    assert get_outbound_edges(db, sym_a.id) == []
    # Inbound to sym_b is also gone (src was deleted).
    assert get_inbound_edges(db, sym_b.id) == []


@pytest.mark.unit
def test_delete_file_idempotent_on_nonexistent_file(db: Database, writer: IndexWriter) -> None:
    """Deleting a file that was never indexed does not raise."""
    _run(writer.delete_file("src/phantom.py"))
    # No exception means success.


# ---------------------------------------------------------------------------
# 11.5: crash recovery — exception mid-write leaves no partial state
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_crash_mid_write_leaves_no_partial_state(db: Database) -> None:
    """An exception raised mid-write rolls back the entire transaction (WAL atomicity).

    This simulates the crash recovery scenario from task 11.5:
    - Open a write transaction via db.write().
    - Write some data.
    - Raise an exception before commit.
    - Verify the DB has no partial writes.
    """
    # Ensure schema is initialized.
    db.connect()

    # Pre-condition: no symbols.
    assert list_symbols(db) == []

    # Simulate a crash mid-transaction.
    with pytest.raises(RuntimeError, match="simulated crash"):
        with db.write() as conn:
            conn.execute(
                "INSERT INTO symbol (id, kind, name, qualified_name, language, module, "
                "file_path, line_start, line_end, content_hash, risk_score, ambiguous, "
                "updated_at) VALUES "
                "('py:src/crash.py:fn@1234', 'function', 'fn', 'src.crash.fn', 'python', "
                "'src.crash', 'src/crash.py', 1, 5, 'abc', 0.0, 0, 1000000)"
            )
            # Simulate crash / unhandled exception before commit.
            raise RuntimeError("simulated crash")

    # Post-condition: rollback means DB is still clean.
    assert list_symbols(db) == []
    file_row = get_file(db, "src/crash.py")
    assert file_row is None


# ---------------------------------------------------------------------------
# 11.1: concurrent writes succeed (serialized via writer lock)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_concurrent_write_file_calls_succeed(tmp_path: Path) -> None:
    """Multiple threads calling write_file concurrently all succeed without DB locked errors."""
    db = Database(tmp_path / "concurrent.db", vec_enabled=False)
    db.connect()  # initialize schema on main thread

    errors: list[BaseException] = []

    def _write_n(prefix: str, n: int) -> None:
        local_writer = IndexWriter(db)
        try:
            for i in range(n):
                sym = _make_parsed_symbol(
                    sid=f"py:src/{prefix}{i}.py:fn@{'0' * 8}{i:04d}",
                    name=f"fn{i}",
                    qname=f"src.{prefix}{i}.fn{i}",
                    file_path=f"src/{prefix}{i}.py",
                )
                _run(
                    local_writer.write_file(
                        _default_payload(file_path=f"src/{prefix}{i}.py", symbols=[sym])
                    )
                )
        except BaseException as exc:
            errors.append(exc)
        finally:
            db.close_thread_connection()

    threads = [
        threading.Thread(target=_write_n, args=("t0_", 5)),
        threading.Thread(target=_write_n, args=("t1_", 5)),
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    db.close_thread_connection()

    assert errors == [], f"concurrent writers raised: {errors!r}"
    db2 = Database(tmp_path / "concurrent.db", vec_enabled=False)
    try:
        all_syms = list_symbols(db2)
        assert len(all_syms) == 10
    finally:
        db2.close_thread_connection()
