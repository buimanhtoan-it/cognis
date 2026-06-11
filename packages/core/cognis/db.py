"""SQLite connection factory, migration runner, and UCKG read/write helpers.

Implements task 3 of ``.kiro/specs/cognis/tasks.md``:

- 3.1 Connection factory in WAL mode, per-thread connection cache, single-writer mutex.
- 3.2 DDL migration ``001_initial.sql`` covering MVP tables.
- 3.3 Tiny migration runner driven by ``meta.schema_version``.
- 3.4 Best-effort ``sqlite-vec`` extension load with graceful degradation.
- 3.5 Insert/query/delete primitives that satisfy the CP-3 invariants.

Design references:

- *Data Models* (DDL columns and PK shape) — keys this module's row mapping.
- *Indexer Pipeline → Writer* (single dedicated writer, per-file transaction,
  cascade behavior on deletion) — keys :func:`delete_symbol`.
- *Correctness Properties → CP-3* (insert/query roundtrip, deletion cascade) —
  keyed by the PBT in ``tests/pbt/test_db_roundtrip.py``.

The single-writer mutex is enforced at the API level rather than relying on
SQLite's busy_timeout alone: every write helper here acquires
:data:`_WRITER_LOCK` before opening a transaction. This matches the design
which calls for a "dedicated writer thread" in Phase 1 and prevents
``database is locked`` storms when multiple callers happen to share a process.
"""

from __future__ import annotations

import importlib
import json
import os
import re
import sqlite3
import threading
import time
from collections.abc import Iterable, Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Final

from cognis import __version__
from cognis.models import Edge, FileRecord, SymbolAttribute, SymbolNode

# ---------------------------------------------------------------------------
# Module-level constants
# ---------------------------------------------------------------------------

MIGRATIONS_DIR: Final[Path] = Path(__file__).parent / "migrations"
"""Directory holding ``NNN_*.sql`` migration files (task 3.2)."""

EMBEDDING_DIM: Final[int] = 384
"""Default vector dimensionality for a fresh DB (bge-small-en-v1.5, 384-d).

This is the *default* used when no embedder has declared a dimension yet (e.g.
lexical/structural-only boots). The **active** dimension is persisted in
``meta.embedding_dim`` and can differ when a higher-dim model is plugged in —
see :func:`reconcile_embedding_dim`. Code that needs the real, active dimension
should read it from the embedder instance or from ``meta``, never assume 384.
"""

DEFAULT_EMBEDDING_DIM: Final[int] = EMBEDDING_DIM
"""Explicit alias for the fresh-DB default, for call sites that want intent."""

EMBEDDING_DIM_META_KEY: Final[str] = "embedding_dim"
"""``meta`` key under which the active vector dimension is persisted."""

BUSY_TIMEOUT_MS: Final[int] = 5000
"""sqlite ``busy_timeout`` per connection (design 3.1)."""


# ---------------------------------------------------------------------------
# Per-thread connection cache + single-writer mutex
# ---------------------------------------------------------------------------

# threading.local gives us cheap per-thread caching without manual cleanup; the
# OS reaps connections when the thread exits. The cache key is the absolute DB
# path so a process opening multiple stores keeps them independent.
_THREAD_LOCAL: Final[threading.local] = threading.local()

_WRITER_LOCK: Final[threading.Lock] = threading.Lock()
"""Process-wide single-writer mutex (design Indexer Pipeline → Writer)."""


def _thread_cache() -> dict[str, sqlite3.Connection]:
    """Return the per-thread ``{abs_path: connection}`` cache, lazily created."""
    cache: dict[str, sqlite3.Connection] | None = getattr(_THREAD_LOCAL, "cache", None)
    if cache is None:
        cache = {}
        _THREAD_LOCAL.cache = cache
    return cache


# ---------------------------------------------------------------------------
# sqlite-vec extension loading (task 3.4)
# ---------------------------------------------------------------------------


def _try_load_sqlite_vec(conn: sqlite3.Connection) -> bool:
    """Best-effort load of ``sqlite-vec`` into *conn*. Returns True on success.

    Graceful degradation: if the Python wheel isn't installed *or* the platform
    can't load the native extension, we return False and let callers fall back
    to the plain ``symbol_vec`` table. Never raises.
    """
    try:
        # ``importlib.import_module`` so mypy --strict doesn't trip on a
        # conditional top-level import. The runtime override in pyproject.toml
        # already declares ``sqlite_vec.*`` as ignore_missing_imports.
        sqlite_vec = importlib.import_module("sqlite_vec")
    except ImportError:
        return False

    if not hasattr(conn, "enable_load_extension"):
        # Some Python builds (e.g. distros that strip extension loading from
        # the bundled SQLite) lack this method. Handled the same as missing pkg.
        return False

    try:
        conn.enable_load_extension(True)
        try:
            sqlite_vec.load(conn)
        finally:
            # Always re-disable: leaving extension loading on is a privilege
            # escalation surface (untrusted SQL could load arbitrary .so/.dll).
            conn.enable_load_extension(False)
    except (sqlite3.OperationalError, sqlite3.NotSupportedError, OSError):
        return False
    return True


def _read_vec_table_dim(conn: sqlite3.Connection) -> int | None:
    """Return the ``FLOAT[N]`` dimension of the current ``symbol_vec`` vec0 table.

    Returns ``None`` when the table is absent or is the plain-BLOB fallback
    (which carries no dimension constraint).
    """
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type IN ('table','view','shadow') "
        "AND name = 'symbol_vec'"
    ).fetchone()
    if row is None:
        return None
    match = re.search(r"FLOAT\[(\d+)\]", str(row[0] or ""), re.IGNORECASE)
    return int(match.group(1)) if match else None


def _ensure_vec_table(
    conn: sqlite3.Connection,
    *,
    vec_enabled: bool,
    dim: int = EMBEDDING_DIM,
) -> None:
    """Ensure ``symbol_vec`` matches the active backend (vec0 vs fallback) and *dim*.

    The DDL in migration 001 creates a plain table as a portable baseline.
    When sqlite-vec is loaded, we (re)create it as ``vec0(... FLOAT[dim] ...)``
    so KNN queries work. Migrations themselves are not parameterized — the
    choice happens here at connection time, idempotently.

    When an existing vec0 table has a *different* dimension than *dim* (a model
    with a new vector size was plugged in), the table is dropped and recreated.
    Embeddings are re-generated on the next index pass (idempotent, CP-5/CP-6).
    """
    if not vec_enabled:
        return  # Migration 001 already created the fallback table; nothing to do.

    # Ask the live schema what `symbol_vec` currently is. Leave a matching vec0
    # table untouched; replace a fallback table or a dim-mismatched vec0 table.
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type IN ('table','view') AND name = 'symbol_vec'"
    ).fetchone()
    sql_text: str = "" if row is None else str(row[0] or "")
    if "USING vec0" in sql_text:
        existing_dim = _read_vec_table_dim(conn)
        if existing_dim == dim:
            return
        # Dimension changed (model swap): fall through to recreate at *dim*.

    # The fallback table has no rows yet at first boot (Writer hasn't run);
    # if a user upgrades after some indexing, dropping the fallback and
    # recreating as vec0 forces a re-embed. That's acceptable — embedding is
    # idempotent (CP-5/CP-6) and incremental.
    conn.execute("DROP TABLE IF EXISTS symbol_vec")
    conn.execute(
        f"CREATE VIRTUAL TABLE symbol_vec USING vec0("
        f"  symbol_id TEXT PRIMARY KEY,"
        f"  embedding FLOAT[{dim}]"
        f")"
    )


# ---------------------------------------------------------------------------
# Connection factory (task 3.1)
# ---------------------------------------------------------------------------


class Database:
    """Handle bundling a DB path with its loaded-extension status.

    Acts as a small facade so callers don't sprinkle ``sqlite3`` calls around.
    Construction does *not* open a connection; connections are created lazily
    per thread the first time :meth:`connect` is called. This keeps the
    constructor cheap and fork/thread safe.
    """

    __slots__ = ("path", "vec_enabled")

    def __init__(self, path: str | os.PathLike[str], *, vec_enabled: bool | None = None) -> None:
        """Bind the database to *path*.

        Args:
            path: Filesystem path to the SQLite file. ``":memory:"`` is supported
                for tests but each thread will get its *own* in-memory DB
                because that's how SQLite memory databases scope.
            vec_enabled: Force the sqlite-vec backend on (True), off (False),
                or auto-detect (None — the default). The auto-detect probes
                a throwaway connection on first use.
        """
        self.path: str = str(path)
        if vec_enabled is None:
            self.vec_enabled = _probe_vec_support(self.path)
        else:
            self.vec_enabled = vec_enabled

    # ------------------------------------------------------------------
    # Connection lifecycle
    # ------------------------------------------------------------------

    def connect(self) -> sqlite3.Connection:
        """Return the cached connection for this thread, creating one if needed."""
        cache = _thread_cache()
        cached = cache.get(self.path)
        if cached is not None:
            return cached
        conn = self._open_new_connection()
        cache[self.path] = conn
        return conn

    def close_thread_connection(self) -> None:
        """Close *this thread's* cached connection (no-op if none).

        Test fixtures use this to fully release file handles between cases on
        Windows where SQLite holds the file until the connection is closed.
        """
        cache = _thread_cache()
        conn = cache.pop(self.path, None)
        if conn is not None:
            conn.close()

    def _open_new_connection(self) -> sqlite3.Connection:
        """Open a fresh connection wired to design 3.1's pragmas."""
        # ``check_same_thread=True`` (default) is the right call: connections
        # are cached per thread already, so a leak across threads is a bug we
        # want SQLite to surface, not silently allow.
        conn = sqlite3.connect(self.path, isolation_level=None, timeout=BUSY_TIMEOUT_MS / 1000)
        conn.row_factory = sqlite3.Row

        # WAL is set per-database (persists in the file header), but issuing it
        # on every connection is the documented idempotent pattern — and
        # cheap. ``synchronous=NORMAL`` is the WAL-recommended pairing.
        conn.execute(f"PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}")
        conn.execute("PRAGMA journal_mode = WAL")
        conn.execute("PRAGMA synchronous = NORMAL")
        conn.execute("PRAGMA foreign_keys = ON")
        conn.execute("PRAGMA temp_store = MEMORY")

        # Best-effort sqlite-vec load. Connection is usable either way; only
        # KNN queries on `symbol_vec` change shape based on the result.
        if self.vec_enabled:
            loaded = _try_load_sqlite_vec(conn)
            if not loaded:
                # If the probe lied (e.g. extension worked once, then a wheel
                # was uninstalled), demote to fallback for *this* connection
                # rather than blowing up on first KNN.
                self.vec_enabled = False

        # Bring the schema into sync with whatever migrations have shipped.
        run_migrations(conn)
        # Build the vec table at the dimension persisted in ``meta`` (set when
        # an embedder was plugged in), falling back to the fresh-DB default.
        active_dim = int(_read_meta(conn, EMBEDDING_DIM_META_KEY, str(EMBEDDING_DIM)))
        _ensure_vec_table(conn, vec_enabled=self.vec_enabled, dim=active_dim)

        return conn

    # ------------------------------------------------------------------
    # Embedding dimension reconciliation (model plug-in/out)
    # ------------------------------------------------------------------

    def reconcile_embedding_dim(self, dim: int) -> bool:
        """Align the persisted vector dimension and ``symbol_vec`` table to *dim*.

        Call this once after building the active embedder (the dimension is read
        from ``embedder.embedding_dim``). When *dim* differs from the value
        stored in ``meta`` — i.e. a model with a new vector size was plugged in
        — the ``symbol_vec`` table is recreated at the new dimension and the old
        vectors are dropped; they are regenerated on the next index pass
        (idempotent, CP-5/CP-6).

        Args:
            dim: The active embedder's ``embedding_dim``.

        Returns:
            True when the dimension changed (a re-embed is required), else False.
        """
        conn = self.connect()
        current = int(_read_meta(conn, EMBEDDING_DIM_META_KEY, str(EMBEDDING_DIM)))
        table_dim = _read_vec_table_dim(conn)
        if current == dim and (table_dim is None or table_dim == dim):
            return False

        with _WRITER_LOCK:
            _write_meta(conn, EMBEDDING_DIM_META_KEY, str(dim))
            _ensure_vec_table(conn, vec_enabled=self.vec_enabled, dim=dim)
        return True

    # ------------------------------------------------------------------
    # Transaction helper (single-writer mutex)
    # ------------------------------------------------------------------

    @contextmanager
    def write(self) -> Iterator[sqlite3.Connection]:
        """Yield a connection inside a write transaction guarded by the writer lock.

        Usage::

            with db.write() as conn:
                conn.execute("INSERT INTO ...")
                # commit happens on context exit; rollback on exception.
        """
        conn = self.connect()
        with _WRITER_LOCK:
            conn.execute("BEGIN IMMEDIATE")
            try:
                yield conn
            except BaseException:
                conn.execute("ROLLBACK")
                raise
            conn.execute("COMMIT")


def _probe_vec_support(path: str) -> bool:
    """Open a throwaway connection to learn whether sqlite-vec loads here.

    Probing once at :class:`Database` construction time keeps the hot
    :meth:`Database.connect` path branch-free in the common "extension works"
    case. The probe never raises.
    """
    try:
        probe = sqlite3.connect(":memory:")
    except sqlite3.Error:
        return False
    try:
        return _try_load_sqlite_vec(probe)
    finally:
        probe.close()
    # Note: the standalone probe answers "does the wheel + platform combo
    # work at all?". Whether the *target* DB at `path` will accept the load
    # is checked separately inside _open_new_connection.


# ---------------------------------------------------------------------------
# Migration runner (task 3.3)
# ---------------------------------------------------------------------------


def _list_migrations() -> list[Path]:
    """Return migration files sorted by their numeric prefix."""
    if not MIGRATIONS_DIR.is_dir():
        return []
    files = [p for p in MIGRATIONS_DIR.iterdir() if p.is_file() and p.suffix == ".sql"]
    # Lexicographic sort works because filenames use zero-padded numeric prefix
    # (001_, 002_, ...). If a future migration breaks that convention the test
    # suite will catch it via the schema_version regression in
    # ``tests/unit/test_db.py``.
    return sorted(files, key=lambda p: p.name)


def _migration_version(path: Path) -> int:
    """Extract the numeric prefix from a migration filename like ``001_foo.sql``."""
    prefix = path.name.split("_", 1)[0]
    try:
        return int(prefix)
    except ValueError as exc:
        raise RuntimeError(f"migration filename {path.name!r} has no numeric prefix") from exc


def _read_meta(conn: sqlite3.Connection, key: str, default: str) -> str:
    """Return ``meta[key]`` or *default* when the row (or table) is missing."""
    try:
        row = conn.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
    except sqlite3.OperationalError:
        # `meta` doesn't exist yet — that's the first-boot case for migration 001.
        return default
    if row is None:
        return default
    return str(row[0])


def _write_meta(conn: sqlite3.Connection, key: str, value: str) -> None:
    """Upsert ``meta[key] = value``."""
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )


def run_migrations(conn: sqlite3.Connection) -> int:
    """Apply pending migrations and return the resulting ``schema_version``.

    Reads ``meta.schema_version`` (default 0), runs every migration whose
    numeric prefix is greater than that value in lexicographic order, and
    updates ``meta.schema_version`` and ``meta.index_version`` to reflect the
    latest applied migration and the current cognis runtime.

    Atomicity note: :meth:`sqlite3.Connection.executescript` issues an implicit
    ``COMMIT`` of any pending transaction before running, so we cannot wrap it
    in a manual ``BEGIN/COMMIT``. The DDL itself is idempotent
    (``CREATE ... IF NOT EXISTS``), so a partial failure simply leaves a
    schema state that the next ``run_migrations`` call can safely retry.
    """
    migrations = _list_migrations()
    if not migrations:
        return 0

    # Read the current version *before* opening a transaction so we don't
    # hold a write lock while reading. SQLite is fine with concurrent reads.
    current = int(_read_meta(conn, "schema_version", "0"))

    applied = current
    for migration in migrations:
        version = _migration_version(migration)
        if version <= current:
            continue

        sql = migration.read_text(encoding="utf-8")
        with _WRITER_LOCK:
            # ``executescript`` runs DDL under autocommit mode (each statement
            # commits as it runs). No outer BEGIN is needed or allowed; the
            # DDL is idempotent so a mid-script failure is safely retried on
            # the next ``run_migrations`` call.
            conn.executescript(sql)
            # ``meta`` exists by the time we get here (migration 001 creates
            # it). The two inserts auto-commit individually under autocommit
            # mode, which is fine — losing the second one only triggers a
            # re-apply on the next boot.
            _write_meta(conn, "schema_version", str(version))
            _write_meta(conn, "index_version", __version__)
        applied = version

    return applied


# ---------------------------------------------------------------------------
# Row mapping helpers
# ---------------------------------------------------------------------------


def _symbol_to_row(sym: SymbolNode) -> tuple[Any, ...]:
    """Project a :class:`SymbolNode` to the column tuple expected by INSERT."""
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


_SYMBOL_COLUMNS: Final[tuple[str, ...]] = (
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


def _row_to_symbol(row: sqlite3.Row) -> SymbolNode:
    """Inverse of :func:`_symbol_to_row` — rehydrates a :class:`SymbolNode`."""
    raw_flags = row["untrusted_flags"]
    flags: list[str]
    if raw_flags is None or raw_flags == "":
        flags = []
    else:
        decoded = json.loads(raw_flags)
        if not isinstance(decoded, list):
            raise ValueError(f"corrupt untrusted_flags for symbol {row['id']!r}: {decoded!r}")
        flags = [str(item) for item in decoded]

    return SymbolNode(
        id=str(row["id"]),
        kind=row["kind"],
        name=str(row["name"]),
        qualified_name=str(row["qualified_name"]),
        language=str(row["language"]),
        module=str(row["module"]),
        file_path=str(row["file_path"]),
        line_start=int(row["line_start"]),
        line_end=int(row["line_end"]),
        signature=row["signature"],
        docstring=row["docstring"],
        content_hash=str(row["content_hash"]),
        body_excerpt=row["body_excerpt"],
        semantic_summary=row["semantic_summary"],
        risk_score=float(row["risk_score"]),
        ambiguous=bool(row["ambiguous"]),
        untrusted_flags=flags,
        updated_at=int(row["updated_at"]),
    )


def _row_to_edge(row: sqlite3.Row) -> Edge:
    """Rehydrate an :class:`Edge` from a SELECT row."""
    raw_meta = row["meta"]
    meta: dict[str, Any]
    if raw_meta is None or raw_meta == "":
        meta = {}
    else:
        decoded = json.loads(raw_meta)
        if not isinstance(decoded, dict):
            raise ValueError(f"corrupt edge.meta payload: {decoded!r}")
        meta = decoded
    return Edge(
        src_id=str(row["src_id"]),
        dst_id=str(row["dst_id"]),
        kind=row["kind"],
        confidence=float(row["confidence"]),
        meta=meta,
    )


# ---------------------------------------------------------------------------
# Symbol CRUD (task 3.5 — CP-3 surface)
# ---------------------------------------------------------------------------


def upsert_symbol(db: Database, symbol: SymbolNode) -> None:
    """Insert or replace *symbol*. Single-row convenience over :func:`upsert_symbols`."""
    upsert_symbols(db, (symbol,))


def upsert_symbols(db: Database, symbols: Iterable[SymbolNode]) -> None:
    """Insert or replace many symbols in one transaction (Writer-aligned).

    The ``ON CONFLICT(id) DO UPDATE`` clause matches design 11.2's "atomic
    upsert of all rows for that file" requirement: callers can pass the full
    set for a file and trust the DB end-state.
    """
    rows = [_symbol_to_row(s) for s in symbols]
    if not rows:
        return

    placeholders = ", ".join(["?"] * len(_SYMBOL_COLUMNS))
    update_clause = ", ".join(f"{col} = excluded.{col}" for col in _SYMBOL_COLUMNS if col != "id")
    sql = (
        f"INSERT INTO symbol ({', '.join(_SYMBOL_COLUMNS)}) "
        f"VALUES ({placeholders}) "
        f"ON CONFLICT(id) DO UPDATE SET {update_clause}"
    )

    with db.write() as conn:
        conn.executemany(sql, rows)


def get_symbol(db: Database, symbol_id: str) -> SymbolNode | None:
    """Return the symbol with *symbol_id* or None if absent."""
    conn = db.connect()
    row = conn.execute(
        f"SELECT {', '.join(_SYMBOL_COLUMNS)} FROM symbol WHERE id = ?", (symbol_id,)
    ).fetchone()
    if row is None:
        return None
    return _row_to_symbol(row)


def list_symbols(db: Database) -> list[SymbolNode]:
    """Return every symbol in deterministic id order. Test/diagnostics helper."""
    conn = db.connect()
    cursor = conn.execute(f"SELECT {', '.join(_SYMBOL_COLUMNS)} FROM symbol ORDER BY id")
    return [_row_to_symbol(row) for row in cursor.fetchall()]


def delete_symbol(db: Database, symbol_id: str) -> bool:
    """Delete *symbol_id* and apply the CP-3 cascade.

    Per design Property 3:

    1. Delete the symbol row itself (also cascades ``symbol_attribute`` and
       ``symbol_vec`` via FK ON DELETE CASCADE).
    2. Delete every outbound edge ``(symbol_id, *, *)``.
    3. Mark every inbound edge ``(*, symbol_id, *)`` with ``meta.dst_missing = true``.
       Inbound rows are *kept* for archaeology but never returned by structural
       traversal (callers filter on ``meta.dst_missing``).

    Returns True when the symbol existed, False otherwise. The cascade still
    runs on a hit even when there are zero edges.
    """
    with db.write() as conn:
        cur = conn.execute("DELETE FROM symbol WHERE id = ?", (symbol_id,))
        existed = cur.rowcount > 0

        # Outbound edges — the source is gone, so these are unrecoverable.
        conn.execute("DELETE FROM edge WHERE src_id = ?", (symbol_id,))

        # Inbound edges — flag with dst_missing while preserving meta.confidence
        # and any prior payload. Use json_patch so concurrent indexers that
        # set other meta keys don't get clobbered.
        conn.execute(
            """
            UPDATE edge
               SET meta = json_patch(
                       COALESCE(meta, '{}'),
                       json_object('dst_missing', json('true'))
                   )
             WHERE dst_id = ?
            """,
            (symbol_id,),
        )

    return existed


# ---------------------------------------------------------------------------
# Edge CRUD
# ---------------------------------------------------------------------------


def upsert_edge(db: Database, edge: Edge) -> None:
    """Insert or replace one edge."""
    upsert_edges(db, (edge,))


def upsert_edges(db: Database, edges: Iterable[Edge]) -> None:
    """Insert or replace many edges in one transaction."""
    rows = [
        (e.src_id, e.dst_id, e.kind, e.confidence, json.dumps(e.meta) if e.meta else None)
        for e in edges
    ]
    if not rows:
        return
    sql = (
        "INSERT INTO edge (src_id, dst_id, kind, confidence, meta) "
        "VALUES (?, ?, ?, ?, ?) "
        "ON CONFLICT(src_id, dst_id, kind) DO UPDATE SET "
        "confidence = excluded.confidence, meta = excluded.meta"
    )
    with db.write() as conn:
        conn.executemany(sql, rows)


def list_edges(db: Database) -> list[Edge]:
    """Return every edge in deterministic order. Test/diagnostics helper."""
    conn = db.connect()
    cursor = conn.execute(
        "SELECT src_id, dst_id, kind, confidence, meta FROM edge ORDER BY src_id, dst_id, kind"
    )
    return [_row_to_edge(row) for row in cursor.fetchall()]


def get_outbound_edges(db: Database, src_id: str) -> list[Edge]:
    """Edges whose ``src_id`` matches, ordered for deterministic tests."""
    conn = db.connect()
    cursor = conn.execute(
        "SELECT src_id, dst_id, kind, confidence, meta FROM edge "
        "WHERE src_id = ? ORDER BY dst_id, kind",
        (src_id,),
    )
    return [_row_to_edge(row) for row in cursor.fetchall()]


def get_inbound_edges(db: Database, dst_id: str) -> list[Edge]:
    """Edges whose ``dst_id`` matches, ordered for deterministic tests.

    These rows may include the ``meta.dst_missing`` flag if the destination
    symbol has been deleted (CP-3); callers decide whether to honor or skip.
    """
    conn = db.connect()
    cursor = conn.execute(
        "SELECT src_id, dst_id, kind, confidence, meta FROM edge "
        "WHERE dst_id = ? ORDER BY src_id, kind",
        (dst_id,),
    )
    return [_row_to_edge(row) for row in cursor.fetchall()]


# ---------------------------------------------------------------------------
# Symbol attribute CRUD
# ---------------------------------------------------------------------------


def upsert_symbol_attributes(db: Database, attrs: Iterable[SymbolAttribute]) -> None:
    """Insert or replace many symbol attributes."""
    rows = [(a.symbol_id, a.key, a.value) for a in attrs]
    if not rows:
        return
    with db.write() as conn:
        conn.executemany(
            "INSERT OR REPLACE INTO symbol_attribute (symbol_id, key, value) VALUES (?, ?, ?)",
            rows,
        )


def get_symbol_attributes(db: Database, symbol_id: str) -> list[SymbolAttribute]:
    """Return attributes for *symbol_id* sorted by (key, value)."""
    conn = db.connect()
    cursor = conn.execute(
        "SELECT symbol_id, key, value FROM symbol_attribute "
        "WHERE symbol_id = ? ORDER BY key, value",
        (symbol_id,),
    )
    return [
        SymbolAttribute(symbol_id=str(row["symbol_id"]), key=row["key"], value=str(row["value"]))
        for row in cursor.fetchall()
    ]


# ---------------------------------------------------------------------------
# File CRUD
# ---------------------------------------------------------------------------


def upsert_file(db: Database, record: FileRecord) -> None:
    """Insert or replace one file row."""
    with db.write() as conn:
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


def get_file(db: Database, path: str) -> FileRecord | None:
    """Return the file row for *path* or None."""
    conn = db.connect()
    row = conn.execute(
        "SELECT path, language, size_bytes, content_hash, parsed_at, parse_status "
        "FROM file WHERE path = ?",
        (path,),
    ).fetchone()
    if row is None:
        return None
    return FileRecord(
        path=str(row["path"]),
        language=str(row["language"]),
        size_bytes=int(row["size_bytes"]),
        content_hash=str(row["content_hash"]),
        parsed_at=int(row["parsed_at"]),
        parse_status=row["parse_status"],
    )


# ---------------------------------------------------------------------------
# Convenience for callers that just want "now" as the row timestamp
# ---------------------------------------------------------------------------


def now_epoch() -> int:
    """Return current time in epoch seconds (writer timestamp helper)."""
    return int(time.time())


__all__ = [
    "BUSY_TIMEOUT_MS",
    "DEFAULT_EMBEDDING_DIM",
    "EMBEDDING_DIM",
    "EMBEDDING_DIM_META_KEY",
    "MIGRATIONS_DIR",
    "Database",
    "delete_symbol",
    "get_file",
    "get_inbound_edges",
    "get_outbound_edges",
    "get_symbol",
    "get_symbol_attributes",
    "list_edges",
    "list_symbols",
    "now_epoch",
    "run_migrations",
    "upsert_edge",
    "upsert_edges",
    "upsert_file",
    "upsert_symbol",
    "upsert_symbol_attributes",
    "upsert_symbols",
]
