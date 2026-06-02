"""Unit tests for ``cognis_indexd.main`` daemon helpers.

These cover the pure/near-pure building blocks of the live-indexing daemon that
the VS Code "Set Up for AI" flow launches: path resolution, the cold-index
emptiness probe, status-file serialization, and repo-relative path rendering.

The full ``run_daemon`` loop is exercised separately in
``tests/integration/test_indexd_daemon.py``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from cognis.db import Database
from cognis_indexd.main import (
    _compose_status_payload,
    _db_is_empty,
    _relative_paths,
    _resolve_db_path,
    _resolve_status_path,
    _write_status_file,
)

pytestmark = pytest.mark.unit


# ---------------------------------------------------------------------------
# _resolve_db_path
# ---------------------------------------------------------------------------


def test_resolve_db_path_prefers_explicit_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("COGNIS_DB_PATH", str(tmp_path / "from-env.db"))
    override = tmp_path / "explicit.db"
    resolved = _resolve_db_path(tmp_path, override)
    assert resolved == override.resolve()


def test_resolve_db_path_falls_back_to_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    env_db = tmp_path / "env" / "uckg.db"
    monkeypatch.setenv("COGNIS_DB_PATH", str(env_db))
    resolved = _resolve_db_path(tmp_path, None)
    assert resolved == env_db.resolve()


def test_resolve_db_path_defaults_under_cognis(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("COGNIS_DB_PATH", raising=False)
    resolved = _resolve_db_path(tmp_path, None)
    assert resolved == (tmp_path / ".cognis" / "uckg.db").resolve()


# ---------------------------------------------------------------------------
# _resolve_status_path
# ---------------------------------------------------------------------------


def test_resolve_status_path_honours_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    custom = tmp_path / "custom-status.json"
    monkeypatch.setenv("COGNIS_INDEXD_STATUS_PATH", str(custom))
    assert _resolve_status_path(tmp_path) == custom.resolve()


def test_resolve_status_path_default(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("COGNIS_INDEXD_STATUS_PATH", raising=False)
    assert _resolve_status_path(tmp_path) == (tmp_path / ".cognis" / "indexd-status.json").resolve()


# ---------------------------------------------------------------------------
# _db_is_empty
# ---------------------------------------------------------------------------


def test_db_is_empty_true_for_fresh_db(tmp_path: Path) -> None:
    """A brand-new UCKG (fresh user) reports empty so the daemon cold-indexes."""
    db = Database(str(tmp_path / "uckg.db"))
    try:
        assert _db_is_empty(db) is True
    finally:
        db.close_thread_connection()


def test_db_is_empty_false_after_a_file_row(tmp_path: Path) -> None:
    db = Database(str(tmp_path / "uckg.db"))
    try:
        conn = db.connect()
        conn.execute(
            """
            INSERT INTO file (path, language, size_bytes, content_hash, parsed_at, parse_status)
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            ("src/app.py", "python", 10, "abc123", 0, "ok"),
        )
        conn.commit()
        assert _db_is_empty(db) is False
    finally:
        db.close_thread_connection()


# ---------------------------------------------------------------------------
# _relative_paths
# ---------------------------------------------------------------------------


def test_relative_paths_renders_repo_relative_posix(tmp_path: Path) -> None:
    repo_root = tmp_path.resolve()
    paths = [repo_root / "src" / "a.py", repo_root / "src" / "b.py"]
    assert _relative_paths(paths, repo_root) == ["src/a.py", "src/b.py"]


def test_relative_paths_falls_back_for_outside_paths(tmp_path: Path) -> None:
    repo_root = (tmp_path / "repo").resolve()
    repo_root.mkdir()
    outside = (tmp_path / "elsewhere" / "x.py").resolve()
    rendered = _relative_paths([outside], repo_root)
    # Outside-repo paths must not raise; they fall back to a posix string.
    assert rendered == [outside.as_posix()]


def test_relative_paths_honours_limit(tmp_path: Path) -> None:
    repo_root = tmp_path.resolve()
    paths = [repo_root / f"f{i}.py" for i in range(20)]
    assert len(_relative_paths(paths, repo_root, limit=5)) == 5


# ---------------------------------------------------------------------------
# _compose_status_payload
# ---------------------------------------------------------------------------


def test_compose_status_payload_shape_without_watcher() -> None:
    runtime_status = {
        "active": True,
        "phase": "cold_index",
        "message": "Building initial index for this workspace…",
        "progress_percent": 15.0,
        "inflight_files": [],
        "recent_files": [],
        "last_error": None,
    }
    payload = _compose_status_payload(watcher=None, runtime_status=runtime_status)

    assert payload["phase"] == "cold_index"
    assert payload["active"] is True
    assert payload["pending_count"] == 0
    assert payload["pending_files"] == []
    assert payload["inflight_count"] == 0
    assert isinstance(payload["updated_at"], float)
    assert payload["progress_percent"] == 15.0


def test_compose_status_payload_counts_inflight() -> None:
    runtime_status = {
        "active": True,
        "phase": "incremental",
        "message": "Indexing 2 changed files…",
        "progress_percent": 65.0,
        "inflight_files": ["src/a.py", "src/b.py"],
        "recent_files": ["src/a.py", "src/b.py"],
        "last_error": None,
    }
    payload = _compose_status_payload(watcher=None, runtime_status=runtime_status)
    assert payload["inflight_count"] == 2
    assert payload["inflight_files"] == ["src/a.py", "src/b.py"]


# ---------------------------------------------------------------------------
# _write_status_file
# ---------------------------------------------------------------------------


def test_write_status_file_is_atomic_and_valid_json(tmp_path: Path) -> None:
    status_path = tmp_path / ".cognis" / "indexd-status.json"
    payload = {"active": True, "phase": "watching", "message": "Watching for file changes."}
    _write_status_file(status_path, payload)

    assert status_path.exists()
    # No stray temp file left behind by the atomic replace.
    assert not status_path.with_name(f"{status_path.name}.tmp").exists()
    loaded = json.loads(status_path.read_text(encoding="utf-8"))
    assert loaded == payload


def test_write_status_file_retries_on_transient_permission_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A transient Windows-style sharing violation on replace is retried, not fatal.

    The status file is polled concurrently by the VS Code extension; on Windows
    ``os.replace`` can raise PermissionError if the reader momentarily holds the
    destination open. The writer must retry rather than crash the daemon.
    """
    status_path = tmp_path / ".cognis" / "indexd-status.json"
    payload = {"active": True, "phase": "watching", "message": "ok"}

    real_replace = Path.replace
    calls = {"n": 0}

    def flaky_replace(self: Path, target: object) -> object:
        # Fail the first two attempts as Windows would under a sharing violation,
        # then succeed.
        if calls["n"] < 2:
            calls["n"] += 1
            raise PermissionError("[WinError 5] Access is denied")
        return real_replace(self, target)  # type: ignore[arg-type]

    monkeypatch.setattr(Path, "replace", flaky_replace)
    _write_status_file(status_path, payload)

    assert calls["n"] == 2, "expected two transient failures before success"
    assert status_path.exists()
    loaded = json.loads(status_path.read_text(encoding="utf-8"))
    assert loaded == payload


def test_write_status_file_gives_up_without_crashing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """If replace keeps failing, the writer drops the snapshot instead of raising."""
    status_path = tmp_path / ".cognis" / "indexd-status.json"

    def always_denied(self: Path, target: object) -> object:
        raise PermissionError("[WinError 32] in use")

    monkeypatch.setattr(Path, "replace", always_denied)
    # Must not raise — a status update must never take down the daemon.
    _write_status_file(status_path, {"phase": "watching"})

    # The temp file is cleaned up so it doesn't accumulate.
    assert not status_path.with_name(f"{status_path.name}.tmp").exists()
