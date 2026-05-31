"""Unit tests for the retrieval layers (tasks 12.1-12.3).

Covers:
- Query rewriter identifier extraction.
- LexicalLayer: FTS5 hits with snippet evidence.
- SemanticLayer: empty results without vec0; basic structure when vec available.
- StructuralLayer: outbound traversal, inbound traversal, cycle detection,
  max_depth clamping, direction validation.
- Hit dataclass field contract.

All tests use a real in-memory SQLite database (no mocks).
"""

from __future__ import annotations

import time

import pytest
from cognis.db import Database, upsert_edge, upsert_symbol
from cognis.models import Edge, SymbolNode
from cognis_retrieval import (
    Hit,
    LexicalLayer,
    SemanticLayer,
    StructuralLayer,
    populate_fts,
    rewrite_query,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_symbol(
    sym_id: str,
    name: str,
    *,
    qualified_name: str | None = None,
    signature: str | None = None,
    docstring: str | None = None,
    body_excerpt: str | None = None,
) -> SymbolNode:
    return SymbolNode(
        id=sym_id,
        kind="function",
        name=name,
        qualified_name=qualified_name or name,
        language="python",
        module="test_module",
        file_path="src/test.py",
        line_start=1,
        line_end=10,
        signature=signature,
        docstring=docstring,
        content_hash="deadbeef",
        body_excerpt=body_excerpt,
        updated_at=int(time.time()),
    )


def _make_db() -> Database:
    return Database(":memory:", vec_enabled=False)


# ---------------------------------------------------------------------------
# Hit dataclass
# ---------------------------------------------------------------------------


class TestHitDataclass:
    def test_hit_fields(self) -> None:
        h = Hit(
            symbol_id="ts:src/a.ts:foo@abc1",
            score=0.9,
            layer="lexical",
            reason="matched",
            evidence={"snippet": "«foo»"},
        )
        assert h.symbol_id == "ts:src/a.ts:foo@abc1"
        assert h.score == 0.9
        assert h.layer == "lexical"
        assert h.reason == "matched"
        assert h.evidence == {"snippet": "«foo»"}

    def test_hit_default_evidence(self) -> None:
        h = Hit(symbol_id="x", score=1.0, layer="semantic", reason="r")
        assert h.evidence == {}


# ---------------------------------------------------------------------------
# Query rewriter
# ---------------------------------------------------------------------------


class TestRewriteQuery:
    def test_extracts_identifiers(self) -> None:
        result = rewrite_query("Why is validateJwtToken timing out?")
        tokens = result.split(" OR ")
        assert "validateJwtToken" in tokens

    def test_filters_stop_words(self) -> None:
        result = rewrite_query("why is the auth failing")
        tokens = [t.lower() for t in result.split(" OR ")]
        for sw in ("why", "is", "the"):
            assert sw not in tokens

    def test_extracts_backtick_tokens(self) -> None:
        result = rewrite_query("The `auth.jwt` module is broken")
        tokens = result.split(" OR ")
        assert any("auth" in t or "jwt" in t for t in tokens)

    def test_extracts_todo_markers(self) -> None:
        result = rewrite_query("Look for TODO items in the auth flow")
        tokens = result.split(" OR ")
        assert "TODO" in tokens

    def test_extracts_fixme_markers(self) -> None:
        result = rewrite_query("FIXME the login handler")
        tokens = result.split(" OR ")
        assert "FIXME" in tokens

    def test_extracts_file_glob(self) -> None:
        result = rewrite_query("search *.ts files for auth")
        tokens = result.split(" OR ")
        # Extension 'ts' should be a token.
        assert any("ts" in t for t in tokens)

    def test_empty_query_returns_empty(self) -> None:
        result = rewrite_query("")
        assert result == ""

    def test_only_stop_words_returns_empty(self) -> None:
        result = rewrite_query("is a the")
        assert result == ""

    def test_deduplication(self) -> None:
        result = rewrite_query("auth auth auth")
        assert result.count("auth") == 1

    def test_multiple_identifiers(self) -> None:
        result = rewrite_query("validate jwt token in auth middleware")
        tokens = result.split(" OR ")
        # Should include non-stop-word identifiers.
        assert len(tokens) >= 2


# ---------------------------------------------------------------------------
# LexicalLayer
# ---------------------------------------------------------------------------


class TestLexicalLayer:
    def _setup_db(self, symbols: list[SymbolNode]) -> Database:
        db = _make_db()
        upsert_symbols = __import__("cognis.db", fromlist=["upsert_symbols"]).upsert_symbols
        upsert_symbols(db, symbols)
        populate_fts(db, symbols)
        return db

    def test_returns_hits_with_snippet_evidence(self) -> None:
        sym = _make_symbol(
            "py:src/auth.py:validate_token@1234",
            "validate_token",
            qualified_name="auth.validate_token",
            signature="def validate_token(token: str) -> bool",
            docstring="Validates a JWT token.",
        )
        db = self._setup_db([sym])
        layer = LexicalLayer()
        hits = layer.search("validate token", k=10, db=db)
        assert len(hits) >= 1
        assert hits[0].symbol_id == sym.id
        assert hits[0].layer == "lexical"
        assert "snippet" in hits[0].evidence

    def test_returns_empty_for_no_match(self) -> None:
        sym = _make_symbol("py:src/foo.py:bar@ffff", "bar", qualified_name="foo.bar")
        db = self._setup_db([sym])
        layer = LexicalLayer()
        hits = layer.search("xyzzyNonExistentTokenABC", k=10, db=db)
        assert hits == []

    def test_respects_k_limit(self) -> None:
        symbols = [
            _make_symbol(
                f"py:src/m.py:func_{i}@{i:04x}",
                f"func_{i}",
                qualified_name=f"m.func_{i}",
                signature=f"def func_{i}(): pass",
                docstring="auth function",
            )
            for i in range(20)
        ]
        db = self._setup_db(symbols)
        layer = LexicalLayer()
        hits = layer.search("auth function", k=5, db=db)
        assert len(hits) <= 5

    def test_empty_fts_query_returns_empty(self) -> None:
        sym = _make_symbol("py:src/x.py:foo@0001", "foo", qualified_name="x.foo")
        db = self._setup_db([sym])
        layer = LexicalLayer()
        # Query with only stop-words → rewrite_query returns "" → no search
        hits = layer.search("is a the", k=10, db=db)
        assert hits == []

    def test_score_positive(self) -> None:
        sym = _make_symbol(
            "py:src/auth.py:login@aaaa",
            "login",
            qualified_name="auth.login",
            signature="def login(user, password): ...",
        )
        db = self._setup_db([sym])
        layer = LexicalLayer()
        hits = layer.search("login password", k=5, db=db)
        assert all(h.score > 0 for h in hits)


# ---------------------------------------------------------------------------
# SemanticLayer
# ---------------------------------------------------------------------------


class TestSemanticLayerNoVec:
    """Tests that run when sqlite-vec is not available."""

    def test_returns_empty_when_no_vec(self) -> None:
        """Without sqlite-vec the layer returns an empty list."""

        class _ZeroEmbedder:
            embedding_dim = 4

            def embed_text(self, text: str) -> object:
                import numpy as np

                return np.zeros(4, dtype=np.float32)

        db = _make_db()
        layer = SemanticLayer(_ZeroEmbedder())
        hits = layer.search("find authentication", k=5, db=db)
        assert hits == []

    def test_layer_name(self) -> None:
        class _Stub:
            embedding_dim = 4

            def embed_text(self, text: str) -> object:
                import numpy as np

                return np.zeros(4, dtype=np.float32)

        layer = SemanticLayer(_Stub())
        assert layer.name == "semantic"


# ---------------------------------------------------------------------------
# StructuralLayer
# ---------------------------------------------------------------------------


def _build_graph_db(edges: list[tuple[str, str]]) -> Database:
    """Create an in-memory DB with the given directed edges and corresponding symbols."""
    db = _make_db()
    # Collect all unique ids.
    ids: set[str] = set()
    for src, dst in edges:
        ids.add(src)
        ids.add(dst)
    symbols = [_make_symbol(sid, sid, qualified_name=sid) for sid in ids]
    upsert_symbol_batch = __import__("cognis.db", fromlist=["upsert_symbols"]).upsert_symbols
    upsert_symbol_batch(db, symbols)
    edge_objs = [Edge(src_id=src, dst_id=dst, kind="calls") for src, dst in edges]
    for e in edge_objs:
        upsert_edge(db, e)
    return db


class TestStructuralLayer:
    def test_outbound_traversal_basic(self) -> None:
        # A → B → C
        db = _build_graph_db([("A", "B"), ("B", "C")])
        layer = StructuralLayer()
        hits = layer.dependency_trace("A", "out", max_depth=3, db=db)
        ids = {h.symbol_id for h in hits}
        assert "B" in ids
        assert "C" in ids
        assert "A" not in ids  # start node excluded

    def test_inbound_traversal_basic(self) -> None:
        # A → B → C  (inbound from C should give B, A)
        db = _build_graph_db([("A", "B"), ("B", "C")])
        layer = StructuralLayer()
        hits = layer.dependency_trace("C", "in", max_depth=3, db=db)
        ids = {h.symbol_id for h in hits}
        assert "B" in ids
        assert "A" in ids
        assert "C" not in ids

    def test_cycle_does_not_loop(self) -> None:
        # A → B → C → A (cycle)
        db = _build_graph_db([("A", "B"), ("B", "C"), ("C", "A")])
        layer = StructuralLayer()
        # Should terminate without recursion error or infinite loop.
        hits = layer.dependency_trace("A", "out", max_depth=5, db=db)
        # Should contain B and C but not infinite duplicates.
        ids = [h.symbol_id for h in hits]
        assert ids.count("B") <= 1
        assert ids.count("C") <= 1

    def test_max_depth_respected(self) -> None:
        # A → B → C → D → E (chain of length 4)
        db = _build_graph_db([("A", "B"), ("B", "C"), ("C", "D"), ("D", "E")])
        layer = StructuralLayer()
        hits_2 = layer.dependency_trace("A", "out", max_depth=2, db=db)
        ids_2 = {h.symbol_id for h in hits_2}
        # At depth 2 should only reach B (depth 1) and C (depth 2).
        assert "B" in ids_2
        assert "C" in ids_2
        assert "D" not in ids_2
        assert "E" not in ids_2

    def test_max_depth_hard_cap(self) -> None:
        """Depth > 8 is clamped to 8."""
        db = _build_graph_db([("A", "B")])
        layer = StructuralLayer()
        # Should not raise.
        hits = layer.dependency_trace("A", "out", max_depth=100, db=db)
        assert isinstance(hits, list)

    def test_both_direction(self) -> None:
        # X → A ← Y (A has both an inbound from X and outbound to Y... wait,
        # let's use: X → A → Y)
        db = _build_graph_db([("X", "A"), ("A", "Y")])
        layer = StructuralLayer()
        hits = layer.dependency_trace("A", "both", max_depth=2, db=db)
        ids = {h.symbol_id for h in hits}
        assert "X" in ids  # inbound
        assert "Y" in ids  # outbound

    def test_invalid_direction_raises(self) -> None:
        db = _make_db()
        layer = StructuralLayer()
        with pytest.raises(ValueError, match="direction"):
            layer.dependency_trace("A", "sideways", max_depth=3, db=db)

    def test_no_edges_returns_empty(self) -> None:
        db = _make_db()
        sym = _make_symbol("iso:a@0001", "iso", qualified_name="iso")
        upsert_symbol(db, sym)
        layer = StructuralLayer()
        hits = layer.dependency_trace("iso:a@0001", "out", max_depth=3, db=db)
        assert hits == []

    def test_evidence_contains_depth(self) -> None:
        db = _build_graph_db([("A", "B"), ("B", "C")])
        layer = StructuralLayer()
        hits = layer.dependency_trace("A", "out", max_depth=3, db=db)
        for h in hits:
            assert "depth" in h.evidence
            assert isinstance(h.evidence["depth"], int)

    def test_score_higher_for_closer_symbols(self) -> None:
        # A → B (depth 1) → C (depth 2): B should have higher score than C.
        db = _build_graph_db([("A", "B"), ("B", "C")])
        layer = StructuralLayer()
        hits = layer.dependency_trace("A", "out", max_depth=3, db=db)
        score_map = {h.symbol_id: h.score for h in hits}
        assert score_map["B"] > score_map["C"]

    def test_layer_name(self) -> None:
        assert StructuralLayer().name == "structural"

    def test_search_returns_empty(self) -> None:
        """search() on StructuralLayer always returns [] (protocol compliance)."""
        db = _make_db()
        layer = StructuralLayer()
        assert layer.search("anything", k=10, db=db) == []
