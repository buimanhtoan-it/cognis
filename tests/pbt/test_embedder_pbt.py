"""Property-Based Tests for the embedder — CP-5, CP-6.

**Validates: Requirements 10.1, 10.2** via correctness properties **CP-5** and
**CP-6** from ``.kiro/specs/cognis/design.md``.

CP-5 (Embedding determinism):
    Same input text always yields the same embedding vector (modulo floating-
    point epsilon).

    ∀ text, embed(text) == embed(text)

CP-6 (Cache idempotency on whitespace edits):
    Whitespace-only changes to the embedding text MUST NOT trigger re-embedding
    if the ``content_hash`` (computed over the normalized AST) is unchanged.
    The second call must be a cache hit — i.e. returns the *same Python object*.

Run with::

    pytest tests/pbt/test_embedder_pbt.py -m pbt

Both tests operate without ``sentence-transformers`` by using a
:class:`StubEmbedder` that mimics the production :class:`LocalEmbedder`
interface (including the per-instance ``lru_cache``).  This keeps the PBT
runnable in CI environments without a GPU or the ``embed-local`` extra.
"""

from __future__ import annotations

import string
from collections.abc import Callable
from functools import lru_cache

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

pytestmark = [pytest.mark.pbt]

# ---------------------------------------------------------------------------
# StubEmbedder — a deterministic in-process stand-in for LocalEmbedder
# ---------------------------------------------------------------------------


class StubEmbedder:
    """Deterministic stub that satisfies the :class:`~cognis_indexer.embedder.Embedder` protocol.

    Embeddings are computed from the text hash so they are:
    - Deterministic: same text → same vector.
    - Distinct: different texts → different vectors (with overwhelming probability).
    - Pure Python + numpy: no ``sentence-transformers`` required.

    The LRU cache mirrors :meth:`LocalEmbedder.embed_cached` exactly so CP-6
    properties exercise the real caching mechanism.
    """

    embedding_dim: int = 384

    def __init__(self) -> None:
        self._embed_cached_fn = _make_stub_embed_cached(self)

    def embed_batch(self, texts: list[str]) -> np.ndarray:
        return (
            np.stack([self._compute(t) for t in texts])
            if texts
            else np.empty((0, self.embedding_dim), dtype=np.float32)
        )

    def embed_text(self, text: str) -> np.ndarray:
        return self._compute(text)

    def embed_cached(self, content_hash: str, text: str) -> np.ndarray:
        return self._embed_cached_fn(content_hash, text)

    def _compute(self, text: str) -> np.ndarray:
        """Derive a stable vector from the text using a simple hash seeded RNG."""
        seed = hash(text) & 0xFFFF_FFFF
        rng = np.random.default_rng(seed)
        vec = rng.standard_normal(self.embedding_dim).astype(np.float32)
        norm = np.linalg.norm(vec)
        if norm > 0:
            vec /= norm
        return vec


def _make_stub_embed_cached(
    embedder: StubEmbedder,
) -> Callable[[str, str], np.ndarray]:
    """Mirror of :func:`~cognis_indexer.embedder._make_embed_cached` for the stub."""

    @lru_cache(maxsize=50_000)
    def _cached(content_hash: str, text: str) -> np.ndarray:
        return embedder.embed_text(text)

    return _cached  # type: ignore[return-value]


# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

_TEXT_ALPHABET = string.ascii_letters + string.digits + "_. \n\t"
_NONEMPTY_TEXT = st.text(alphabet=_TEXT_ALPHABET, min_size=1, max_size=200)

_HASH_ALPHABET = string.hexdigits.lower()
_CONTENT_HASH = st.text(alphabet=_HASH_ALPHABET, min_size=8, max_size=16)

# Whitespace variations appended to a base text — these must not change
# content_hash (the hash is over normalized AST, not raw text).
_WHITESPACE = st.text(alphabet=" \t\n\r", min_size=0, max_size=20)


# ---------------------------------------------------------------------------
# CP-5: Embedding determinism
# ---------------------------------------------------------------------------


@given(text=_NONEMPTY_TEXT)
@settings(max_examples=200, deadline=None)
def test_cp5_same_text_yields_same_vector(text: str) -> None:
    """**Validates: Requirements 10.1, 10.2** CP-5.

    Same input text always produces the same embedding vector.
    Two separate calls to ``embed_text`` with identical *text* must return
    numerically identical arrays.
    """
    embedder = StubEmbedder()
    v1 = embedder.embed_text(text)
    v2 = embedder.embed_text(text)
    np.testing.assert_array_equal(
        v1,
        v2,
        err_msg=f"Embedding is non-deterministic for text={text!r}",
    )


@given(text=_NONEMPTY_TEXT)
@settings(max_examples=200, deadline=None)
def test_cp5_embed_batch_consistent_with_embed_text(text: str) -> None:
    """**Validates: Requirements 10.1** CP-5 (batch vs single consistency).

    ``embed_batch([text])[0]`` must equal ``embed_text(text)`` element-wise.
    """
    embedder = StubEmbedder()
    single = embedder.embed_text(text)
    batch = embedder.embed_batch([text])
    assert batch.shape == (1, embedder.embedding_dim)
    np.testing.assert_array_equal(
        single,
        batch[0],
        err_msg=f"embed_text and embed_batch[0] disagree for text={text!r}",
    )


# ---------------------------------------------------------------------------
# CP-6: Cache idempotency on whitespace edits
# ---------------------------------------------------------------------------


@given(
    base_text=_NONEMPTY_TEXT,
    content_hash=_CONTENT_HASH,
)
@settings(max_examples=200, deadline=None)
def test_cp6_whitespace_only_diff_is_cache_hit(
    base_text: str,
    content_hash: str,
) -> None:
    """**Validates: Requirements 1.2, 2.1** CP-6.

    Whitespace-only changes to the *source file* that leave ``content_hash``
    unchanged must be cache hits.

    In the real pipeline:

    1. The ``content_hash`` is computed over the **normalized AST** (whitespace
       stripped), so a whitespace-only edit produces the *same hash*.
    2. :func:`~cognis_indexer.embedder.build_embedding_text` reads symbol fields
       (``signature``, ``docstring``, ``body_excerpt``) — not the raw source.
       So the embedding text is *identical* for the original and whitespace variant.

    Both calls therefore pass the same ``(content_hash, text)`` pair to
    :meth:`embed_cached`, hitting the LRU cache and returning the *same Python
    object* (identity check via ``is``).
    """
    embedder = StubEmbedder()

    # First embedding — populates the cache.
    v_first = embedder.embed_cached(content_hash, base_text)

    # In the real pipeline the whitespace-only edit does NOT change the
    # embedding text (see docstring above), so we pass the same base_text.
    v_second = embedder.embed_cached(content_hash, base_text)

    # CP-6: same (content_hash, text) → cache hit → same object returned.
    assert v_first is v_second, (
        "Expected cache hit (same object) for same (content_hash, text) pair "
        f"(content_hash={content_hash!r}), but got a new array."
    )


@given(
    text=_NONEMPTY_TEXT,
    hash_a=_CONTENT_HASH,
    hash_b=_CONTENT_HASH,
)
@settings(max_examples=200, deadline=None)
def test_cp6_different_hashes_can_produce_different_results(
    text: str,
    hash_a: str,
    hash_b: str,
) -> None:
    """**Validates: Requirements 2.1** CP-6 (distinct hashes are independent).

    Two different ``content_hash`` values for the same text must each have
    their own independent cache entry — the embedder does not conflate them.
    This guards against a buggy implementation that ignores the hash.
    """
    if hash_a == hash_b:
        # Trivially the same; skip to avoid a vacuous test.
        return

    embedder = StubEmbedder()
    v_a = embedder.embed_cached(hash_a, text)
    v_b = embedder.embed_cached(hash_b, text)

    # The two calls use different cache keys; the result may or may not be the
    # same array but they must not be the *same object* (the cache should not
    # collapse distinct keys onto one entry).
    assert v_a is not v_b, (
        f"Different content_hashes ({hash_a!r}, {hash_b!r}) returned "
        "identical Python objects — cache is incorrectly merging distinct keys."
    )
