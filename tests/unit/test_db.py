"""Unit tests for ``cognis.db`` (task 3.1-3.4).

Coverage:

- Migration runner advances ``meta.schema_version`` and stamps ``index_version``.
- Connection factory enables WAL, foreign_keys, and busy_timeout.
- Per-thread connection cache reuses the same handle within a thread and
  produces a distinct handle in a sibling thread.
- ``Database.write`` serializes through the single-writer mutex.
- Round-trip insert + query for SymbolNode, Edge, SymbolAttribute, FileRecord.
- ``delete_symbol`` cascade matches CP-3 (outbound deleted, inbound flagged
  with ``meta.dst_missing``).
- sqlite-vec backend: virtual table replaces fallback when extension loads;
  fallback table preserved when extension unavailable. Tests skip the
  extension-only assertions via ``pytest.importorskip``.

These are sample-based tests; the property-based round-trip is in
``tests/pbt/test_db_roundtrip.py`` (task 3.5).
"""

from __future__ import annotations

import sqlite3
import threading
from collections.abc import Iterator
from pathlib import Path

import pytest
from cognis import __version__
from cognis.db import (
    BUSY_TIMEOUT_MS,
    Database,
    delete_symbol,
    get_file,
    get_inbound_edges,
    get_outbound_edges,
    get_symbol,
    get_symbol_attributes,
    list_symbols,
    now_epoch,
    upsert_edges,
    upsert_file,
    upsert_symbol,
    upsert_symbol_attributes,
    upsert_symbols,
)
from cognis.models import Edge, FileRecord, SymbolAttribute, SymbolNode

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def db(tmp_path: Path) -> Iterator[Database]:
    """A fresh on-disk Database with vec auto-detect.

    On-disk (rather than ``:memory:``) because the per-thread cache key is the
    DB path; a memory DB would give every thread an *empty* DB and mask cache
    bugs. tmp_path is auto-cleaned by pytest.

    Teardown closes the connection on the calling thread so Python 3.14
    doesn't raise ``ResourceWarning: unclosed database`` at process exit.
    Sibling-thread connections opened by individual tests are responsible for
    their own cleanup.
    """
    database = Database(tmp_path / "uckg.db")
    try:
        yield database
    finally:
        database.close_thread_connection()


def _make_symbol(
    *,
    sid: str = "py:src/foo.py:foo@aaaa",
    name: str = "foo",
    qname: str = "src.foo.foo",
    line_start: int = 1,
    line_end: int = 5,
    flags: list[str] | None = None,
) -> SymbolNode:
    return SymbolNode(
        id=sid,
        kind="function",
        name=name,
        qualified_name=qname,
        language="python",
        module="src.foo",
        file_path="src/foo.py",
        line_start=line_start,
        line_end=line_end,
        signature="def foo() -> None",
        docstring="Does foo.",
        content_hash="abc123",
        body_excerpt="pass",
        semantic_summary=None,
        risk_score=0.25,
        ambiguous=False,
        untrusted_flags=flags or [],
        updated_at=now_epoch(),
    )


# ---------------------------------------------------------------------------
# Migration runner
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_migrations_apply_initial_schema(db: Database) -> None:
    conn = db.connect()
    # Tables from migration 001 must exist.
    tables = {
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY name"
        )
    }
    for required in ("meta", "symbol", "edge", "symbol_attribute", "file", "symbol_fts"):
        assert required in tables, f"missing table {required!r}; got {tables!r}"


@pytest.mark.unit
def test_migrations_stamp_meta_versions(db: Database) -> None:
    conn = db.connect()
    rows = dict(conn.execute("SELECT key, value FROM meta").fetchall())
    assert rows.get("schema_version") == "1"
    assert rows.get("index_version") == __version__


@pytest.mark.unit
def test_migrations_idempotent(db: Database) -> None:
    """Running migrations a second time is a no-op (schema_version unchanged)."""
    from cognis.db import run_migrations

    conn = db.connect()
    first = run_migrations(conn)
    second = run_migrations(conn)
    assert first == second == 1


# ---------------------------------------------------------------------------
# Connection pragmas (task 3.1)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_connection_uses_wal_and_foreign_keys(db: Database) -> None:
    conn = db.connect()
    journal_mode = conn.execute("PRAGMA journal_mode").fetchone()[0]
    assert str(journal_mode).lower() == "wal"

    fk = conn.execute("PRAGMA foreign_keys").fetchone()[0]
    assert int(fk) == 1

    busy = conn.execute("PRAGMA busy_timeout").fetchone()[0]
    assert int(busy) == BUSY_TIMEOUT_MS


# ---------------------------------------------------------------------------
# Per-thread cache
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_per_thread_cache_returns_same_connection(db: Database) -> None:
    a = db.connect()
    b = db.connect()
    assert a is b


@pytest.mark.unit
def test_per_thread_cache_is_thread_local(db: Database) -> None:
    main_conn = db.connect()
    sibling: dict[str, sqlite3.Connection] = {}

    def _open() -> None:
        try:
            sibling["c"] = db.connect()
        finally:
            # Close the sibling's cached connection inside the same thread so
            # Python 3.14's strict ResourceWarning cleanup stays happy.
            db.close_thread_connection()

    t = threading.Thread(target=_open)
    t.start()
    t.join()

    assert "c" in sibling
    assert sibling["c"] is not main_conn


@pytest.mark.unit
def test_single_writer_mutex_serializes_writes(db: Database) -> None:
    """Two threads attempting concurrent writes commit without 'database is locked'."""
    db.connect()  # ensure schema before threads race

    errors: list[BaseException] = []

    def _writer(prefix: str) -> None:
        try:
            for i in range(10):
                upsert_symbol(
                    db,
                    _make_symbol(
                        sid=f"py:src/{prefix}.py:fn{i}@h{i:04d}",
                        name=f"fn{i}",
                        qname=f"src.{prefix}.fn{i}",
                    ),
                )
        except BaseException as exc:
            errors.append(exc)
        finally:
            db.close_thread_connection()

    a = threading.Thread(target=_writer, args=("a",))
    b = threading.Thread(target=_writer, args=("b",))
    a.start()
    b.start()
    a.join()
    b.join()

    assert errors == [], f"writer threads raised: {errors!r}"
    assert len(list_symbols(db)) == 20


# ---------------------------------------------------------------------------
# Symbol round-trip
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_upsert_then_get_symbol_preserves_fields(db: Database) -> None:
    sym = _make_symbol(flags=["secret_redacted"])
    upsert_symbol(db, sym)

    fetched = get_symbol(db, sym.id)
    assert fetched is not None
    assert fetched == sym


@pytest.mark.unit
def test_upsert_symbol_replaces_on_conflict(db: Database) -> None:
    sym_v1 = _make_symbol()
    upsert_symbol(db, sym_v1)
    sym_v2 = sym_v1.model_copy(update={"docstring": "Now better.", "risk_score": 0.9})
    upsert_symbol(db, sym_v2)

    fetched = get_symbol(db, sym_v1.id)
    assert fetched is not None
    assert fetched.docstring == "Now better."
    assert fetched.risk_score == pytest.approx(0.9)


@pytest.mark.unit
def test_get_symbol_returns_none_for_missing(db: Database) -> None:
    assert get_symbol(db, "py:src/missing.py:nope@0000") is None


@pytest.mark.unit
def test_upsert_symbols_handles_empty_iterable(db: Database) -> None:
    upsert_symbols(db, [])  # must not raise / open a transaction
    assert list_symbols(db) == []


# ---------------------------------------------------------------------------
# Edge round-trip
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_edge_roundtrip_preserves_meta(db: Database) -> None:
    a = _make_symbol(sid="py:src/a.py:a@0001", name="a", qname="src.a.a")
    b = _make_symbol(sid="py:src/b.py:b@0002", name="b", qname="src.b.b")
    upsert_symbols(db, [a, b])

    edge = Edge(src_id=a.id, dst_id=b.id, kind="calls", confidence=0.6, meta={"note": "x"})
    upsert_edges(db, [edge])

    out = get_outbound_edges(db, a.id)
    assert out == [edge]
    inb = get_inbound_edges(db, b.id)
    assert inb == [edge]


# ---------------------------------------------------------------------------
# CP-3 — deletion cascade
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_delete_symbol_removes_outbound_edges(db: Database) -> None:
    a = _make_symbol(sid="py:src/a.py:a@aaaa", name="a", qname="src.a.a")
    b = _make_symbol(sid="py:src/b.py:b@bbbb", name="b", qname="src.b.b")
    upsert_symbols(db, [a, b])
    upsert_edges(db, [Edge(src_id=a.id, dst_id=b.id, kind="calls")])

    assert delete_symbol(db, a.id) is True
    assert get_outbound_edges(db, a.id) == []
    # Inbound to b is gone too because src was deleted.
    assert get_inbound_edges(db, b.id) == []


@pytest.mark.unit
def test_delete_symbol_flags_inbound_edges_dst_missing(db: Database) -> None:
    a = _make_symbol(sid="py:src/a.py:a@aaaa", name="a", qname="src.a.a")
    b = _make_symbol(sid="py:src/b.py:b@bbbb", name="b", qname="src.b.b")
    upsert_symbols(db, [a, b])
    upsert_edges(db, [Edge(src_id=a.id, dst_id=b.id, kind="calls")])

    assert delete_symbol(db, b.id) is True

    # Inbound to b is now an outbound from a; it must remain but be flagged.
    out = get_outbound_edges(db, a.id)
    assert len(out) == 1
    assert out[0].meta.get("dst_missing") is True


@pytest.mark.unit
def test_delete_symbol_returns_false_when_missing(db: Database) -> None:
    assert delete_symbol(db, "py:src/ghost.py:ghost@0000") is False


@pytest.mark.unit
def test_delete_symbol_cascades_attributes(db: Database) -> None:
    sym = _make_symbol()
    upsert_symbol(db, sym)
    upsert_symbol_attributes(
        db,
        [
            SymbolAttribute(symbol_id=sym.id, key="db_table", value="users"),
            SymbolAttribute(symbol_id=sym.id, key="env_var", value="DATABASE_URL"),
        ],
    )
    assert len(get_symbol_attributes(db, sym.id)) == 2

    delete_symbol(db, sym.id)
    assert get_symbol_attributes(db, sym.id) == []


# ---------------------------------------------------------------------------
# File round-trip
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_file_record_roundtrip(db: Database) -> None:
    record = FileRecord(
        path="src/foo.py",
        language="python",
        size_bytes=1234,
        content_hash="ab" * 32,
        parsed_at=now_epoch(),
        parse_status="ok",
    )
    upsert_file(db, record)
    fetched = get_file(db, record.path)
    assert fetched == record


# ---------------------------------------------------------------------------
# sqlite-vec backend (task 3.4)
# ---------------------------------------------------------------------------


@pytest.mark.unit
def test_symbol_vec_table_exists_in_some_form(db: Database) -> None:
    """Either the vec0 virtual table or the fallback table must be present."""
    conn = db.connect()
    row = conn.execute("SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'").fetchone()
    assert row is not None
    sql_text = (row[0] or "").upper()
    assert ("VEC0" in sql_text) or ("EMBEDDING BLOB" in sql_text)


@pytest.mark.unit
def test_symbol_vec_uses_vec0_when_extension_loaded(tmp_path: Path) -> None:
    """When sqlite-vec is installed and loadable, ``symbol_vec`` is a vec0 vtable."""
    pytest.importorskip("sqlite_vec")

    db = Database(tmp_path / "uckg.db")
    try:
        if not db.vec_enabled:
            pytest.skip("sqlite-vec installed but not loadable on this platform")
        conn = db.connect()
        row = conn.execute("SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'").fetchone()
        assert row is not None
        assert "USING vec0" in str(row[0])
    finally:
        db.close_thread_connection()


@pytest.mark.unit
def test_database_can_force_vec_disabled(tmp_path: Path) -> None:
    """Passing ``vec_enabled=False`` keeps the fallback table even if extension is available."""
    db = Database(tmp_path / "uckg.db", vec_enabled=False)
    try:
        assert db.vec_enabled is False
        conn = db.connect()
        row = conn.execute("SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'").fetchone()
        assert row is not None
        assert "USING vec0" not in str(row[0])
    finally:
        db.close_thread_connection()
