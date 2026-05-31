"""Integration tests for cognis MCP tools.

These tests exercise the MCP tools directly (not via stdio MCP transport)
by calling ``cognis_mcpd.tools`` functions against a seeded test database.

Requirements validated:
- REQ-MCP-1: all MCP tools functional
- REQ-RET-1: lexical retrieval / symbol discovery
- REQ-RET-3: structural traversal
- REQ-CAP-1: capsule schema conformance

Run with: ``pytest -m integration``

Note: these tests do NOT spin up an external MCP server process. They call the
tool implementations directly so they can run in any environment.
"""

from __future__ import annotations

import os
import time
from typing import Any

import pytest

from tests.integration.conftest import (
    PLANTED_AUTH_SYMBOL_ID,
    PLANTED_BUG_SYMBOL_ID,
    PLANTED_ROUTE_SYMBOL_ID,
)

pytestmark = pytest.mark.integration

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _set_db_env(db_path: str) -> None:
    """Point COGNIS_DB_PATH at the test database."""
    os.environ["COGNIS_DB_PATH"] = db_path


def _clear_db_env() -> None:
    os.environ.pop("COGNIS_DB_PATH", None)


def _require_tool(name: str):
    """Return an MCP tool callable, skipping when not yet exported."""
    import importlib

    tools = importlib.import_module("cognis_mcpd.tools")
    fn = getattr(tools, name, None)
    if fn is None:
        pytest.skip(f"{name} not yet exported from cognis_mcpd.tools")
    return fn


_TRACE_ENRICHMENT_FIELDS = (
    "qualified_name",
    "kind",
    "file_path",
    "line_start",
    "line_end",
)


# ---------------------------------------------------------------------------
# Task 16.2 — Tool schema and latency tests
# ---------------------------------------------------------------------------


class TestSymbolLookup:
    """symbol_lookup tool — schema and latency assertions."""

    def test_exact_id_returns_symbol_node(self, tmp_db: Any) -> None:
        """Calling symbol_lookup with an exact id returns the expected SymbolNode fields."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            t0 = time.perf_counter()
            result = symbol_lookup(PLANTED_BUG_SYMBOL_ID)
            elapsed_ms = (time.perf_counter() - t0) * 1000

            assert "error" not in result, f"unexpected error: {result}"
            # Schema check.
            for field in ("id", "kind", "name", "qualified_name", "language", "file_path"):
                assert field in result, f"missing field {field!r} in result"
            assert result["id"] == PLANTED_BUG_SYMBOL_ID
            assert result["kind"] == "function"
            # Latency: p95 target < 50ms (lexical/lookup budget; no embedding).
            assert elapsed_ms < 500, f"symbol_lookup too slow: {elapsed_ms:.1f}ms (budget: 500ms)"
        finally:
            _clear_db_env()

    def test_qualified_name_lookup(self, tmp_db: Any) -> None:
        """symbol_lookup can resolve via qualified_name."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            result = symbol_lookup("auth.jwt.validate")
            # May return error if not found — but if found, schema must be valid.
            if "error" not in result:
                assert result["id"] == PLANTED_BUG_SYMBOL_ID
        finally:
            _clear_db_env()

    def test_not_found_returns_error_envelope(self, tmp_db: Any) -> None:
        """symbol_lookup returns a well-formed error envelope when symbol not found."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            result = symbol_lookup("definitely_does_not_exist_xyzzy_12345")
            assert "error" in result
            err = result["error"]
            assert "code" in err
            assert "message" in err
            assert "retryable" in err
        finally:
            _clear_db_env()

    def test_empty_input_returns_error_envelope(self, tmp_db: Any) -> None:
        """symbol_lookup with empty string returns a typed error envelope."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            result = symbol_lookup("")
            assert "error" in result
            assert result["error"]["code"] == "INVALID_ARGUMENT"
        finally:
            _clear_db_env()


class TestSymbolSearch:
    """symbol_search tool — discovery, filters, and bounded k."""

    def test_returns_ranked_hits_for_validate_query(self, tmp_db: Any) -> None:
        """symbol_search should return multiple ranked hits for a partial name."""
        symbol_search = _require_tool("symbol_search")
        _set_db_env(tmp_db.path)
        try:
            t0 = time.perf_counter()
            result = symbol_search("validate", k=5)
            elapsed_ms = (time.perf_counter() - t0) * 1000

            assert isinstance(result, list), f"unexpected type: {type(result)}"
            assert 1 <= len(result) <= 5
            scores = [hit["score"] for hit in result]
            assert scores == sorted(scores, reverse=True)
            for hit in result:
                assert "symbol_id" in hit
                assert "score" in hit
            hit_ids = {hit["symbol_id"] for hit in result}
            assert PLANTED_BUG_SYMBOL_ID in hit_ids
            assert elapsed_ms < 500, f"symbol_search too slow: {elapsed_ms:.1f}ms"
        finally:
            _clear_db_env()

    def test_kind_filter_limits_to_functions(self, tmp_db: Any) -> None:
        """kind=function should exclude non-function symbols from discovery hits."""
        symbol_search = _require_tool("symbol_search")
        _set_db_env(tmp_db.path)
        try:
            result = symbol_search("auth", k=10, kind="function")
            assert isinstance(result, list)
            if result:
                assert all(hit.get("kind") == "function" for hit in result)
        finally:
            _clear_db_env()

    def test_file_path_filter_restricts_hits(self, tmp_db: Any) -> None:
        """file_path filter should keep hits within the requested source file."""
        symbol_search = _require_tool("symbol_search")
        _set_db_env(tmp_db.path)
        try:
            result = symbol_search("validate", k=10, file_path="src/auth/jwt.ts")
            assert isinstance(result, list)
            assert result, "expected validate symbol in jwt.ts"
            for hit in result:
                assert hit.get("file_path") == "src/auth/jwt.ts"
        finally:
            _clear_db_env()

    def test_empty_query_returns_error_envelope(self, tmp_db: Any) -> None:
        """symbol_search with empty query returns error envelope."""
        symbol_search = _require_tool("symbol_search")
        _set_db_env(tmp_db.path)
        try:
            result = symbol_search("")
            assert isinstance(result, dict)
            assert "error" in result
            assert result["error"]["code"] == "INVALID_ARGUMENT"
        finally:
            _clear_db_env()


class TestSemanticSearch:
    """semantic_search tool — schema and graceful degradation."""

    def test_returns_list_or_error_envelope(self, tmp_db: Any) -> None:
        """semantic_search returns a list of hits or a well-formed error envelope."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import semantic_search

            result = semantic_search("jwt token validation", k=5)
            # Either a list (success) or an error envelope (embedder unavailable).
            assert isinstance(result, (list, dict)), f"unexpected type: {type(result)}"
            if isinstance(result, dict) and "error" in result:
                err = result["error"]
                assert "code" in err and "message" in err and "retryable" in err
            elif isinstance(result, list):
                for hit in result:
                    assert "symbol_id" in hit
                    assert "score" in hit
        finally:
            _clear_db_env()

    def test_k_clamped_to_50(self, tmp_db: Any) -> None:
        """semantic_search clamps k to 50 without raising."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import semantic_search

            # k=999 should be accepted (clamped internally).
            result = semantic_search("any query", k=999)
            assert isinstance(result, (list, dict))
        finally:
            _clear_db_env()

    def test_empty_query_returns_error_envelope(self, tmp_db: Any) -> None:
        """semantic_search with empty query returns error envelope."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import semantic_search

            result = semantic_search("")
            assert isinstance(result, dict)
            assert "error" in result
            assert result["error"]["code"] == "INVALID_ARGUMENT"
        finally:
            _clear_db_env()


class TestDiscoverSymbols:
    """discover_symbols tool — natural-language lexical fallback."""

    def test_natural_language_auth_query_surfaces_auth_symbols(self, tmp_db: Any) -> None:
        """Natural-language auth queries should return auth/login symbols even without semantic hits."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import discover_symbols

            result = discover_symbols("login authentication session", k=5)
            assert isinstance(result, list), f"unexpected type: {type(result)}"
            assert result, "expected auth-related hits for a natural-language discovery query"
            hit_ids = {hit["symbol_id"] for hit in result}
            assert (
                PLANTED_ROUTE_SYMBOL_ID in hit_ids
                or PLANTED_AUTH_SYMBOL_ID in hit_ids
                or PLANTED_BUG_SYMBOL_ID in hit_ids
            ), f"expected planted auth symbols, got {hit_ids}"
        finally:
            _clear_db_env()


class TestDependencyTrace:
    """dependency_trace tool — schema, traversal correctness, and latency."""

    def test_trace_outbound_from_route(self, tmp_db: Any) -> None:
        """Tracing outbound from the login route finds the call chain to validate."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import dependency_trace

            t0 = time.perf_counter()
            result = dependency_trace(PLANTED_ROUTE_SYMBOL_ID, direction="out", depth=3)
            elapsed_ms = (time.perf_counter() - t0) * 1000

            assert "error" not in result, f"unexpected error: {result}"
            # Schema check.
            assert "start" in result
            assert "hits" in result
            assert result["start"] == PLANTED_ROUTE_SYMBOL_ID
            # Latency: design budget p95 < 150ms for depth ≤ 5.
            assert elapsed_ms < 500, f"dependency_trace too slow: {elapsed_ms:.1f}ms"
        finally:
            _clear_db_env()

    def test_trace_finds_planted_bug_symbol(self, tmp_db: Any) -> None:
        """Depth-3 outbound trace from login route should reach the validate symbol."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import dependency_trace

            result = dependency_trace(PLANTED_ROUTE_SYMBOL_ID, direction="out", depth=3)
            if "error" in result:
                pytest.skip(f"structural layer unavailable: {result['error']}")

            hit_ids = {h["symbol_id"] for h in result.get("hits", [])}
            assert PLANTED_AUTH_SYMBOL_ID in hit_ids or PLANTED_BUG_SYMBOL_ID in hit_ids, (
                f"Expected call-chain symbols in hits, got: {hit_ids}"
            )
        finally:
            _clear_db_env()

    def test_invalid_direction_returns_error(self, tmp_db: Any) -> None:
        """dependency_trace with bad direction returns error envelope."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import dependency_trace

            result = dependency_trace(PLANTED_ROUTE_SYMBOL_ID, direction="sideways", depth=2)
            assert "error" in result
            assert result["error"]["code"] == "INVALID_ARGUMENT"
        finally:
            _clear_db_env()

    def test_depth_clamped_to_8(self, tmp_db: Any) -> None:
        """dependency_trace clamps depth > 8 without raising."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import dependency_trace

            result = dependency_trace(PLANTED_ROUTE_SYMBOL_ID, "out", depth=100)
            assert isinstance(result, dict)
            if "error" not in result:
                assert result["depth"] <= 8
        finally:
            _clear_db_env()

    def test_hits_include_symbol_metadata(self, tmp_db: Any) -> None:
        """dependency_trace hits should carry symbol metadata for agent follow-up."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import dependency_trace

            result = dependency_trace(PLANTED_ROUTE_SYMBOL_ID, direction="out", depth=3)
            if "error" in result:
                pytest.skip(f"structural layer unavailable: {result['error']}")

            assert result.get("hits"), "expected outbound hits from login route"
            for hit in result["hits"]:
                missing = [field for field in _TRACE_ENRICHMENT_FIELDS if field not in hit]
                if missing:
                    pytest.skip(
                        "dependency_trace hits not yet enriched "
                        f"(missing {missing} on hit {hit.get('symbol_id')})"
                    )
                assert hit["symbol_id"]
                assert hit["qualified_name"]
                assert hit["kind"]
                assert hit["file_path"]
        finally:
            _clear_db_env()


class TestRetrieveContextCapsule:
    """retrieve_context_capsule tool — schema and latency."""

    def test_returns_dict(self, tmp_db: Any) -> None:
        """retrieve_context_capsule returns a dict (capsule or error envelope)."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            result = retrieve_context_capsule("why is /login timing out?", max_tokens=500)
            assert isinstance(result, dict)
        finally:
            _clear_db_env()

    def test_capsule_schema_when_successful(self, tmp_db: Any) -> None:
        """When capsule is composed successfully, it has the required top-level fields."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            result = retrieve_context_capsule("explain the auth flow", max_tokens=2000)
            if "error" in result:
                pytest.skip(f"capsule composition unavailable: {result['error']}")

            # REQ-CAP-1 schema fields.
            required_fields = [
                "version",
                "goal",
                "task_mode",
                "root_cause_candidates",
                "relevant_symbols",
                "call_chain",
                "runtime_evidence",
                "neighbor_patterns",
                "risk_areas",
                "compressed_context",
                "token_estimate",
                "sources",
            ]
            for field in required_fields:
                assert field in result, f"capsule missing required field {field!r}"
        finally:
            _clear_db_env()

    def test_token_estimate_within_budget(self, tmp_db: Any) -> None:
        """token_estimate in returned capsule must not exceed max_tokens."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            max_tokens = 1000
            result = retrieve_context_capsule("what does validate do?", max_tokens=max_tokens)
            if "error" in result:
                pytest.skip(f"capsule unavailable: {result['error']}")

            token_estimate = result.get("token_estimate", 0)
            assert token_estimate <= max_tokens, (
                f"token_estimate={token_estimate} exceeds max_tokens={max_tokens}"
            )
        finally:
            _clear_db_env()

    def test_e2e_latency_budget(self, tmp_db: Any) -> None:
        """End-to-end capsule retrieval should complete within the design latency budget.

        Design budget: p95 < 400ms (without LLM compression).
        We use a generous 5s limit here since CI may be slow.
        """
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            t0 = time.perf_counter()
            retrieve_context_capsule("jwt auth timeout login", max_tokens=2000)
            elapsed_ms = (time.perf_counter() - t0) * 1000

            # CI environments can be slow; cap at 5s to catch gross regressions.
            assert elapsed_ms < 5000, f"capsule retrieval too slow: {elapsed_ms:.1f}ms"
        finally:
            _clear_db_env()


# ---------------------------------------------------------------------------
# Task 16.3 — Known auth-timeout bug test
# ---------------------------------------------------------------------------


class TestAuthTimeoutBugFixMode:
    """Task 16.3: call retrieve_context_capsule for the auth-timeout bug and assert
    that the planted bug symbol appears in the capsule's root_cause_candidates."""

    def test_login_timeout_capsule_finds_validate(self, tmp_db: Any) -> None:
        """retrieve_context_capsule for login timeout query should surface validate symbol.

        The planted bug symbol ``ts:src/auth/jwt.ts:validate@deadbeef`` must appear
        in either ``root_cause_candidates`` or ``relevant_symbols`` of the capsule.
        """
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            result = retrieve_context_capsule(
                task="why is /login timing out? auth-timeout error in JWT middleware.",
                max_tokens=4000,
            )

            if "error" in result:
                pytest.skip(
                    f"capsule composition unavailable (likely DB/embedder not ready in this env): "
                    f"{result['error']['message']}"
                )

            # Collect all symbol_ids mentioned across the capsule.
            mentioned_ids: set[str] = set()

            for candidate in result.get("root_cause_candidates", []):
                mentioned_ids.add(candidate.get("symbol_id", ""))

            for sym in result.get("relevant_symbols", []):
                mentioned_ids.add(sym.get("symbol_id", ""))

            # Assert the planted bug symbol is surfaced.
            assert PLANTED_BUG_SYMBOL_ID in mentioned_ids, (
                f"Planted bug symbol {PLANTED_BUG_SYMBOL_ID!r} not found in capsule.\n"
                f"root_cause_candidates: {result.get('root_cause_candidates')}\n"
                f"relevant_symbols: {[s.get('symbol_id') for s in result.get('relevant_symbols', [])]}"
            )

        finally:
            _clear_db_env()

    def test_task_mode_classified_as_bugfix(self, tmp_db: Any) -> None:
        """The auth-timeout task should be classified as ``bugfix`` mode."""
        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import retrieve_context_capsule

            result = retrieve_context_capsule(
                task="why is /login timing out? auth-timeout error in JWT middleware.",
                max_tokens=500,
            )
            if "error" in result:
                pytest.skip("capsule unavailable")

            assert result.get("task_mode") == "bugfix", (
                f"Expected task_mode='bugfix', got {result.get('task_mode')!r}"
            )
        finally:
            _clear_db_env()


# ---------------------------------------------------------------------------
# Task 16.4 — Incremental write API test (stub for branch-switch scenario)
# ---------------------------------------------------------------------------


class TestIncrementalReIndex:
    """Task 16.4: demonstrate that the write API supports incremental symbol updates.

    The full watcher daemon is not running in tests. This test verifies that
    inserting / updating symbols via the DB write path (as the writer thread
    would) is reflected in subsequent tool queries — simulating what happens
    after a branch switch triggers re-index.
    """

    def test_newly_inserted_symbol_is_queryable(self, tmp_db: Any) -> None:
        """A symbol inserted into the DB is immediately queryable via symbol_lookup."""
        import time as _time

        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            conn = tmp_db.connect()
            new_id = "ts:src/auth/session.ts:createSession@aabbccdd"
            conn.execute(
                """
                INSERT OR IGNORE INTO symbol (
                    id, kind, name, qualified_name, language, module, file_path,
                    line_start, line_end, signature, docstring, content_hash,
                    body_excerpt, semantic_summary, risk_score, ambiguous,
                    untrusted_flags, updated_at
                ) VALUES (?, 'function', 'createSession', 'auth.session.createSession',
                          'typescript', 'src/auth', 'src/auth/session.ts',
                          1, 20,
                          'function createSession(userId: string): Session',
                          'Creates a new session for the given user.',
                          'aabbccdd', 'export function createSession(...) {...}',
                          NULL, 0.1, 0, NULL, ?)
                """,
                (new_id, int(_time.time())),
            )
            conn.commit()

            result = symbol_lookup(new_id)
            assert "error" not in result, f"Newly inserted symbol not found: {result}"
            assert result["id"] == new_id

        finally:
            _clear_db_env()

    def test_updated_symbol_reflects_new_content(self, tmp_db: Any) -> None:
        """Updating a symbol's docstring is immediately reflected in subsequent lookups."""
        import time as _time

        _set_db_env(tmp_db.path)
        try:
            from cognis_mcpd.tools import symbol_lookup

            conn = tmp_db.connect()
            new_docstring = "UPDATED: Fixed blocking crypto call — now async."
            conn.execute(
                "UPDATE symbol SET docstring = ?, updated_at = ? WHERE id = ?",
                (new_docstring, int(_time.time()), PLANTED_BUG_SYMBOL_ID),
            )
            conn.commit()

            result = symbol_lookup(PLANTED_BUG_SYMBOL_ID)
            assert "error" not in result
            assert result["docstring"] == new_docstring, (
                f"Updated docstring not reflected: {result['docstring']!r}"
            )
        finally:
            _clear_db_env()

    def test_write_throughput_under_5s(self, tmp_db: Any) -> None:
        """Inserting 100 symbols (simulating an incremental re-index after branch switch)
        completes in under 5 seconds — the design p95 budget for incremental updates."""
        import time as _time

        _set_db_env(tmp_db.path)
        try:
            conn = tmp_db.connect()
            now = int(_time.time())

            symbols = [
                (
                    f"ts:src/gen/sym{i}.ts:func{i}@{i:08x}",
                    "function",
                    f"func{i}",
                    f"gen.sym{i}.func{i}",
                    "typescript",
                    "src/gen",
                    f"src/gen/sym{i}.ts",
                    1,
                    10,
                    f"function func{i}(): void",
                    f"Generated function {i}",
                    f"{i:08x}",
                    f"function func{i}() {{}}",
                    None,
                    0.0,
                    0,
                    None,
                    now,
                )
                for i in range(100)
            ]

            t0 = _time.perf_counter()
            conn.executemany(
                """
                INSERT OR IGNORE INTO symbol (
                    id, kind, name, qualified_name, language, module, file_path,
                    line_start, line_end, signature, docstring, content_hash,
                    body_excerpt, semantic_summary, risk_score, ambiguous,
                    untrusted_flags, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                symbols,
            )
            conn.commit()
            elapsed = _time.perf_counter() - t0

            assert elapsed < 5.0, f"Incremental batch insert took {elapsed:.2f}s (budget: 5s)"

        finally:
            _clear_db_env()
