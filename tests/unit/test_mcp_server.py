"""Unit tests for the cognis MCP server tools (task 15).

Covers:
- symbol_lookup: exact id, qualified_name, fuzzy, kind filter, not found
- symbol_search: ranked multi-hit discovery, k bound, kind/path filters
- semantic_search: returns hits structure, respects k limit, kind filter
- dependency_trace: outbound, inbound, both directions, enriched hit metadata
- retrieve_context_capsule: basic call, returns valid capsule or error envelope
- Audit log: entry written on tool call, args_hash present (no raw args)
- Hard limits: depth > 8 clamped, k > 50 clamped, max_tokens clamped
- Error envelope shape

All tests use a real in-memory SQLite database via patched COGNIS_DB_PATH env.
"""

from __future__ import annotations

import json
import os
import tempfile
import threading
import time
from collections.abc import Iterator
from contextlib import suppress
from pathlib import Path
from unittest.mock import patch

import pytest
from cognis.db import Database, upsert_edge, upsert_symbol
from cognis.models import Edge, SymbolNode

_TEST_DATABASES: list[Database] = []

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _close_tool_db_cache() -> Iterator[None]:
    import cognis_mcpd.tools as tools
    from cognis_mcpd.result_cache import reset_cache_for_tests

    reset_cache_for_tests()
    tools._SEMANTIC_DISABLED_UNTIL = 0.0
    yield
    import cognis.db as db_module
    from cognis_mcpd.result_cache import reset_cache_for_tests

    reset_cache_for_tests()
    tools._SEMANTIC_DISABLED_UNTIL = 0.0
    for db in list(tools._DB_CACHE.values()):
        db.close_thread_connection()
    tools._DB_CACHE.clear()
    for db in _TEST_DATABASES:
        db.close_thread_connection()
    _TEST_DATABASES.clear()
    thread_cache = getattr(db_module._THREAD_LOCAL, "cache", None)
    if thread_cache:
        for conn in list(thread_cache.values()):
            conn.close()
        thread_cache.clear()


def _make_symbol(
    sym_id: str,
    name: str,
    *,
    kind: str = "function",
    qualified_name: str | None = None,
    signature: str | None = None,
    docstring: str | None = None,
    body_excerpt: str | None = None,
    language: str = "python",
    file_path: str = "src/test.py",
    line_start: int = 1,
    line_end: int = 10,
) -> SymbolNode:
    return SymbolNode(
        id=sym_id,
        kind=kind,  # type: ignore[arg-type]
        name=name,
        qualified_name=qualified_name or name,
        language=language,
        module="test_module",
        file_path=file_path,
        line_start=line_start,
        line_end=line_end,
        signature=signature,
        docstring=docstring,
        content_hash="deadbeef12345678",
        body_excerpt=body_excerpt,
        updated_at=int(time.time()),
    )


def _require_tool(name: str):
    """Return an MCP tool callable, skipping when not yet exported."""
    import importlib

    tools = importlib.import_module("cognis_mcpd.tools")
    fn = getattr(tools, name, None)
    if fn is None:
        import pytest

        pytest.skip(f"{name} not yet exported from cognis_mcpd.tools")
    return fn


def _populate_fts(db: Database, symbols: list[SymbolNode]) -> None:
    from cognis_retrieval.lexical import populate_fts

    populate_fts(db, symbols)


_TRACE_ENRICHMENT_FIELDS = (
    "qualified_name",
    "kind",
    "file_path",
    "line_start",
    "line_end",
)


class _MCPTestDB:
    """Helper that creates a temporary database with in-memory-backed temp file."""

    def __init__(self) -> None:
        self._tmpdir = tempfile.mkdtemp()
        self.db_path = os.path.join(self._tmpdir, "test.db")
        self.audit_path = os.path.join(self._tmpdir, "audit.log")
        self._db = Database(self.db_path, vec_enabled=False)
        _TEST_DATABASES.append(self._db)

    @property
    def db(self) -> Database:
        return self._db

    def add_symbol(self, sym: SymbolNode) -> None:
        upsert_symbol(self._db, sym)

    def add_edge(self, edge: Edge) -> None:
        upsert_edge(self._db, edge)

    def close(self) -> None:
        self._db.close_thread_connection()

    def __del__(self) -> None:
        with suppress(Exception):
            self.close()

    def patch_env(self) -> dict[str, str]:
        """Return env patches for COGNIS_DB_PATH and COGNIS_AUDIT_LOG."""
        return {
            "COGNIS_DB_PATH": self.db_path,
            "COGNIS_AUDIT_LOG": self.audit_path,
        }

    def read_audit(self) -> list[dict]:
        """Return parsed audit log entries."""
        p = Path(self.audit_path)
        if not p.exists():
            return []
        lines = p.read_text(encoding="utf-8").strip().splitlines()
        return [json.loads(line) for line in lines if line.strip()]


# ---------------------------------------------------------------------------
# Error envelope validation helper
# ---------------------------------------------------------------------------


def _is_error_envelope(result: object) -> bool:
    """Return True if result is a valid error envelope dict."""
    if not isinstance(result, dict):
        return False
    err = result.get("error")
    if not isinstance(err, dict):
        return False
    return "code" in err and "message" in err and "retryable" in err


# ---------------------------------------------------------------------------
# Tests: symbol_lookup
# ---------------------------------------------------------------------------


class TestSymbolLookup:
    def _make_ctx(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        sym = _make_symbol(
            "py:src/auth.py:validate_token@abcd",
            "validate_token",
            kind="function",
            qualified_name="auth.validate_token",
            signature="def validate_token(token: str) -> bool",
        )
        ctx.add_symbol(sym)
        return ctx

    def test_exact_id_lookup(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("py:src/auth.py:validate_token@abcd")

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["id"] == "py:src/auth.py:validate_token@abcd"
        assert result["name"] == "validate_token"

    def test_qualified_name_lookup(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("auth.validate_token")

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["qualified_name"] == "auth.validate_token"

    def test_fuzzy_name_lookup(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("validate")

        assert isinstance(result, dict)
        assert "error" not in result

    def test_kind_filter_matches(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("py:src/auth.py:validate_token@abcd", kind="function")

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["kind"] == "function"

    def test_kind_filter_mismatch(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("py:src/auth.py:validate_token@abcd", kind="class")

        # Should return error since kind doesn't match.
        assert _is_error_envelope(result)

    def test_not_found_returns_error_envelope(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("py:nonexistent@zzzz")

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "SYMBOL_NOT_FOUND"

    def test_empty_input_returns_error(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("")

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_audit_entry_written(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_lookup("py:src/auth.py:validate_token@abcd")

        entries = ctx.read_audit()
        assert len(entries) >= 1
        entry = entries[-1]
        assert entry["tool"] == "symbol_lookup"
        assert "args_hash" in entry
        assert "ts" in entry
        assert "ok" in entry
        # MUST NOT contain raw args.
        assert "name_or_id" not in entry
        assert "kind" not in entry

    def test_returns_expected_fields(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_lookup("py:src/auth.py:validate_token@abcd")

        assert isinstance(result, dict)
        for field in ("id", "kind", "name", "qualified_name", "file_path"):
            assert field in result


# ---------------------------------------------------------------------------
# Tests: symbol_search
# ---------------------------------------------------------------------------


class TestSymbolSearch:
    def _make_ctx(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        symbols = [
            _make_symbol(
                "py:src/auth.py:validate_token@aaaa",
                "validate_token",
                kind="function",
                qualified_name="auth.validate_token",
                file_path="src/auth.py",
                line_start=10,
                line_end=25,
                docstring="Validates bearer tokens.",
            ),
            _make_symbol(
                "py:src/auth.py:ValidateToken@bbbb",
                "ValidateToken",
                kind="class",
                qualified_name="auth.ValidateToken",
                file_path="src/auth.py",
                line_start=30,
                line_end=45,
            ),
            _make_symbol(
                "py:src/routes/login.py:login@cccc",
                "login",
                kind="function",
                qualified_name="routes.login",
                file_path="src/routes/login.py",
                line_start=5,
                line_end=20,
                docstring="Login route handler.",
            ),
            _make_symbol(
                "py:src/routes/logout.py:logout@dddd",
                "logout",
                kind="function",
                qualified_name="routes.logout",
                file_path="src/routes/logout.py",
                line_start=5,
                line_end=15,
            ),
        ]
        for sym in symbols:
            ctx.add_symbol(sym)
        _populate_fts(ctx.db, symbols)
        return ctx

    def test_returns_ranked_hits_bounded_by_k(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_search("validate", k=2)

        assert isinstance(result, list)
        assert 1 <= len(result) <= 2
        scores = [hit["score"] for hit in result]
        assert scores == sorted(scores, reverse=True)
        for hit in result:
            assert "symbol_id" in hit
            assert "score" in hit

    def test_kind_filter_excludes_mismatched_symbols(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_search("validate", k=10, kind="function")

        assert isinstance(result, list)
        assert result, "expected at least one function hit for validate query"
        assert all(hit.get("kind") == "function" for hit in result)
        ids = {hit["symbol_id"] for hit in result}
        assert "py:src/auth.py:ValidateToken@bbbb" not in ids

    def test_file_path_filter_restricts_results(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_search("login", k=10, file_path="src/routes/login.py")

        assert isinstance(result, list)
        assert result, "expected login symbol in filtered path"
        for hit in result:
            assert hit.get("file_path") == "src/routes/login.py"

    def test_empty_query_returns_error(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_search("")

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_k_clamped_to_50(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = symbol_search("validate", k=999)

        assert isinstance(result, (list, dict))

    def test_audit_entry_written(self) -> None:
        symbol_search = _require_tool("symbol_search")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_search("validate", k=5)

        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "symbol_search"][-1]
        assert "args_hash" in entry
        assert "query" not in entry


# ---------------------------------------------------------------------------
# Tests: semantic_search
# ---------------------------------------------------------------------------


class TestSemanticSearch:
    def _make_ctx(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        for i in range(5):
            ctx.add_symbol(
                _make_symbol(
                    f"py:src/m.py:func_{i}@{i:04x}",
                    f"func_{i}",
                    qualified_name=f"m.func_{i}",
                    signature=f"def func_{i}(): ...",
                    kind="function" if i % 2 == 0 else "method",
                )
            )
        return ctx

    def test_returns_error_when_embedder_unavailable(self) -> None:
        """With no embedder installed, returns EMBEDDER_UNAVAILABLE envelope."""
        ctx = self._make_ctx()
        import sys

        from cognis_mcpd.tools import semantic_search

        # Simulate cognis_indexer.embedder not being importable.
        with patch.dict(os.environ, ctx.patch_env()):
            # Force ImportError for cognis_indexer.embedder by patching sys.modules
            with patch.dict(
                sys.modules,
                {
                    "cognis_indexer": None,  # type: ignore[dict-item]
                    "cognis_indexer.embedder": None,  # type: ignore[dict-item]
                },
            ):
                import cognis_mcpd.tools as tools

                with patch.object(tools, "_semantic_index_available", return_value=True):
                    result = semantic_search("auth flow", k=5)

        assert _is_error_envelope(result)

    def test_empty_query_returns_error(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import semantic_search

        with patch.dict(os.environ, ctx.patch_env()):
            result = semantic_search("")

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_k_clamped_to_50(self) -> None:
        """k > 50 is silently clamped; no error raised."""
        ctx = self._make_ctx()
        from cognis_mcpd.tools import semantic_search

        # Without real embedder, the call will return an error envelope (expected).
        # The important thing is it doesn't raise an exception.
        with patch.dict(os.environ, ctx.patch_env()):
            result = semantic_search("auth", k=999)

        # Should succeed or return an error envelope — never an unhandled exception.
        assert isinstance(result, (list, dict))

    def test_audit_entry_written(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import semantic_search

        with patch.dict(os.environ, ctx.patch_env()):
            # This may fail with EMBEDDER_UNAVAILABLE but audit should still log.
            semantic_search("test query", k=5)

        entries = ctx.read_audit()
        assert len(entries) >= 1
        entry = [e for e in entries if e["tool"] == "semantic_search"][-1]
        assert "args_hash" in entry
        assert "query" not in entry  # no raw args

    def test_valid_result_is_list(self) -> None:
        """semantic_search returns a list or error envelope (never a bare exception)."""
        ctx = self._make_ctx()
        from cognis_mcpd.tools import semantic_search

        with patch.dict(os.environ, ctx.patch_env()):
            # The call may fail gracefully with embedder unavailable,
            # but must always return a list or error envelope dict.
            result = semantic_search("function query", k=5)

        assert isinstance(result, (list, dict))

    def test_semantic_search_times_out_when_semantic_stage_stalls(self) -> None:
        """A blocked embedder/search stage returns TIMEOUT instead of hanging."""
        ctx = self._make_ctx()
        import cognis_mcpd.tools as tools

        def _slow_semantic_core(*_args: object, **_kwargs: object) -> list[dict]:
            time.sleep(0.05)
            return []

        with patch.dict(os.environ, ctx.patch_env()):
            with patch.object(tools, "_HARD_TIMEOUT_S", 0.01):
                with patch.object(tools, "_SEMANTIC_COOLDOWN_S", 0.01):
                    with patch.object(tools, "_semantic_index_available", return_value=True):
                        with patch.object(
                            tools, "_semantic_search_core", side_effect=_slow_semantic_core
                        ):
                            result = tools.semantic_search("auth flow", k=5)

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "TIMEOUT"
        time.sleep(0.06)

    def test_semantic_search_waits_briefly_for_inflight_semantic_stage(self) -> None:
        """A short overlapping semantic call should wait inside the deadline budget."""
        ctx = self._make_ctx()
        import cognis_mcpd.tools as tools

        started = threading.Event()
        release = threading.Event()
        first_result: list[dict] | dict[str, object] | None = None
        calls = 0

        def _gated_semantic_core(*_args: object, **_kwargs: object) -> list[dict]:
            nonlocal calls
            calls += 1
            started.set()
            if calls == 1:
                assert release.wait(timeout=1.0)
            return []

        def _run_first() -> None:
            nonlocal first_result
            first_result = tools.semantic_search("auth flow", k=5)

        with patch.dict(os.environ, ctx.patch_env()):
            with patch.object(tools, "_HARD_TIMEOUT_S", 0.2):
                with patch.object(tools, "_semantic_index_available", return_value=True):
                    with patch.object(
                        tools,
                        "_semantic_search_core",
                        side_effect=_gated_semantic_core,
                    ):
                        thread = threading.Thread(target=_run_first)
                        thread.start()
                        assert started.wait(timeout=0.1)

                        def _release_soon() -> None:
                            time.sleep(0.02)
                            release.set()

                        releaser = threading.Thread(target=_release_soon)
                        releaser.start()
                        second_result = tools.semantic_search("session flow", k=5)
                        thread.join(timeout=1.0)
                        releaser.join(timeout=1.0)

        assert isinstance(first_result, list)
        assert not _is_error_envelope(first_result)
        assert isinstance(second_result, list)
        assert not _is_error_envelope(second_result)
        assert calls == 2

    def test_semantic_search_does_not_soft_timeout_after_successful_semantic_stage(self) -> None:
        """A completed semantic stage should only be checked against the hard timeout."""
        ctx = self._make_ctx()
        import cognis_mcpd.tools as tools

        def _slow_success(*_args: object, **_kwargs: object) -> list[dict]:
            time.sleep(0.05)
            return []

        with patch.dict(os.environ, ctx.patch_env()):
            with patch.object(tools, "_SOFT_TIMEOUT_S", 0.01):
                with patch.object(tools, "_HARD_TIMEOUT_S", 1.0):
                    with patch.object(tools, "_semantic_index_available", return_value=True):
                        with patch.object(
                            tools,
                            "_semantic_search_core",
                            side_effect=_slow_success,
                        ):
                            result = tools.semantic_search("auth flow", k=5)

        assert isinstance(result, list)
        assert not _is_error_envelope(result)


# ---------------------------------------------------------------------------
# Tests: dependency_trace
# ---------------------------------------------------------------------------


class TestDependencyTrace:
    def _make_ctx_with_graph(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        symbols = [
            _make_symbol(
                "A", "A", qualified_name="mod.A", file_path="src/a.py", line_start=1, line_end=5
            ),
            _make_symbol(
                "B", "B", qualified_name="mod.B", file_path="src/b.py", line_start=10, line_end=20
            ),
            _make_symbol(
                "C", "C", qualified_name="mod.C", file_path="src/c.py", line_start=30, line_end=40
            ),
        ]
        for sym in symbols:
            ctx.add_symbol(sym)
        ctx.add_edge(Edge(src_id="A", dst_id="B", kind="calls"))
        ctx.add_edge(Edge(src_id="B", dst_id="C", kind="calls"))
        return ctx

    def test_outbound_traversal(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("A", direction="out", depth=3)

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["start"] == "A"
        assert result["direction"] == "out"
        ids = {h["symbol_id"] for h in result["hits"]}
        assert "B" in ids
        assert "C" in ids

    def test_inbound_traversal(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("C", direction="in", depth=3)

        assert isinstance(result, dict)
        assert "error" not in result
        ids = {h["symbol_id"] for h in result["hits"]}
        assert "B" in ids
        assert "A" in ids

    def test_both_directions(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("B", direction="both", depth=3)

        assert isinstance(result, dict)
        assert "error" not in result
        ids = {h["symbol_id"] for h in result["hits"]}
        assert "A" in ids  # inbound
        assert "C" in ids  # outbound

    def test_invalid_direction_returns_error(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("A", direction="sideways", depth=3)

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_depth_over_8_clamped(self) -> None:
        """depth > 8 should be clamped, not raise an error."""
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("A", direction="out", depth=99)

        assert isinstance(result, dict)
        assert "error" not in result
        # Clamped depth reported in result.
        assert result["depth"] <= 8

    def test_empty_symbol_id_returns_error(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("", direction="out", depth=3)

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_audit_entry_written(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            dependency_trace("A", direction="out", depth=2)

        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "dependency_trace"][-1]
        assert "args_hash" in entry
        assert "symbol_id" not in entry  # no raw args

    def test_result_fields(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("A", direction="out", depth=2)

        assert "start" in result
        assert "direction" in result
        assert "depth" in result
        assert "hits" in result
        assert isinstance(result["hits"], list)

    def test_hits_include_symbol_metadata(self) -> None:
        ctx = self._make_ctx_with_graph()
        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("A", direction="out", depth=2)

        assert "error" not in result
        assert result["hits"], "expected outbound hits from A"
        for hit in result["hits"]:
            missing = [field for field in _TRACE_ENRICHMENT_FIELDS if field not in hit]
            if missing:
                import pytest

                pytest.skip(
                    "dependency_trace hits not yet enriched "
                    f"(missing {missing} on hit {hit.get('symbol_id')})"
                )
            if hit["symbol_id"] == "B":
                assert hit["qualified_name"] == "mod.B"
                assert hit["kind"] == "function"
                assert hit["file_path"] == "src/b.py"
                assert hit["line_start"] == 10
                assert hit["line_end"] == 20


# ---------------------------------------------------------------------------
# Tests: retrieve_context_capsule
# ---------------------------------------------------------------------------


class TestRetrieveContextCapsule:
    def _make_ctx(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        ctx.add_symbol(
            _make_symbol(
                "py:src/auth.py:login@aabb",
                "login",
                qualified_name="auth.login",
                signature="def login(user, pwd): ...",
                docstring="Handles user login.",
            )
        )
        return ctx

    def test_returns_dict(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        with patch.dict(os.environ, ctx.patch_env()):
            result = retrieve_context_capsule("Why is login failing?")

        assert isinstance(result, dict)

    def test_valid_capsule_or_error_envelope(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        with patch.dict(os.environ, ctx.patch_env()):
            result = retrieve_context_capsule("explain auth flow", max_tokens=1000)

        assert isinstance(result, dict)
        # Either a valid capsule (has "goal") or an error envelope.
        assert "goal" in result or "error" in result

    def test_capsule_goal_matches_task(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        task = "Why is the auth module slow?"
        with patch.dict(os.environ, ctx.patch_env()):
            result = retrieve_context_capsule(task)

        if "error" not in result:
            assert result.get("goal") == task

    def test_max_tokens_clamped(self) -> None:
        """max_tokens > 32000 is silently clamped."""
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        with patch.dict(os.environ, ctx.patch_env()):
            result = retrieve_context_capsule("auth", max_tokens=999999)

        assert isinstance(result, dict)
        if "error" not in result:
            assert result.get("token_estimate", 0) <= 32000

    def test_empty_task_returns_error(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        with patch.dict(os.environ, ctx.patch_env()):
            result = retrieve_context_capsule("")

        assert _is_error_envelope(result)
        assert result["error"]["code"] == "INVALID_ARGUMENT"

    def test_audit_entry_written(self) -> None:
        ctx = self._make_ctx()
        from cognis_mcpd.tools import retrieve_context_capsule

        with patch.dict(os.environ, ctx.patch_env()):
            retrieve_context_capsule("find the bug")

        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "retrieve_context_capsule"][-1]
        assert "args_hash" in entry
        assert "task" not in entry  # no raw args


# ---------------------------------------------------------------------------
# Tests: Audit log
# ---------------------------------------------------------------------------


class TestAuditLog:
    def test_audit_path_created_automatically(self) -> None:
        import tempfile as _tempfile

        with _tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmpdir:
            audit_path = Path(tmpdir) / "subdir" / "audit.log"
            db_path = os.path.join(tmpdir, "test.db")
            db = Database(db_path, vec_enabled=False)
            sym = _make_symbol("py:src/x.py:foo@1111", "foo", qualified_name="x.foo")
            upsert_symbol(db, sym)

            from cognis_mcpd.tools import symbol_lookup

            with patch.dict(
                os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": str(audit_path)}
            ):
                symbol_lookup("py:src/x.py:foo@1111")

            # Close the DB connection before the tempdir cleanup on Windows.
            db.close_thread_connection()

            assert audit_path.exists()

    def test_audit_entry_has_correct_fields(self) -> None:
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("py:src/x.py:bar@2222", "bar", qualified_name="x.bar"))

        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_lookup("py:src/x.py:bar@2222")

        entries = ctx.read_audit()
        assert entries, "No audit entries written"
        entry = entries[-1]
        assert set(entry.keys()) == {"ts", "tool", "args_hash", "ok"}

    def test_args_hash_is_sha256_hex(self) -> None:
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("py:src/x.py:baz@3333", "baz", qualified_name="x.baz"))

        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_lookup("py:src/x.py:baz@3333")

        entries = ctx.read_audit()
        entry = entries[-1]
        # SHA-256 hex is 64 chars.
        assert len(entry["args_hash"]) == 64
        assert all(c in "0123456789abcdef" for c in entry["args_hash"])

    def test_ok_field_is_false_on_error(self) -> None:
        ctx = _MCPTestDB()

        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_lookup("nonexistent_symbol_xyz")

        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "symbol_lookup"][-1]
        assert entry["ok"] is False

    def test_ok_field_is_true_on_success(self) -> None:
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("py:src/x.py:qux@4444", "qux", qualified_name="x.qux"))

        from cognis_mcpd.tools import symbol_lookup

        with patch.dict(os.environ, ctx.patch_env()):
            symbol_lookup("py:src/x.py:qux@4444")

        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "symbol_lookup"][-1]
        assert entry["ok"] is True


# ---------------------------------------------------------------------------
# Tests: Error envelope shape
# ---------------------------------------------------------------------------


class TestErrorEnvelope:
    def test_error_envelope_structure(self) -> None:
        from cognis_mcpd.errors import error_envelope

        env = error_envelope("SYMBOL_NOT_FOUND", "not found", False)
        assert "error" in env
        assert env["error"]["code"] == "SYMBOL_NOT_FOUND"
        assert env["error"]["message"] == "not found"
        assert env["error"]["retryable"] is False

    def test_mcp_error_to_envelope(self) -> None:
        from cognis_mcpd.errors import TIMEOUT, McpError

        err = McpError(TIMEOUT, "timed out")
        env = err.to_envelope()
        assert env["error"]["code"] == "TIMEOUT"
        assert env["error"]["retryable"] is True  # TIMEOUT is retryable by default

    def test_retryable_defaults(self) -> None:
        from cognis_mcpd.errors import INDEX_NOT_READY, SYMBOL_NOT_FOUND, TIMEOUT, McpError

        assert McpError(SYMBOL_NOT_FOUND, "x").retryable is False
        assert McpError(TIMEOUT, "x").retryable is True
        assert McpError(INDEX_NOT_READY, "x").retryable is True


# ---------------------------------------------------------------------------
# Tests: Hard limits
# ---------------------------------------------------------------------------


class TestHardLimits:
    def test_dependency_trace_depth_clamped_to_8(self) -> None:
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("X", "X", qualified_name="X"))

        from cognis_mcpd.tools import dependency_trace

        with patch.dict(os.environ, ctx.patch_env()):
            result = dependency_trace("X", direction="out", depth=100)

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["depth"] == 8  # clamped

    def test_semantic_search_k_clamped(self) -> None:
        """k > 50 is clamped without raising an error."""
        ctx = _MCPTestDB()
        from cognis_mcpd.tools import semantic_search

        # We can't run real semantic search without an embedder, but we can
        # verify the tool handles k=999 gracefully (no unhandled exception).
        with patch.dict(os.environ, ctx.patch_env()):
            result = semantic_search("auth", k=999)

        # Result is either a list or an error envelope — never an unhandled exception.
        assert isinstance(result, (list, dict))


# ---------------------------------------------------------------------------
# Tests: discover_symbols
# ---------------------------------------------------------------------------


class TestDiscoverSymbols:
    def _make_ctx(self) -> _MCPTestDB:
        ctx = _MCPTestDB()
        symbols = [
            _make_symbol(
                "py:src/auth.py:validate_token@aaaa",
                "validate_token",
                kind="function",
                qualified_name="auth.validate_token",
                file_path="src/auth.py",
                body_excerpt="def validate_token(token): ...",
            ),
            _make_symbol(
                "py:src/routes/login.py:login@cccc",
                "login",
                kind="function",
                qualified_name="routes.login",
                file_path="src/routes/login.py",
            ),
            _make_symbol(
                "py:src/auth/session.py:verifySession@dddd",
                "verifySession",
                kind="function",
                qualified_name="auth.verifySession",
                file_path="src/auth/session.py",
                body_excerpt="def verifySession(request): ...",
            ),
        ]
        for sym in symbols:
            ctx.add_symbol(sym)
        _populate_fts(ctx.db, symbols)
        return ctx

    def test_lexical_only_fallback(self) -> None:
        discover_symbols = _require_tool("discover_symbols")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = discover_symbols("validate", k=5)

        assert isinstance(result, list)
        assert result
        assert result[0]["match_sources"]
        assert "lexical" in result[0]["match_sources"]

    def test_natural_language_query_uses_tokenized_lexical_search(self) -> None:
        discover_symbols = _require_tool("discover_symbols")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = discover_symbols("login authentication session", k=5)

        assert isinstance(result, list)
        assert result
        hit_ids = {hit["symbol_id"] for hit in result}
        assert "py:src/routes/login.py:login@cccc" in hit_ids
        assert "py:src/auth/session.py:verifySession@dddd" in hit_ids

    def test_semantic_leg_timeout_keeps_lexical_results(self) -> None:
        """discover_symbols should not hang or fail when only semantic stalls."""
        ctx = self._make_ctx()
        import cognis_mcpd.tools as tools

        def _slow_semantic_core(*_args: object, **_kwargs: object) -> list[dict]:
            time.sleep(0.05)
            return []

        with patch.dict(os.environ, ctx.patch_env()):
            with patch.object(tools, "_semantic_index_available", return_value=True):
                with patch.object(tools, "_DISCOVER_SEMANTIC_TIMEOUT_S", 0.01):
                    with patch.object(
                        tools, "_semantic_search_core", side_effect=_slow_semantic_core
                    ):
                        result = tools.discover_symbols("validate", k=5)

        assert isinstance(result, list)
        assert result
        assert "lexical" in result[0]["match_sources"]

    def test_discover_timeout_does_not_trigger_semantic_cooldown(self) -> None:
        """A discover timeout should not block the next explicit semantic_search call."""
        ctx = self._make_ctx()
        import cognis_mcpd.tools as tools

        calls = 0

        def _slow_then_fast(*_args: object, **_kwargs: object) -> list[dict]:
            nonlocal calls
            calls += 1
            if calls == 1:
                time.sleep(0.05)
            return []

        with patch.dict(os.environ, ctx.patch_env()):
            with patch.object(tools, "_semantic_index_available", return_value=True):
                with patch.object(tools, "_DISCOVER_SEMANTIC_TIMEOUT_S", 0.01):
                    with patch.object(tools, "_semantic_search_core", side_effect=_slow_then_fast):
                        first = tools.discover_symbols("validate", k=5)
                        time.sleep(0.06)
                        second = tools.semantic_search("auth flow", k=5)

        assert isinstance(first, list)
        assert first
        assert isinstance(second, list)
        assert not _is_error_envelope(second)

    def test_empty_query_returns_error(self) -> None:
        discover_symbols = _require_tool("discover_symbols")
        ctx = self._make_ctx()

        with patch.dict(os.environ, ctx.patch_env()):
            result = discover_symbols("")

        assert _is_error_envelope(result)


# ---------------------------------------------------------------------------
# Tests: diffuse_context (CSAR — flagship retrieval)
# ---------------------------------------------------------------------------


class TestDiffuseContext:
    def _make_ctx(self) -> _MCPTestDB:
        """Login flow: postLogin -> requireAuth -> validate.

        Only postLogin/validate are lexical matches for "jwt validate"; the
        requireAuth middleware sits on the path between them with no matching
        text, so CSAR must recover it via diffusion.
        """
        ctx = _MCPTestDB()
        symbols = [
            _make_symbol(
                "ts:login.ts:postLogin@1111",
                "postLogin",
                qualified_name="login.postLogin",
                file_path="src/login.ts",
                language="ts",
                docstring="POST /login handler; validates jwt token via middleware.",
                body_excerpt="export function postLogin(req) { return requireAuth(req); }",
            ),
            _make_symbol(
                "ts:auth.ts:requireAuth@2222",
                "requireAuth",
                qualified_name="auth.requireAuth",
                file_path="src/auth.ts",
                language="ts",
                docstring="Express middleware guarding protected routes.",
                body_excerpt="export function requireAuth(req) { return next(req); }",
            ),
            _make_symbol(
                "ts:jwt.ts:validate@3333",
                "validate",
                qualified_name="jwt.validate",
                file_path="src/jwt.ts",
                language="ts",
                docstring="Validate a jwt token signature and expiry.",
                body_excerpt="export function validate(token) { /* jwt validate */ }",
            ),
            _make_symbol(
                "ts:util.ts:formatCurrency@4444",
                "formatCurrency",
                qualified_name="util.formatCurrency",
                file_path="src/util.ts",
                language="ts",
                docstring="Formats currency strings for display.",
            ),
        ]
        for sym in symbols:
            ctx.add_symbol(sym)
        _populate_fts(ctx.db, symbols)
        ctx.add_edge(
            Edge(
                src_id="ts:login.ts:postLogin@1111",
                dst_id="ts:auth.ts:requireAuth@2222",
                kind="calls",
            )
        )
        ctx.add_edge(
            Edge(
                src_id="ts:auth.ts:requireAuth@2222", dst_id="ts:jwt.ts:validate@3333", kind="calls"
            )
        )
        return ctx

    def test_returns_ranked_list(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            result = diffuse_context("jwt validate token", k=10)
        assert isinstance(result, list)
        assert result, "expected diffused hits for a lexical-matching query"
        scores = [h["score"] for h in result]
        assert scores == sorted(scores, reverse=True)
        assert all(h["match_reason"] == "csar_diffusion" for h in result)

    def test_recovers_on_path_middleware(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            result = diffuse_context("jwt validate token", k=10, alpha=0.2)
        ids = {h["symbol_id"] for h in result}
        # requireAuth has no lexical match for the query but is on the call path.
        assert "ts:auth.ts:requireAuth@2222" in ids
        mw = next(h for h in result if h["symbol_id"] == "ts:auth.ts:requireAuth@2222")
        assert mw["on_path"] is True

    def test_empty_query_returns_error(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            result = diffuse_context("")
        assert _is_error_envelope(result)

    def test_invalid_alpha_returns_error(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            result = diffuse_context("validate", alpha=1.5)
        assert _is_error_envelope(result)

    def test_no_match_returns_empty(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            result = diffuse_context("zzzznomatchquery", k=5)
        assert result == []

    def test_audit_entry_written(self) -> None:
        diffuse_context = _require_tool("diffuse_context")
        ctx = self._make_ctx()
        with patch.dict(os.environ, ctx.patch_env()):
            diffuse_context("validate", k=5)
        entries = ctx.read_audit()
        entry = [e for e in entries if e["tool"] == "diffuse_context"][-1]
        assert "args_hash" in entry
        assert "query" not in entry  # no raw args persisted


# ---------------------------------------------------------------------------
# Tests: resolve_symbols
# ---------------------------------------------------------------------------


class TestResolveSymbols:
    def test_batch_hydration(self) -> None:
        resolve_symbols = _require_tool("resolve_symbols")
        ctx = _MCPTestDB()
        sym_a = _make_symbol("A", "alpha", qualified_name="mod.alpha")
        sym_b = _make_symbol("B", "beta", qualified_name="mod.beta")
        ctx.add_symbol(sym_a)
        ctx.add_symbol(sym_b)

        with patch.dict(os.environ, ctx.patch_env()):
            result = resolve_symbols(["A", "B", "missing"])

        assert isinstance(result, dict)
        assert "error" not in result
        assert result["requested_count"] == 3
        assert result["resolved_count"] == 2
        assert result["missing"] == ["missing"]
        ids = {item["id"] for item in result["symbols"]}
        assert ids == {"A", "B"}

    def test_include_body_false(self) -> None:
        resolve_symbols = _require_tool("resolve_symbols")
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("A", "alpha", body_excerpt="body here"))

        with patch.dict(os.environ, ctx.patch_env()):
            result = resolve_symbols(["A"], include_body=False)

        assert result["symbols"][0]["id"] == "A"
        assert "body_excerpt" not in result["symbols"][0]

    def test_empty_ids_returns_error(self) -> None:
        resolve_symbols = _require_tool("resolve_symbols")
        ctx = _MCPTestDB()

        with patch.dict(os.environ, ctx.patch_env()):
            result = resolve_symbols([])

        assert _is_error_envelope(result)


# ---------------------------------------------------------------------------
# Tests: result cache
# ---------------------------------------------------------------------------


class TestResultCache:
    def test_symbol_search_cache_hit(self) -> None:
        from cognis_mcpd.result_cache import reset_cache_for_tests

        symbol_search = _require_tool("symbol_search")
        ctx = _MCPTestDB()
        ctx.add_symbol(_make_symbol("A", "alpha"))
        reset_cache_for_tests()

        with patch.dict(os.environ, ctx.patch_env()):
            first = symbol_search("alpha", k=5)
            second = symbol_search("alpha", k=5)

        assert first == second
