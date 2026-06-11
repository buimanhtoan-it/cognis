"""Unit tests for ``cognis_mcpd.main`` startup warm-up helpers.

The MCP server must construct its ``Database`` (and trigger the optional
``sqlite-vec``/``numpy`` import) on the *main* thread before serving. If that
heavy, import-locking work first happens on a FastMCP anyio worker thread
during the first tool call, it can deadlock the stdio serve loop (observed on
Python 3.14 / Windows). These tests pin the warm-up contract.
"""

from __future__ import annotations

import logging
from pathlib import Path

import pytest
from cognis_mcpd.main import _warm_db_on_startup

pytestmark = pytest.mark.unit


def test_warm_db_opens_database_on_calling_thread(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """_warm_db_on_startup constructs a Database for COGNIS_DB_PATH eagerly."""
    db_path = tmp_path / ".cognis" / "uckg.db"
    db_path.parent.mkdir(parents=True)
    monkeypatch.setenv("COGNIS_DB_PATH", str(db_path))

    constructed: list[str] = []

    import cognis_mcpd.main as mcpd_main

    real_database = mcpd_main.__dict__.get("Database")  # not imported at module scope

    # Patch cognis.db.Database to record construction without heavy probing.
    import cognis.db as db_module

    class _RecordingDB:
        def __init__(self, path: str, **kwargs: object) -> None:
            constructed.append(path)

    monkeypatch.setattr(db_module, "Database", _RecordingDB)

    _warm_db_on_startup(logging.getLogger("test"))

    assert constructed == [str(db_path)], (
        f"warm-up must open the DB at COGNIS_DB_PATH, got {constructed}"
    )
    assert real_database is None  # sanity: not bound at module import time


def test_warm_db_is_best_effort_and_never_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A DB construction failure must not propagate out of warm-up."""
    import cognis.db as db_module

    def _boom(*args: object, **kwargs: object) -> object:
        raise RuntimeError("simulated DB open failure")

    monkeypatch.setattr(db_module, "Database", _boom)

    # Must swallow the error: warm-up failure should never stop the server.
    _warm_db_on_startup(logging.getLogger("test"))


def test_semantic_warm_runs_on_calling_main_thread(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The semantic warm-up must load the layer ON the calling (main) thread.

    Loading torch/sentence-transformers for the first time on a non-main thread
    hangs inside the MCP server process (the tool call then times out). The
    warm-up must therefore run synchronously on the caller's thread, NOT spawn a
    background worker — so the heavy first load happens on the main thread before
    mcp.run() takes it over.
    """
    import threading

    import cognis_mcpd.main as mcpd_main

    monkeypatch.setenv("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "1")

    calling_thread = threading.current_thread()
    loaded_on: list[threading.Thread] = []

    import cognis_mcpd.embedder_pool as pool

    def _fake_layer() -> object:
        loaded_on.append(threading.current_thread())
        return object()

    monkeypatch.setattr(pool, "get_shared_semantic_layer", _fake_layer)

    mcpd_main._warm_semantic_layer_on_startup(logging.getLogger("test"))

    assert loaded_on, "warm-up must actually load the semantic layer"
    assert loaded_on[0] is calling_thread, (
        "semantic layer must load on the main/calling thread, not a background "
        "thread — off-main-thread torch init hangs the server"
    )


def test_semantic_warm_respects_disable_flag(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Setting COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0 skips the load entirely."""
    import cognis_mcpd.embedder_pool as pool
    import cognis_mcpd.main as mcpd_main

    monkeypatch.setenv("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "0")
    called = {"n": 0}

    def _fake_layer() -> object:
        called["n"] += 1
        return object()

    monkeypatch.setattr(pool, "get_shared_semantic_layer", _fake_layer)
    mcpd_main._warm_semantic_layer_on_startup(logging.getLogger("test"))

    assert called["n"] == 0, "disable flag must skip the semantic warm-up"


def test_semantic_warm_is_best_effort_and_never_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A semantic-layer load failure must not propagate out of warm-up."""
    import cognis_mcpd.embedder_pool as pool
    import cognis_mcpd.main as mcpd_main

    monkeypatch.setenv("COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP", "1")

    def _boom() -> object:
        raise RuntimeError("simulated embedder load failure")

    monkeypatch.setattr(pool, "get_shared_semantic_layer", _boom)
    # Must not raise — warm-up failure should never stop the server.
    mcpd_main._warm_semantic_layer_on_startup(logging.getLogger("test"))


# ---------------------------------------------------------------------------
# Transport selection (stdio default; opt-in http for the panel-managed server)
# ---------------------------------------------------------------------------


class _FakeMcp:
    """Records the transport/kwargs a serve call would use, without binding."""

    def __init__(self) -> None:
        self.calls: list[dict[str, object]] = []

    def run(self, **kwargs: object) -> None:
        self.calls.append(kwargs)


def test_parse_args_defaults_to_stdio() -> None:
    from cognis_mcpd.main import _parse_args

    args = _parse_args([])
    assert args.transport == "stdio"


def test_parse_args_reads_http_host_port() -> None:
    from cognis_mcpd.main import _parse_args

    args = _parse_args(["--transport", "http", "--host", "127.0.0.1", "--port", "8123"])
    assert (args.transport, args.host, args.port) == ("http", "127.0.0.1", 8123)


def test_serve_stdio_runs_stdio_transport() -> None:
    from cognis_mcpd.main import _parse_args, _serve

    mcp = _FakeMcp()
    _serve(mcp, _parse_args([]), logging.getLogger("test"))
    assert mcp.calls == [{"transport": "stdio"}]


def test_serve_http_binds_requested_host_and_port() -> None:
    from cognis_mcpd.main import _parse_args, _serve

    mcp = _FakeMcp()
    _serve(
        mcp,
        _parse_args(["--transport", "http", "--host", "127.0.0.1", "--port", "8123"]),
        logging.getLogger("test"),
    )
    assert mcp.calls == [{"transport": "http", "host": "127.0.0.1", "port": 8123}]


def test_serve_http_requires_a_port() -> None:
    from cognis_mcpd.main import _parse_args, _serve

    mcp = _FakeMcp()
    with pytest.raises(SystemExit):
        _serve(mcp, _parse_args(["--transport", "http"]), logging.getLogger("test"))
    assert mcp.calls == []


def test_serve_http_refuses_non_loopback_without_optin(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from cognis_mcpd.main import _parse_args, _serve

    monkeypatch.delenv("COGNIS_MCP_ALLOW_REMOTE", raising=False)
    mcp = _FakeMcp()
    with pytest.raises(SystemExit):
        _serve(
            mcp,
            _parse_args(["--transport", "http", "--host", "0.0.0.0", "--port", "8123"]),
            logging.getLogger("test"),
        )
    assert mcp.calls == []
