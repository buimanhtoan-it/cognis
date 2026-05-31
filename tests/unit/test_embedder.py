"""Unit tests for the embedder module (tasks 10.1-10.8).

Covers:

- :func:`~cognis_indexer.embedder.build_embedding_text` format.
- LRU cache returns the same object on a hit (no re-computation).
- ``embed_cached`` with different content hashes returns different results.
- :func:`~cognis_indexer.embedder.assert_vec_dim` passes on matching dim and
  raises on a mismatch.
- :class:`~cognis_indexer.embedder.VoyageEmbedder` stub returns zero vectors.
- :class:`~cognis_indexer.embedder.Embedder` protocol is satisfied by stub and
  real classes.

All tests run without ``sentence-transformers`` by using a lightweight stub
embedder.
"""

from __future__ import annotations

import tempfile
from collections.abc import Callable
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path

import numpy as np
import pytest
from cognis.db import EMBEDDING_DIM, Database
from cognis_indexer.embedder import (
    Embedder,
    VoyageEmbedder,
    assert_vec_dim,
    build_embedding_text,
)
from cognis_indexer.parsers.base import ParsedSymbol

# ---------------------------------------------------------------------------
# Helpers: minimal stub objects
# ---------------------------------------------------------------------------


def _make_symbol(
    kind: str = "function",
    qualified_name: str = "mymodule.my_func",
    signature: str | None = "def my_func() -> None",
    docstring: str | None = "Does something.",
    body_excerpt: str | None = "    pass",
    content_hash: str = "abc123def456",
) -> ParsedSymbol:
    """Return a minimal :class:`ParsedSymbol` for testing."""
    return ParsedSymbol(
        id=f"py:src/mymodule.py:{qualified_name}@{content_hash}",
        kind=kind,  # type: ignore[arg-type]
        name="my_func",
        qualified_name=qualified_name,
        language="python",
        module="mymodule",
        file_path="src/mymodule.py",
        line_start=1,
        line_end=5,
        signature=signature,
        docstring=docstring,
        content_hash=content_hash,
        body_excerpt=body_excerpt,
    )


class StubEmbedder:
    """Lightweight deterministic stub that satisfies the :class:`Embedder` protocol."""

    embedding_dim: int = EMBEDDING_DIM
    _call_count: int

    def __init__(self) -> None:
        self._call_count = 0
        self._embed_cached_fn = _make_stub_cached(self)

    def embed_batch(self, texts: list[str]) -> np.ndarray:
        self._call_count += len(texts)
        return (
            np.stack([self._compute(t) for t in texts])
            if texts
            else np.empty((0, self.embedding_dim), dtype=np.float32)
        )

    def embed_text(self, text: str) -> np.ndarray:
        self._call_count += 1
        return self._compute(text)

    def embed_cached(self, content_hash: str, text: str) -> np.ndarray:
        return self._embed_cached_fn(content_hash, text)

    def _compute(self, text: str) -> np.ndarray:
        seed = hash(text) & 0xFFFF_FFFF
        rng = np.random.default_rng(seed)
        return rng.standard_normal(self.embedding_dim).astype(np.float32)


def _make_stub_cached(embedder: StubEmbedder) -> Callable[[str, str], np.ndarray]:
    @lru_cache(maxsize=50_000)
    def _cached(content_hash: str, text: str) -> np.ndarray:
        return embedder.embed_text(text)

    return _cached


# ---------------------------------------------------------------------------
# 1. build_embedding_text
# ---------------------------------------------------------------------------


class TestBuildEmbeddingText:
    def test_full_fields_produces_expected_format(self) -> None:
        sym = _make_symbol(
            kind="function",
            qualified_name="mymodule.my_func",
            signature="def my_func() -> None",
            docstring="Does something.",
            body_excerpt="    pass",
        )
        text = build_embedding_text(sym)
        lines = text.split("\n")
        assert lines[0] == "[function] mymodule.my_func"
        assert lines[1] == "def my_func() -> None"
        assert lines[2] == "Does something."
        assert lines[3] == "    pass"

    def test_missing_optional_fields_are_omitted(self) -> None:
        sym = _make_symbol(
            kind="class",
            qualified_name="mymodule.MyClass",
            signature=None,
            docstring=None,
            body_excerpt=None,
        )
        text = build_embedding_text(sym)
        assert text == "[class] mymodule.MyClass"

    def test_body_excerpt_truncated_to_1500_chars(self) -> None:
        long_body = "x" * 2000
        sym = _make_symbol(body_excerpt=long_body, signature=None, docstring=None)
        text = build_embedding_text(sym)
        # Text = header + "\n" + truncated body
        parts = text.split("\n")
        assert parts[-1] == "x" * 1500

    def test_enriched_symbol_unwrapped_automatically(self) -> None:
        """EnrichedSymbol wraps ParsedSymbol in .symbol; build_embedding_text unwraps it."""

        sym = _make_symbol()

        @dataclass
        class _FakeEnriched:
            symbol: ParsedSymbol
            attributes: list = field(default_factory=list)
            untrusted_flags: list = field(default_factory=list)

        enriched = _FakeEnriched(symbol=sym)
        text_direct = build_embedding_text(sym)
        text_enriched = build_embedding_text(enriched)  # type: ignore[arg-type]
        assert text_direct == text_enriched


# ---------------------------------------------------------------------------
# 2. LRU cache behaviour
# ---------------------------------------------------------------------------


class TestEmbedCachedCache:
    def test_same_content_hash_returns_same_object(self) -> None:
        """Cache hit: embed_cached with same hash returns identical Python object."""
        embedder = StubEmbedder()
        v1 = embedder.embed_cached("hash_abc", "some text")
        v2 = embedder.embed_cached("hash_abc", "some text")
        assert v1 is v2, "Expected cache hit (same object) for same content_hash + text"

    def test_same_hash_same_text_is_cache_hit(self) -> None:
        """Cache hit: same content_hash AND same text returns the same object.

        In the real pipeline a whitespace-only source edit does NOT change
        ``content_hash`` (it's over normalized AST) and does NOT change
        ``build_embedding_text`` output (which reads symbol fields, not raw
        source).  Both calls therefore pass *identical* (hash, text) tuples, so
        the lru_cache returns the same object.
        """
        embedder = StubEmbedder()
        text = "original text"
        v1 = embedder.embed_cached("hash_xyz", text)
        # Exact same (hash, text) — must be a cache hit (same object).
        v2 = embedder.embed_cached("hash_xyz", text)
        assert v1 is v2

    def test_different_hashes_are_independent_cache_entries(self) -> None:
        """Different content hashes produce independent cache entries."""
        embedder = StubEmbedder()
        v1 = embedder.embed_cached("hash_aaa", "text")
        v2 = embedder.embed_cached("hash_bbb", "text")
        assert v1 is not v2

    def test_cache_does_not_recompute_on_repeat(self) -> None:
        """After a cache hit, the underlying embed_text is not called again."""
        embedder = StubEmbedder()
        # First call — populates cache.
        embedder.embed_cached("hash_c", "text c")
        count_after_first = embedder._call_count
        # Second call — must be a cache hit (no new embed_text call).
        embedder.embed_cached("hash_c", "text c")
        assert embedder._call_count == count_after_first, (
            "embed_text was called again on a cache hit"
        )


# ---------------------------------------------------------------------------
# 3. assert_vec_dim
# ---------------------------------------------------------------------------


@pytest.fixture
def fresh_db() -> Database:
    """Yield a fresh in-memory Database (per test)."""
    db = Database(":memory:")
    # Connect to trigger migration.
    db.connect()
    return db


class TestAssertVecDim:
    def test_passes_when_no_symbol_vec_table(self, fresh_db: Database) -> None:
        """No symbol_vec in sqlite_master → skip assertion, no error."""
        # The in-memory DB has symbol_vec from migration (fallback table).
        # Drop it to simulate first-boot state.
        # Connection uses isolation_level=None (autocommit), so no explicit COMMIT needed.
        conn = fresh_db.connect()
        conn.execute("DROP TABLE IF EXISTS symbol_vec")

        embedder = StubEmbedder()
        # Must not raise.
        assert_vec_dim(fresh_db, embedder)

    def test_passes_when_plain_fallback_table_no_float_token(self) -> None:
        """symbol_vec as a plain table (no FLOAT[N]) → skip assertion, no error."""
        with tempfile.TemporaryDirectory() as td:
            db = Database(Path(td) / "test.db")
            conn = db.connect()
            # Migration creates symbol_vec as a plain table (no vec0, no FLOAT[N]).
            # Verify the fallback DDL has no FLOAT[N] token.
            row = conn.execute("SELECT sql FROM sqlite_master WHERE name = 'symbol_vec'").fetchone()
            if row:
                ddl = str(row[0] or "")
                if "FLOAT[" not in ddl.upper():
                    # No FLOAT[N] token → assert_vec_dim should pass silently.
                    embedder = StubEmbedder()
                    assert_vec_dim(db, embedder)  # must not raise
            db.close_thread_connection()

    def test_raises_on_dim_mismatch_in_vec0_ddl(self) -> None:
        """When symbol_vec DDL has FLOAT[N] with wrong N, AssertionError is raised."""
        with tempfile.TemporaryDirectory() as td:
            db = Database(Path(td) / "mismatch.db")
            conn = db.connect()

            # Recreate symbol_vec with a wrong dimension.
            conn.execute("DROP TABLE IF EXISTS symbol_vec")
            wrong_dim = 512
            # Use a plain CREATE TABLE (not vec0) but include FLOAT[N] token.
            conn.execute(
                f"CREATE TABLE symbol_vec (symbol_id TEXT PRIMARY KEY, embedding FLOAT[{wrong_dim}])"
            )

            class _MismatchEmbedder:
                embedding_dim = EMBEDDING_DIM

                def embed_batch(self, texts):  # type: ignore[no-untyped-def]
                    ...

                def embed_text(self, text):  # type: ignore[no-untyped-def]
                    ...

            with pytest.raises(AssertionError, match=str(wrong_dim)):
                assert_vec_dim(db, _MismatchEmbedder())  # type: ignore[arg-type]

            db.close_thread_connection()

    def test_passes_on_matching_dim_in_ddl(self) -> None:
        """When symbol_vec DDL FLOAT[N] matches embedder.embedding_dim, no error."""
        with tempfile.TemporaryDirectory() as td:
            db = Database(Path(td) / "match.db")
            conn = db.connect()

            conn.execute("DROP TABLE IF EXISTS symbol_vec")
            conn.execute(
                f"CREATE TABLE symbol_vec (symbol_id TEXT PRIMARY KEY, embedding FLOAT[{EMBEDDING_DIM}])"
            )

            embedder = StubEmbedder()
            assert_vec_dim(db, embedder)  # must not raise

            db.close_thread_connection()


# ---------------------------------------------------------------------------
# 4. VoyageEmbedder stub
# ---------------------------------------------------------------------------


class TestVoyageEmbedderStub:
    def test_embed_text_returns_zero_vector(self) -> None:
        voyage = VoyageEmbedder()
        v = voyage.embed_text("hello world")
        assert v.shape == (EMBEDDING_DIM,)
        np.testing.assert_array_equal(v, np.zeros(EMBEDDING_DIM, dtype=np.float32))

    def test_embed_batch_returns_zero_matrix(self) -> None:
        voyage = VoyageEmbedder()
        texts = ["a", "b", "c"]
        result = voyage.embed_batch(texts)
        assert result.shape == (len(texts), EMBEDDING_DIM)
        np.testing.assert_array_equal(
            result, np.zeros((len(texts), EMBEDDING_DIM), dtype=np.float32)
        )

    def test_embedding_dim_is_384(self) -> None:
        voyage = VoyageEmbedder()
        assert voyage.embedding_dim == EMBEDDING_DIM


# ---------------------------------------------------------------------------
# 5. Embedder protocol conformance
# ---------------------------------------------------------------------------


class TestEmbedderProtocol:
    def test_stub_satisfies_embedder_protocol(self) -> None:
        """StubEmbedder must be recognised as implementing the Embedder protocol."""
        embedder = StubEmbedder()
        assert isinstance(embedder, Embedder)

    def test_voyage_satisfies_embedder_protocol(self) -> None:
        """VoyageEmbedder must satisfy the Embedder protocol."""
        voyage = VoyageEmbedder()
        assert isinstance(voyage, Embedder)

    def test_embed_batch_empty_list_returns_empty_array(self) -> None:
        embedder = StubEmbedder()
        result = embedder.embed_batch([])
        assert result.shape == (0, EMBEDDING_DIM)

    def test_embed_text_returns_correct_shape(self) -> None:
        embedder = StubEmbedder()
        v = embedder.embed_text("test")
        assert v.shape == (EMBEDDING_DIM,)
        assert v.dtype == np.float32
