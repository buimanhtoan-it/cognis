"""Integration tests for the ``cognis-indexd`` daemon loop (``run_daemon``).

The VS Code "Set Up for AI" flow, on a brand-new install, spawns

    python -m cognis_indexd.main --repo-root <repo> --db-path <db> --full-rebuild

These tests drive that exact entrypoint (``run_daemon``) against a throwaway
repo and assert the observable contract a fresh user depends on:

- A full rebuild on an empty DB cold-indexes the repo so symbols + FTS land.
- The daemon publishes a status file that walks ``cold_index → watching`` and
  ends ``stopped`` after a clean shutdown.
- A populated DB does an incremental sweep instead of a destructive rebuild.
- Live edits made while the daemon watches get indexed incrementally.

The daemon is launched as an in-process asyncio task and stopped by cancelling
that task, which unwinds ``run_daemon``'s ``finally`` block exactly like a
``SIGINT`` / ``cognis-cli down`` would. This stays portable on Windows, where
``loop.add_signal_handler`` is unavailable. No real subprocess or Python
interpreter spawn is needed.

Run with: ``pytest -m integration -k indexd_daemon``.
"""

from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path

import pytest

pytest.importorskip("tree_sitter_python")
pytest.importorskip("watchdog")

from cognis.config import Config
from cognis.db import Database
from cognis_indexd.main import run_daemon

pytestmark = [pytest.mark.integration, pytest.mark.asyncio]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_repo(repo_root: Path) -> None:
    """Lay down a tiny but real Python repo for the daemon to index."""
    (repo_root / "pkg").mkdir(parents=True, exist_ok=True)
    (repo_root / "pkg" / "alpha.py").write_text(
        "def alpha():\n    return 1\n\n\ndef helper():\n    return alpha()\n",
        encoding="utf-8",
    )
    (repo_root / "pkg" / "beta.py").write_text(
        "def beta():\n    return 2\n",
        encoding="utf-8",
    )


async def _wait_for(predicate, *, timeout: float = 15.0, interval: float = 0.1) -> bool:
    """Poll *predicate* until it returns truthy or *timeout* elapses."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        await asyncio.sleep(interval)
    return False


def _read_status(status_path: Path) -> dict | None:
    if not status_path.exists():
        return None
    try:
        return json.loads(status_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None


def _symbol_count(db_path: Path) -> int:
    db = Database(str(db_path))
    try:
        return int(db.connect().execute("SELECT COUNT(*) FROM symbol").fetchone()[0])
    finally:
        db.close_thread_connection()


class _DaemonHandle:
    """Run ``run_daemon`` as a task and stop it deterministically."""

    def __init__(self, repo_root: Path, db_path: Path, status_path: Path) -> None:
        self.repo_root = repo_root
        self.db_path = db_path
        self.status_path = status_path
        self.task: asyncio.Task[int] | None = None

    async def start(self, *, force_full_rebuild: bool, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("COGNIS_INDEXD_STATUS_PATH", str(self.status_path))
        # Force the embedder off so the test never loads a heavy local model;
        # lexical + structural indexing (the fresh-user smoke path) still runs.
        cfg = Config.default()
        self.task = asyncio.create_task(
            run_daemon(
                self.repo_root,
                cfg,
                db_path_override=self.db_path,
                force_full_rebuild=force_full_rebuild,
            )
        )

    async def wait_until_watching(self, *, timeout: float = 15.0) -> None:
        ok = await _wait_for(
            lambda: (_read_status(self.status_path) or {}).get("phase") == "watching",
            timeout=timeout,
        )
        assert ok, f"daemon never reached 'watching'; last status={_read_status(self.status_path)}"

    async def stop(self) -> int:
        assert self.task is not None
        # Cancelling the task unwinds the run_daemon finally-block: it flips the
        # status to "stopped", stops the watcher, and closes the pipeline.
        self.task.cancel()
        try:
            return await self.task
        except asyncio.CancelledError:
            return 0


async def _run_with_embedder_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Patch the daemon's embedder builder to None for speed/determinism."""
    import cognis_indexd.main as daemon_main

    monkeypatch.setattr(daemon_main, "_build_embedder", lambda _config: None)


# ---------------------------------------------------------------------------
# Cold index on a fresh repo (the "Set Up for AI" path)
# ---------------------------------------------------------------------------


async def test_full_rebuild_cold_indexes_fresh_repo(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``--full-rebuild`` on an empty DB populates symbols + FTS for a new user."""
    await _run_with_embedder_disabled(monkeypatch)
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    daemon = _DaemonHandle(repo_root, db_path, status_path)
    await daemon.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    try:
        await daemon.wait_until_watching()
    finally:
        await daemon.stop()

    # The cold index must have produced the repo's symbols.
    assert db_path.exists(), "daemon should have created the UCKG database"
    count = _symbol_count(db_path)
    assert count >= 3, f"expected ≥3 symbols from cold index, got {count}"

    db = Database(str(db_path))
    try:
        conn = db.connect()
        alpha = conn.execute("SELECT id FROM symbol WHERE name = 'alpha'").fetchone()
        assert alpha is not None, "expected the alpha symbol to be indexed"
        fts_hits = conn.execute(
            "SELECT COUNT(*) FROM symbol_fts WHERE symbol_fts MATCH 'helper'"
        ).fetchone()[0]
        assert fts_hits >= 1, "cold-indexed symbols must be searchable via FTS"
    finally:
        db.close_thread_connection()


async def test_full_rebuild_stamps_index_version(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A daemon cold rebuild must stamp ``meta.index_version`` with the runtime.

    Regression: previously only the CLI ``index --full/--clear`` path wrote
    ``index_version``; the daemon's ``--full-rebuild`` rebuilt the index but
    left the stamp untouched. After a version upgrade that left the ``version``
    health check failing forever, and the extension's auto-manage — which forces
    a full rebuild whenever the version check fails — re-triggered the rebuild
    on every activation (an endless loop). The stamp now lives in
    ``index_repo`` so the daemon and the CLI can never drift.
    """
    from cognis import __version__

    await _run_with_embedder_disabled(monkeypatch)
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    daemon = _DaemonHandle(repo_root, db_path, status_path)
    await daemon.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    try:
        await daemon.wait_until_watching()
    finally:
        await daemon.stop()

    db = Database(str(db_path))
    try:
        row = db.connect().execute("SELECT value FROM meta WHERE key = 'index_version'").fetchone()
    finally:
        db.close_thread_connection()
    assert row is not None, "daemon full rebuild must record meta.index_version"
    assert row[0] == __version__, (
        f"index_version={row[0]!r} should match runtime {__version__!r} so the "
        "health version check passes and auto-manage stops forcing rebuilds"
    )


async def test_status_file_transitions_to_watching_then_stopped(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The IDE-facing status file walks cold_index → watching → stopped."""
    await _run_with_embedder_disabled(monkeypatch)
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    daemon = _DaemonHandle(repo_root, db_path, status_path)
    await daemon.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    await daemon.wait_until_watching()

    watching = _read_status(status_path)
    assert watching is not None
    assert watching["phase"] == "watching"
    assert watching["active"] is True
    assert watching["progress_percent"] == 100.0
    assert isinstance(watching.get("pid"), int)

    await daemon.stop()

    # After a clean shutdown the daemon must mark itself stopped/inactive so the
    # extension's status bar doesn't show a phantom "indexing" forever.
    stopped = await _wait_for(
        lambda: (_read_status(status_path) or {}).get("phase") == "stopped",
        timeout=5.0,
    )
    assert stopped, f"daemon never reported 'stopped'; last status={_read_status(status_path)}"
    final = _read_status(status_path)
    assert final is not None
    assert final["active"] is False


async def test_populated_db_runs_incremental_sweep_not_rebuild(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A daemon restart on a populated DB sweeps instead of cold-indexing."""
    await _run_with_embedder_disabled(monkeypatch)
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    # First run: cold index, then stop.
    first = _DaemonHandle(repo_root, db_path, status_path)
    await first.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    await first.wait_until_watching()
    await first.stop()
    baseline = _symbol_count(db_path)
    assert baseline >= 3

    # Second run: NOT a full rebuild and DB is populated → sweep path.
    second = _DaemonHandle(repo_root, db_path, status_path)
    await second.start(force_full_rebuild=False, monkeypatch=monkeypatch)
    await second.wait_until_watching()
    await second.stop()

    # The sweep is idempotent: symbol count is stable, not doubled or wiped.
    assert _symbol_count(db_path) == baseline


async def test_live_edit_is_indexed_incrementally(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A file created while the daemon watches gets picked up and indexed."""
    await _run_with_embedder_disabled(monkeypatch)
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    daemon = _DaemonHandle(repo_root, db_path, status_path)
    await daemon.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    await daemon.wait_until_watching()
    try:
        # Add a brand-new file after the watcher is live.
        (repo_root / "pkg" / "gamma.py").write_text(
            "def gamma_handler():\n    return 42\n",
            encoding="utf-8",
        )

        indexed = await _wait_for(
            lambda: _has_symbol(db_path, "gamma_handler"),
            timeout=15.0,
        )
        assert indexed, "live-created file should be indexed incrementally"
    finally:
        await daemon.stop()


def _has_symbol(db_path: Path, name: str) -> bool:
    db = Database(str(db_path))
    try:
        row = (
            db.connect().execute("SELECT 1 FROM symbol WHERE name = ? LIMIT 1", (name,)).fetchone()
        )
        return row is not None
    finally:
        db.close_thread_connection()


async def test_cold_index_is_queryable_before_embeddings_finish(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Regression: a full rebuild populates the DB fast, before embeddings complete.

    The bug (seen only via the extension's daemon path, not manual CLI runs):
    ``index_repo`` embeds *every* symbol before writing any of them, so on a real
    repo the DB stayed empty — and health reported "0 files / fail" — for the
    entire multi-minute embed. The daemon now cold-indexes lexical/structural
    data first (skip_embeddings), making the workspace searchable in seconds,
    then backfills embeddings in the background.

    We install a deliberately SLOW embedder so the embedding phase is still in
    flight when we assert the DB is already populated and the watcher is live.
    """
    import time as _time

    import cognis_indexd.main as daemon_main

    class _SlowEmbedder:
        """Embedder (structural) whose every batch sleeps, simulating slow CPU embed."""

        # Match the DB's pinned embedding dim so vec writes stay valid.
        embedding_dim = 384

        def embed_batch(self, texts: list[str]):
            import numpy as np

            _time.sleep(2.0)  # block long enough to observe the pre-embed state
            return np.zeros((len(texts), self.embedding_dim), dtype=np.float32)

    monkeypatch.setattr(daemon_main, "_build_embedder", lambda _config: _SlowEmbedder())

    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _write_repo(repo_root)
    db_path = tmp_path / ".cognis" / "uckg.db"
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    daemon = _DaemonHandle(repo_root, db_path, status_path)
    await daemon.start(force_full_rebuild=True, monkeypatch=monkeypatch)
    try:
        # The DB must become queryable quickly — Phase A (skip-embeddings) writes
        # symbols before the slow embedding phase runs. This is the core fix:
        # without it, the symbol count would stay 0 until all embeddings finish.
        populated = await _wait_for(
            lambda: db_path.exists() and _symbol_count(db_path) >= 3,
            timeout=20.0,
        )
        assert populated, (
            "DB must be populated by the lexical phase before embeddings finish; "
            f"symbol_count={_symbol_count(db_path)}"
        )

        # And the watcher comes up so the workspace is fully serving while the
        # embedding backfill proceeds in the background.
        await daemon.wait_until_watching(timeout=60.0)
        assert _symbol_count(db_path) >= 3
    finally:
        await daemon.stop()
