"""Unit tests for MCP runtime-level caching helpers.

These tests cover process-level singleton reuse that reduces repeated MCP tool
latency across calls in a long-lived server process.
"""

from __future__ import annotations

import os
import sys
import time
import types

import pytest


def test_shared_semantic_layer_is_singleton(monkeypatch) -> None:
    from cognis_mcpd import embedder_pool

    embedder_pool.reset_shared_semantic_layer_for_tests()
    sentinel_embedder = object()
    monkeypatch.setattr(embedder_pool, "get_shared_embedder", lambda: sentinel_embedder)

    semantic_module = types.ModuleType("cognis_retrieval.semantic")

    class DummySemanticLayer:
        def __init__(self, embedder: object) -> None:
            self.embedder = embedder

    semantic_module.SemanticLayer = DummySemanticLayer  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "cognis_retrieval.semantic", semantic_module)

    first = embedder_pool.get_shared_semantic_layer()
    second = embedder_pool.get_shared_semantic_layer()

    assert first is second
    assert first.embedder is sentinel_embedder

    embedder_pool.reset_shared_semantic_layer_for_tests()


def test_reset_shared_semantic_layer_creates_new_instance(monkeypatch) -> None:
    from cognis_mcpd import embedder_pool

    embedder_pool.reset_shared_semantic_layer_for_tests()
    monkeypatch.setattr(embedder_pool, "get_shared_embedder", lambda: object())

    semantic_module = types.ModuleType("cognis_retrieval.semantic")

    class DummySemanticLayer:
        def __init__(self, embedder: object) -> None:
            self.embedder = embedder

    semantic_module.SemanticLayer = DummySemanticLayer  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "cognis_retrieval.semantic", semantic_module)

    first = embedder_pool.get_shared_semantic_layer()
    embedder_pool.reset_shared_semantic_layer_for_tests()
    second = embedder_pool.get_shared_semantic_layer()

    assert first is not second

    embedder_pool.reset_shared_semantic_layer_for_tests()


def test_get_db_reuses_database_for_same_path(monkeypatch) -> None:
    import cognis_mcpd.tools as tools

    calls: list[str] = []

    class DummyDatabase:
        def __init__(self, path: str) -> None:
            self.path = path
            calls.append(path)

    monkeypatch.setattr(tools, "Database", DummyDatabase)
    monkeypatch.setattr(tools, "_DB_CACHE", {})
    monkeypatch.setenv("COGNIS_DB_PATH", "tmp/test-cache.db")

    first = tools._get_db()
    second = tools._get_db()

    assert first is second
    assert calls == [os.path.abspath("tmp/test-cache.db")]


def test_get_db_separates_distinct_paths(monkeypatch) -> None:
    import cognis_mcpd.tools as tools

    calls: list[str] = []

    class DummyDatabase:
        def __init__(self, path: str) -> None:
            self.path = path
            calls.append(path)

    monkeypatch.setattr(tools, "Database", DummyDatabase)
    monkeypatch.setattr(tools, "_DB_CACHE", {})

    monkeypatch.setenv("COGNIS_DB_PATH", "tmp/one.db")
    first = tools._get_db()

    monkeypatch.setenv("COGNIS_DB_PATH", "tmp/two.db")
    second = tools._get_db()

    assert first is not second
    assert calls == [os.path.abspath("tmp/one.db"), os.path.abspath("tmp/two.db")]


def test_semantic_stage_timeout_enters_cooldown(monkeypatch) -> None:
    import cognis_mcpd.tools as tools
    from cognis_mcpd.errors import TIMEOUT, McpError

    tools._SEMANTIC_DISABLED_UNTIL = 0.0
    monkeypatch.setattr(tools, "_HARD_TIMEOUT_S", 0.01)
    monkeypatch.setattr(tools, "_SEMANTIC_COOLDOWN_S", 0.05)

    def slow_stage() -> list[dict]:
        time.sleep(0.05)
        return []

    with pytest.raises(McpError) as first:
        tools._run_semantic_with_deadline(
            "semantic_search",
            "semantic_retrieval",
            time.perf_counter(),
            slow_stage,
        )

    assert first.value.code == TIMEOUT

    with pytest.raises(McpError) as second:
        tools._run_semantic_with_deadline(
            "semantic_search",
            "semantic_retrieval",
            time.perf_counter(),
            lambda: [],
        )

    assert second.value.code == TIMEOUT
    time.sleep(0.06)
    tools._SEMANTIC_DISABLED_UNTIL = 0.0
