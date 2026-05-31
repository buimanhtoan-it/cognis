"""Property-based tests for MCP tools (CP-10).

**Validates: Requirements 22.1, 22.2** (REQ-MCP-1 tool surface stability).

CP-10: For any input on any MCP tool:
  - Returns a typed result conforming to its schema, OR
  - Returns a typed error envelope {"error": {"code", "message", "retryable"}}.
  - An unhandled exception MUST NOT escape the tool handler.

Tests use Hypothesis to generate arbitrary inputs. The tools are tested against
a real (temp file) SQLite database to ensure no I/O-related uncaught exceptions
escape.

Framework: hypothesis (see design.md Correctness Properties section).
"""

from __future__ import annotations

import os
import tempfile
import time
from typing import Any
from unittest.mock import patch

import pytest
from cognis.db import Database, upsert_edge, upsert_symbol
from cognis.models import Edge, SymbolNode
from hypothesis import given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Shared strategies
# ---------------------------------------------------------------------------

# Non-empty printable string (avoids control characters that can break SQLite
# or hypothesis shrinking).
_printable = st.text(
    alphabet=st.characters(whitelist_categories=("Lu", "Ll", "Nd", "Pc", "Zs")),
    min_size=1,
    max_size=200,
)

# Plausible symbol_id strings.
_symbol_id_str = st.text(
    alphabet=st.characters(whitelist_categories=("Lu", "Ll", "Nd", "Pc")),
    min_size=1,
    max_size=100,
)

# Integers that might be k or depth values (including edge cases).
_k_int = st.integers(min_value=-1000, max_value=1000)
_depth_int = st.integers(min_value=-100, max_value=100)
_tokens_int = st.integers(min_value=-1000, max_value=100_000)

# Direction strings (including invalid ones to test error handling).
_direction_str = st.one_of(
    st.just("out"),
    st.just("in"),
    st.just("both"),
    st.text(min_size=0, max_size=20),
)

# Optional strings (None or a printable string).
_opt_str = st.one_of(st.none(), _printable)


# ---------------------------------------------------------------------------
# DB fixture factory
# ---------------------------------------------------------------------------


def _make_test_db() -> tuple[Database, str, str]:
    """Create a temp DB with one symbol and return (db, db_path, audit_path)."""
    tmpdir = tempfile.mkdtemp()
    db_path = os.path.join(tmpdir, "test.db")
    audit_path = os.path.join(tmpdir, "audit.log")

    db = Database(db_path, vec_enabled=False)
    sym = SymbolNode(
        id="py:src/test.py:sample_func@abcdef01",
        kind="function",
        name="sample_func",
        qualified_name="test_module.sample_func",
        language="python",
        module="test_module",
        file_path="src/test.py",
        line_start=1,
        line_end=10,
        signature="def sample_func(): ...",
        docstring="Sample function for PBT.",
        content_hash="abcdef0123456789",
        body_excerpt="def sample_func(): pass",
        updated_at=int(time.time()),
    )
    upsert_symbol(db, sym)
    # Add a simple edge for structural tests.
    sym2 = SymbolNode(
        id="py:src/test.py:helper@abcdef02",
        kind="function",
        name="helper",
        qualified_name="test_module.helper",
        language="python",
        module="test_module",
        file_path="src/test.py",
        line_start=12,
        line_end=20,
        signature="def helper(): ...",
        content_hash="abcdef0223456789",
        updated_at=int(time.time()),
    )
    upsert_symbol(db, sym2)
    upsert_edge(
        db,
        Edge(
            src_id="py:src/test.py:sample_func@abcdef01",
            dst_id="py:src/test.py:helper@abcdef02",
            kind="calls",
        ),
    )

    return db, db_path, audit_path


# ---------------------------------------------------------------------------
# Helper: validate result is a valid result or a valid error envelope
# ---------------------------------------------------------------------------


def _is_valid_result_or_error(result: Any) -> bool:
    """Return True if result is either a non-exception result or a typed error envelope.

    For dict results: accept any dict (valid result or error envelope).
    For list results: accept any list.
    Never accept None (tool must always return something).
    """
    return isinstance(result, (dict, list)) and result is not None


def _validate_error_envelope_if_error(result: Any) -> None:
    """If result looks like an error envelope, validate its structure."""
    if isinstance(result, dict) and "error" in result:
        err = result["error"]
        assert isinstance(err, dict), f"error must be a dict, got {type(err)}"
        assert "code" in err, f"error envelope missing 'code': {result}"
        assert "message" in err, f"error envelope missing 'message': {result}"
        assert "retryable" in err, f"error envelope missing 'retryable': {result}"
        assert isinstance(err["retryable"], bool), f"retryable must be bool: {result}"


# ---------------------------------------------------------------------------
# CP-10: symbol_lookup — valid input → result OR typed error, never exception
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=100, deadline=30_000)
@given(
    name_or_id=st.text(min_size=0, max_size=200),
    kind=st.one_of(
        st.none(),
        st.sampled_from(
            ["function", "class", "method", "interface", "route", "module", "var", "const"]
        ),
        st.text(min_size=1, max_size=50),  # invalid kinds too
    ),
)
def test_symbol_lookup_never_raises(name_or_id: str, kind: str | None) -> None:
    """CP-10: symbol_lookup never raises; always returns result or error envelope.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import symbol_lookup

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        # Must not raise any exception.
        result = symbol_lookup(name_or_id, kind)

    assert _is_valid_result_or_error(result), (
        f"symbol_lookup returned invalid type {type(result)}: {result!r}"
    )
    _validate_error_envelope_if_error(result)


# ---------------------------------------------------------------------------
# CP-10: semantic_search — valid input → result OR typed error, never exception
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=50, deadline=30_000)
@given(
    query=st.text(min_size=0, max_size=500),
    k=_k_int,
    mode=_opt_str,
)
def test_semantic_search_never_raises(query: str, k: int, mode: str | None) -> None:
    """CP-10: semantic_search never raises; always returns result or error envelope.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import semantic_search

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        # Must not raise any exception.
        result = semantic_search(query, k, mode)

    assert _is_valid_result_or_error(result), (
        f"semantic_search returned invalid type {type(result)}: {result!r}"
    )
    _validate_error_envelope_if_error(result)


# ---------------------------------------------------------------------------
# CP-10: dependency_trace — valid input → result OR typed error, never exception
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=100, deadline=30_000)
@given(
    symbol_id=st.text(min_size=0, max_size=200),
    direction=_direction_str,
    depth=_depth_int,
)
def test_dependency_trace_never_raises(symbol_id: str, direction: str, depth: int) -> None:
    """CP-10: dependency_trace never raises; always returns result or error envelope.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import dependency_trace

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        # Must not raise any exception.
        result = dependency_trace(symbol_id, direction, depth)

    assert _is_valid_result_or_error(result), (
        f"dependency_trace returned invalid type {type(result)}: {result!r}"
    )
    _validate_error_envelope_if_error(result)


@pytest.mark.pbt
@settings(max_examples=50, deadline=30_000)
@given(depth=st.integers(min_value=9, max_value=10_000))
def test_dependency_trace_depth_always_clamped(depth: int) -> None:
    """dependency_trace with depth > 8 is clamped, never an unhandled error.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import dependency_trace

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        result = dependency_trace("py:src/test.py:sample_func@abcdef01", "out", depth)

    # Must be a dict (valid result) with depth clamped to <= 8.
    assert isinstance(result, dict)
    if "error" not in result:
        assert result["depth"] <= 8, f"depth not clamped: {result['depth']}"


# ---------------------------------------------------------------------------
# CP-10: retrieve_context_capsule — valid input → result OR typed error, never exception
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=50, deadline=60_000)
@given(
    task=st.text(min_size=0, max_size=500),
    max_tokens=_tokens_int,
    include_runtime=st.booleans(),
)
def test_retrieve_context_capsule_never_raises(
    task: str, max_tokens: int, include_runtime: bool
) -> None:
    """CP-10: retrieve_context_capsule never raises; always returns result or error envelope.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import retrieve_context_capsule

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        # Must not raise any exception.
        result = retrieve_context_capsule(task, max_tokens, include_runtime)

    assert _is_valid_result_or_error(result), (
        f"retrieve_context_capsule returned invalid type {type(result)}: {result!r}"
    )
    _validate_error_envelope_if_error(result)


@pytest.mark.pbt
@settings(max_examples=30, deadline=60_000)
@given(max_tokens=st.integers(min_value=32_001, max_value=1_000_000))
def test_context_capsule_max_tokens_clamped(max_tokens: int) -> None:
    """max_tokens > 32000 is clamped; capsule token_estimate never exceeds 32000.

    **Validates: Requirements 22.1, 22.2**
    """
    _, db_path, audit_path = _make_test_db()

    from cognis_mcpd.tools import retrieve_context_capsule

    with patch.dict(os.environ, {"COGNIS_DB_PATH": db_path, "COGNIS_AUDIT_LOG": audit_path}):
        result = retrieve_context_capsule("explain the auth module", max_tokens)

    assert isinstance(result, dict)
    if "error" not in result:
        assert result.get("token_estimate", 0) <= 32_000
