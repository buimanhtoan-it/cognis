"""End-to-end integration tests for :class:`cognis_indexer.pipeline.IndexerPipeline`.

These tests drive the real pipeline against the three mini fixture repos
shipped under ``tests/fixtures/repos/`` and assert observable invariants on
the resulting UCKG database:

- Cold index produces a non-trivial number of symbols and the FTS table
  surfaces them via lexical query.
- Re-running the index without changes is idempotent (skips files).
- A modified file's old symbols are dropped and the new ones land.
- Skipping the embedder leaves ``symbol_vec`` empty without disturbing the
  other tables.
- Secret redaction (CP-7) ensures no body_excerpt leaks the canonical AWS
  test access key value.

Run with: ``pytest -m integration -k indexer_pipeline``.
"""

from __future__ import annotations

from pathlib import Path

import pytest

# Skip the whole module when any of the tree-sitter grammars are missing —
# these tests can't run without the parsers, but the rest of the suite should
# still pass on a stripped-down install.
pytest.importorskip("tree_sitter_python")
pytest.importorskip("tree_sitter_typescript")
pytest.importorskip("tree_sitter_go")

from cognis.config import Config
from cognis.db import Database
from cognis_indexer.pipeline import IndexerPipeline

pytestmark = pytest.mark.integration


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

FIXTURES_ROOT = Path(__file__).resolve().parent.parent / "fixtures" / "repos"
TS_FIXTURE = FIXTURES_ROOT / "mini-ts-app"
PY_FIXTURE = FIXTURES_ROOT / "mini-py-svc"
GO_FIXTURE = FIXTURES_ROOT / "mini-go-svc"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_pipeline(tmp_path: Path) -> tuple[IndexerPipeline, Database]:
    """Build a pipeline with an in-tree DB under *tmp_path* and no embedder."""
    db_path = tmp_path / "uckg.db"
    db = Database(str(db_path))
    cfg = Config.default()
    pipeline = IndexerPipeline(db=db, config=cfg, embedder=None)
    return pipeline, db


# ---------------------------------------------------------------------------
# Cold index: TypeScript
# ---------------------------------------------------------------------------


def test_cold_index_mini_ts_app(tmp_path: Path) -> None:
    """Cold-index the TS fixture and verify symbols + FTS + an edge land."""
    pipeline, db = _make_pipeline(tmp_path)
    try:
        stats = pipeline.index_repo(TS_FIXTURE, full=True, skip_embeddings=True)
    finally:
        pipeline.close()

    assert stats.files_processed > 0, stats.errors
    assert stats.symbols_indexed >= 30, (
        f"expected ≥30 TS symbols, got {stats.symbols_indexed}: {stats.errors}"
    )

    conn = db.connect()

    # The validate function exists.
    row = conn.execute(
        "SELECT id FROM symbol WHERE name = 'validate' AND file_path = 'src/auth/jwt.ts'"
    ).fetchone()
    assert row is not None, "expected validate symbol from src/auth/jwt.ts"

    # The requireAuth → validate edge resolved. The heuristic resolver may
    # name the wrapper differently across versions; do a permissive check:
    # there is *some* edge that lands on the validate symbol from a symbol
    # in the auth middleware file.
    edge_row = conn.execute(
        """
        SELECT 1 FROM edge e
        JOIN symbol s_src ON e.src_id = s_src.id
        JOIN symbol s_dst ON e.dst_id = s_dst.id
        WHERE s_src.file_path = 'src/middleware/auth.ts'
          AND s_dst.name = 'validate'
        LIMIT 1
        """
    ).fetchone()
    assert edge_row is not None, "expected edge from middleware/auth.ts to validate"

    # FTS should find the validate function via the search term "validate".
    fts_count = conn.execute(
        "SELECT COUNT(*) FROM symbol_fts WHERE symbol_fts MATCH 'validate'"
    ).fetchone()[0]
    assert fts_count >= 1, f"expected ≥1 FTS hit for 'validate', got {fts_count}"


# ---------------------------------------------------------------------------
# Cold index: Python  (with secret redaction check)
# ---------------------------------------------------------------------------


def test_cold_index_mini_py_svc(tmp_path: Path) -> None:
    """Cold-index the Python fixture; verify symbols and CP-7 redaction."""
    pipeline, db = _make_pipeline(tmp_path)
    try:
        stats = pipeline.index_repo(PY_FIXTURE, full=True, skip_embeddings=True)
    finally:
        pipeline.close()

    assert stats.files_processed > 0, stats.errors
    assert stats.symbols_indexed >= 25, (
        f"expected ≥25 Python symbols, got {stats.symbols_indexed}: {stats.errors}"
    )

    conn = db.connect()

    # CP-7: no row may carry the canonical AWS test access key in plain text.
    # The literal here is intentionally NOT the canonical secret string itself
    # (the full constant is concatenated to dodge accidental scanning).
    canonical = "AKIA" + "IOSFODNN7" + "EXAMPLE"
    row = conn.execute(
        "SELECT id FROM symbol WHERE body_excerpt LIKE ? LIMIT 1",
        (f"%{canonical}%",),
    ).fetchone()
    assert row is None, (
        f"CP-7 violation: symbol {row['id'] if row else None} contains an AKIA secret"
    )

    # Sanity: the well-known fixture function is present.
    row = conn.execute(
        "SELECT 1 FROM symbol WHERE name = 'encode_jwt' AND language = 'python'"
    ).fetchone()
    assert row is not None, "expected encode_jwt symbol from src/app/security.py"


# ---------------------------------------------------------------------------
# Cold index: Go
# ---------------------------------------------------------------------------


def test_cold_index_mini_go_svc(tmp_path: Path) -> None:
    """Cold-index the Go fixture; verify ≥30 symbols and main is present."""
    pipeline, db = _make_pipeline(tmp_path)
    try:
        stats = pipeline.index_repo(GO_FIXTURE, full=True, skip_embeddings=True)
    finally:
        pipeline.close()

    assert stats.files_processed > 0, stats.errors
    assert stats.symbols_indexed >= 30, (
        f"expected ≥30 Go symbols, got {stats.symbols_indexed}: {stats.errors}"
    )

    conn = db.connect()
    row = conn.execute("SELECT id FROM symbol WHERE name = 'main' AND language = 'go'").fetchone()
    assert row is not None, "expected main symbol in mini-go-svc"


# ---------------------------------------------------------------------------
# Idempotency
# ---------------------------------------------------------------------------


def test_idempotent_reindex(tmp_path: Path) -> None:
    """Running the index twice in a row skips unchanged files (REQ-IDX-2)."""
    pipeline, db = _make_pipeline(tmp_path)
    try:
        first = pipeline.index_repo(TS_FIXTURE, full=True, skip_embeddings=True)
        second = pipeline.index_repo(TS_FIXTURE, full=False, skip_embeddings=True)
    finally:
        pipeline.close()

    assert first.files_processed > 0
    assert second.files_skipped > 0, "expected the second run to skip unchanged files"

    # Symbol count must be stable across runs.
    conn = db.connect()
    count = conn.execute("SELECT COUNT(*) FROM symbol").fetchone()[0]
    assert count == first.symbols_indexed, (
        f"symbol count drifted: indexed={first.symbols_indexed}, db={count}"
    )


# ---------------------------------------------------------------------------
# Incremental modification
# ---------------------------------------------------------------------------


def test_incremental_modification(tmp_path: Path) -> None:
    """Editing a file replaces its symbols on the next ``index_changed_files``."""
    # Lay down a tiny throwaway repo so we don't mutate the canonical fixture.
    repo = tmp_path / "repo"
    repo.mkdir()
    target = repo / "module.py"
    target.write_text(
        "def alpha():\n    return 1\n\n\ndef beta():\n    return 2\n",
        encoding="utf-8",
    )

    pipeline, db = _make_pipeline(tmp_path)
    try:
        first = pipeline.index_repo(repo, full=True, skip_embeddings=True)
        assert first.files_processed == 1
        assert first.symbols_indexed >= 2

        conn = db.connect()
        old_ids = {
            row[0]
            for row in conn.execute(
                "SELECT id FROM symbol WHERE file_path = 'module.py'"
            ).fetchall()
        }
        assert old_ids, "expected initial symbols for module.py"

        # Replace the file's contents — alpha goes away, gamma appears.
        target.write_text(
            "def beta():\n    return 22\n\n\ndef gamma():\n    return 3\n",
            encoding="utf-8",
        )

        stats = pipeline.index_changed_files([target], repo)
        assert stats.files_processed == 1
    finally:
        pipeline.close()

    conn = db.connect()
    new_rows = conn.execute("SELECT id, name FROM symbol WHERE file_path = 'module.py'").fetchall()
    new_ids = {row[0] for row in new_rows}
    new_names = {row[1] for row in new_rows}
    alpha_fts = conn.execute(
        "SELECT COUNT(*) FROM symbol_fts WHERE symbol_fts MATCH 'alpha'"
    ).fetchone()[0]
    gamma_fts = conn.execute(
        "SELECT COUNT(*) FROM symbol_fts WHERE symbol_fts MATCH 'gamma'"
    ).fetchone()[0]

    # alpha is gone; gamma is present; beta survives but with a different id
    # (its body changed).
    assert "alpha" not in new_names, "old alpha symbol should be gone"
    assert "gamma" in new_names, "new gamma symbol should be present"
    assert old_ids != new_ids, "expected at least one id to change after edit"
    assert alpha_fts == 0, "removed symbols should not leave stale FTS rows behind"
    assert gamma_fts >= 1, "new symbols should be searchable through FTS"


# ---------------------------------------------------------------------------
# Skip embeddings path
# ---------------------------------------------------------------------------


def test_skip_embeddings_path(tmp_path: Path) -> None:
    """With ``embedder=None`` the symbol_vec table stays empty (other tables fill)."""
    pipeline, db = _make_pipeline(tmp_path)
    try:
        stats = pipeline.index_repo(TS_FIXTURE, full=True, skip_embeddings=True)
    finally:
        pipeline.close()

    assert stats.symbols_indexed > 0

    conn = db.connect()
    sym_count = conn.execute("SELECT COUNT(*) FROM symbol").fetchone()[0]
    file_count = conn.execute("SELECT COUNT(*) FROM file").fetchone()[0]
    fts_count = conn.execute("SELECT COUNT(*) FROM symbol_fts").fetchone()[0]
    vec_count = conn.execute("SELECT COUNT(*) FROM symbol_vec").fetchone()[0]

    assert sym_count > 0
    assert file_count > 0
    assert fts_count > 0
    assert vec_count == 0, f"expected empty symbol_vec, got {vec_count} rows"
