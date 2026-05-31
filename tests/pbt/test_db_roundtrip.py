"""Property test: SymbolNode/Edge round-trip and CP-3 deletion cascade.

**Validates: Requirements REQ-IDX-1, REQ-IDX-2, NFR Reliability** via
correctness properties **CP-3** (insert-then-query round trip preserves all
fields; deletion cascades clear inbound edges as designed) from
``.kiro/specs/cognis/design.md``.

Two properties are encoded:

1. ``test_symbol_roundtrip_preserves_all_fields`` — for any valid
   :class:`SymbolNode`, ``upsert_symbol`` then ``get_symbol`` returns an equal
   value (every field, including JSON-encoded ``untrusted_flags``).

2. ``test_delete_symbol_cascade_invariants`` — for a random graph of symbols
   and edges, deleting one symbol *x* leaves the DB in this state:
     - symbol *x* is absent.
     - every outbound edge ``(x, *, *)`` is absent.
     - every inbound edge ``(*, x, *)`` is *kept* but flagged
       ``meta.dst_missing = true`` (design *Property 3*).
     - all *other* symbols and edges are unchanged.

The Hypothesis profile is pinned in ``tests/conftest.py`` so this file is
deterministic in CI. Each example builds its own :class:`Database` in a
freshly-allocated temp directory so cumulative state does not leak between
hypothesis iterations.
"""

from __future__ import annotations

import string
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import cast, get_args

import pytest
from cognis.db import (
    Database,
    delete_symbol,
    get_inbound_edges,
    get_outbound_edges,
    get_symbol,
    list_edges,
    list_symbols,
    upsert_edges,
    upsert_symbol,
    upsert_symbols,
)
from cognis.models import Edge, EdgeKind, SymbolKind, SymbolNode
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st


@contextmanager
def _fresh_db() -> Iterator[Database]:
    """Yield a brand-new :class:`Database` whose temp dir is reaped on exit.

    Each Hypothesis example needs an isolated DB; reusing ``tmp_path`` across
    iterations would leak state from one example into the next (a deleted
    symbol stays deleted, prior edges still match queries, etc.). Using a
    private :class:`tempfile.TemporaryDirectory` per call keeps the property
    self-contained and platform-portable.
    """
    with tempfile.TemporaryDirectory() as td:
        db = Database(Path(td) / "uckg.db")
        try:
            yield db
        finally:
            db.close_thread_connection()


# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

# Identifiers/qualified-names use a restricted alphabet so we exercise the
# round-trip rather than SQLite's tokenizer or the JSON escape paths. The
# property is "every field survives", not "every Unicode codepoint round-trips
# through SQLite's Row mapping" — which is a cross-cutting concern handled by
# the DB driver itself.
_NAME_ALPHABET = string.ascii_letters + string.digits + "_"
_NAME = st.text(alphabet=_NAME_ALPHABET, min_size=1, max_size=20)
_QNAME = st.text(alphabet=_NAME_ALPHABET + ".", min_size=1, max_size=40).filter(
    lambda s: s and not s.startswith(".") and not s.endswith(".") and ".." not in s
)
_PATH = st.text(alphabet=_NAME_ALPHABET + "/", min_size=1, max_size=40).filter(
    lambda s: s and not s.startswith("/") and not s.endswith("/") and "//" not in s
)

# Optional free-form text with printable ASCII keeps generators producing
# rows that *could* trip JSON-encoding bugs (quotes, backslashes, brackets)
# while excluding control chars that aren't part of this property's contract.
_OPT_TEXT = st.one_of(
    st.none(),
    st.text(
        alphabet=st.characters(min_codepoint=0x20, max_codepoint=0x7E),
        max_size=80,
    ),
)

_FLAG = st.sampled_from(["secret_redacted", "untrusted_doc", "ambiguous_call", "low_confidence"])
_FLAGS = st.lists(_FLAG, max_size=4, unique=True)

_KIND: st.SearchStrategy[SymbolKind] = cast(
    "st.SearchStrategy[SymbolKind]", st.sampled_from(get_args(SymbolKind))
)
_EDGE_KIND: st.SearchStrategy[EdgeKind] = cast(
    "st.SearchStrategy[EdgeKind]", st.sampled_from(get_args(EdgeKind))
)


@st.composite
def _symbol(draw: st.DrawFn, *, sid: str | None = None) -> SymbolNode:
    """Generate a valid :class:`SymbolNode`. Optionally pin its id."""
    line_start = draw(st.integers(min_value=1, max_value=10_000))
    line_end = draw(st.integers(min_value=line_start, max_value=line_start + 5_000))
    name = draw(_NAME)
    qname = draw(_QNAME)
    path = draw(_PATH) + ".py"
    chash = draw(st.text(alphabet=string.hexdigits.lower(), min_size=8, max_size=16))

    return SymbolNode(
        id=sid if sid is not None else f"py:{path}:{qname}@{chash}",
        kind=draw(_KIND),
        name=name,
        qualified_name=qname,
        language=draw(st.sampled_from(["python", "typescript", "go"])),
        module=draw(_QNAME),
        file_path=path,
        line_start=line_start,
        line_end=line_end,
        signature=draw(_OPT_TEXT),
        docstring=draw(_OPT_TEXT),
        content_hash=chash,
        body_excerpt=draw(_OPT_TEXT),
        semantic_summary=draw(_OPT_TEXT),
        risk_score=draw(st.floats(min_value=0.0, max_value=1.0, allow_nan=False)),
        ambiguous=draw(st.booleans()),
        untrusted_flags=draw(_FLAGS),
        updated_at=draw(st.integers(min_value=0, max_value=2_000_000_000)),
    )


# ---------------------------------------------------------------------------
# Property 1 — round-trip preserves all fields
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=50, deadline=None)
@given(sym=_symbol())
def test_symbol_roundtrip_preserves_all_fields(sym: SymbolNode) -> None:
    """**Validates: Requirements REQ-IDX-1** (CP-3).

    Insert any valid :class:`SymbolNode`; the round-tripped value equals the
    original on every field.
    """
    with _fresh_db() as db:
        upsert_symbol(db, sym)
        fetched = get_symbol(db, sym.id)
        assert fetched is not None
        assert fetched == sym


# ---------------------------------------------------------------------------
# Property 2 — deletion cascade invariants
# ---------------------------------------------------------------------------


@st.composite
def _symbol_set(draw: st.DrawFn) -> list[SymbolNode]:
    """Generate 2-6 distinct symbols with unique ids."""
    count = draw(st.integers(min_value=2, max_value=6))
    symbols: list[SymbolNode] = []
    used_ids: set[str] = set()
    while len(symbols) < count:
        sym = draw(_symbol())
        if sym.id in used_ids:
            continue
        used_ids.add(sym.id)
        symbols.append(sym)
    return symbols


@st.composite
def _edges_for(draw: st.DrawFn, symbols: list[SymbolNode]) -> list[Edge]:
    """Generate a deduplicated list of edges between *symbols*."""
    pairs = st.tuples(st.sampled_from(symbols), st.sampled_from(symbols), _EDGE_KIND)
    raw = draw(st.lists(pairs, min_size=0, max_size=8))
    seen: set[tuple[str, str, EdgeKind]] = set()
    out: list[Edge] = []
    for src, dst, kind in raw:
        key = (src.id, dst.id, kind)
        if key in seen:
            continue
        seen.add(key)
        out.append(
            Edge(
                src_id=src.id,
                dst_id=dst.id,
                kind=kind,
                confidence=draw(st.floats(min_value=0.0, max_value=1.0, allow_nan=False)),
                meta={},
            )
        )
    return out


@pytest.mark.pbt
@settings(
    max_examples=30,
    deadline=None,
    suppress_health_check=[HealthCheck.large_base_example],
)
@given(data=st.data())
def test_delete_symbol_cascade_invariants(data: st.DataObject) -> None:
    """**Validates: Requirements REQ-IDX-2** (CP-3 deletion cascade).

    For a random symbol+edge graph, deleting one symbol enforces:

    - the deleted symbol is gone.
    - all outbound edges from it are gone.
    - all inbound edges to it are kept but flagged ``meta.dst_missing=true``.
    - other symbols/edges are untouched (modulo the flag bump).
    """
    symbols = data.draw(_symbol_set())
    edges = data.draw(_edges_for(symbols))
    target = data.draw(st.sampled_from(symbols))

    with _fresh_db() as db:
        upsert_symbols(db, symbols)
        upsert_edges(db, edges)

        # Pre-deletion bookkeeping for the "everything else unchanged" check.
        survivors = [s for s in symbols if s.id != target.id]
        edges_before = {(e.src_id, e.dst_id, e.kind): e for e in edges}

        deleted = delete_symbol(db, target.id)
        assert deleted is True

        # 1. Target symbol is gone.
        assert get_symbol(db, target.id) is None

        # 2. Every outbound edge from target is gone.
        assert get_outbound_edges(db, target.id) == []

        # 3. Every inbound edge to target is kept and flagged dst_missing=true.
        inbound = get_inbound_edges(db, target.id)
        expected_inbound = {
            key: e
            for key, e in edges_before.items()
            if e.dst_id == target.id and e.src_id != target.id
        }
        assert {(e.src_id, e.dst_id, e.kind) for e in inbound} == set(expected_inbound)
        for e in inbound:
            assert e.meta.get("dst_missing") is True

        # 4. Survivor symbols are untouched.
        for s in survivors:
            assert get_symbol(db, s.id) == s

        # 5. Edges that don't touch the target are unchanged.
        untouched_keys = {
            key
            for key, e in edges_before.items()
            if e.src_id != target.id and e.dst_id != target.id
        }
        actual_edges = {(e.src_id, e.dst_id, e.kind): e for e in list_edges(db)}
        for key in untouched_keys:
            assert key in actual_edges
            before = edges_before[key]
            after = actual_edges[key]
            # ``meta`` may have been left untouched (we only patch when dst matches).
            assert after.src_id == before.src_id
            assert after.dst_id == before.dst_id
            assert after.kind == before.kind
            assert after.confidence == pytest.approx(before.confidence)
            assert after.meta == before.meta

        # 6. Sanity: list_symbols never returns the deleted id.
        surviving_ids = {s.id for s in list_symbols(db)}
        assert target.id not in surviving_ids
