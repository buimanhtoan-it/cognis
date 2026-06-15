"""Embedder stage of the cognis indexer pipeline.

Implements tasks 10.1-10.8 from ``.kiro/specs/cognis/tasks.md``:

- **10.1** :class:`Embedder` protocol interface (``embed_batch``, ``embed_text``,
  ``embedding_dim``).
- **10.2** :class:`LocalEmbedder` — ``sentence-transformers`` with
  ``BAAI/bge-small-en-v1.5`` (384-d), pinned at ``EMBEDDING_DIM = 384`` for MVP.
  Schema-migration note for future higher-dim swap is in ``packages/core/cognis/db.py``
  where ``EMBEDDING_DIM`` is the single source of truth.
- **10.3** :class:`VoyageEmbedder` stub — opt-in via config flag, returns zeros
  at MVP.  ``TODO: implement actual voyage-code-3 API call when activating.``
- **10.4** :func:`build_embedding_text` — concatenates ``[kind] qualified_name``,
  ``signature``, ``docstring``, and ``body_excerpt[:1500]``.
- **10.5** Worker pool: ``ThreadPoolExecutor(max_workers=min(os.cpu_count(), 4))``
  for local; bounded queue size 256.
- **10.6** LRU cache: ``functools.lru_cache(maxsize=50_000)`` keyed on
  ``content_hash`` via :meth:`LocalEmbedder.embed_cached`.
- **10.7** PBT properties CP-5 and CP-6 in ``tests/pbt/test_embedder_pbt.py``.
- **10.8** :func:`assert_vec_dim` startup guard asserts the ``symbol_vec`` DDL
  dimension matches :attr:`Embedder.embedding_dim`.

Design references:

- *Indexer Pipeline → Embedder* (design.md) — embedding text template,
  pool size, cache key.
- *Data Models → symbol_vec* — ``FLOAT[384]`` pinned at MVP.
- *Correctness Properties CP-5, CP-6* — determinism and cache idempotency.
- *Resolved Open Questions Q-1, Q-2* — local-first, dim=384.
"""

from __future__ import annotations

import concurrent.futures
import logging
import os
import re
import sqlite3
import threading
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from functools import lru_cache
from typing import TYPE_CHECKING, Protocol, cast, runtime_checkable

# ``numpy`` ships under the ``embed-local`` optional extra. Import it lazily so
# this module — and the whole indexer pipeline that imports it — stays
# importable without the extra (e.g. a lexical+structural-only install). ``np``
# is only used inside embedding code paths, which are unreachable until an
# embedder is constructed; constructing ``LocalEmbedder`` requires
# ``sentence-transformers`` (which pulls in numpy), so a ``None`` here can never
# reach a real embedding call.
if TYPE_CHECKING:
    import numpy as np
    from numpy.typing import NDArray

    from cognis_indexer.enricher.enricher import EnrichedSymbol
    from cognis_indexer.parsers.base import ParsedSymbol
else:
    try:
        import numpy as np
    except ImportError:  # pragma: no cover - exercised only without embed-local
        np = None

from cognis.db import EMBEDDING_DIM, Database

logger = logging.getLogger(__name__)

# A first-run embedder load that exceeds this (seconds) is the painful
# fresh-user path the panel/agent should surface, so we escalate it to WARNING
# (visible at the default mcpd log level) instead of INFO.
_SLOW_LOAD_WARN_S = 10.0

__all__ = [
    "EMBEDDING_DIM",
    "Embedder",
    "LocalEmbedder",
    "OpenAIEmbedder",
    "VoyageEmbedder",
    "assert_vec_dim",
    "build_embedding_text",
]

# ---------------------------------------------------------------------------
# Embedder protocol (task 10.1)
# ---------------------------------------------------------------------------


@runtime_checkable
class Embedder(Protocol):
    """Structural protocol for embedding backends.

    Any object with the two ``embed_*`` methods and an ``embedding_dim``
    attribute satisfies this protocol (no inheritance required).
    """

    embedding_dim: int
    """Dimensionality of the embedding vectors this backend produces."""

    def embed_batch(self, texts: list[str]) -> NDArray[np.float32]:
        """Embed a list of texts and return an ``(N, dim)`` float32 array.

        Args:
            texts: Non-empty list of strings to embed.

        Returns:
            ``numpy.ndarray`` of shape ``(len(texts), self.embedding_dim)``
            and dtype ``float32``.
        """
        ...

    def embed_text(self, text: str) -> NDArray[np.float32]:
        """Embed a single text and return a ``(dim,)`` float32 array.

        Args:
            text: The text to embed.

        Returns:
            ``numpy.ndarray`` of shape ``(self.embedding_dim,)`` and dtype
            ``float32``.
        """
        ...


# ---------------------------------------------------------------------------
# Embedding text builder (task 10.4)
# ---------------------------------------------------------------------------


def build_embedding_text(symbol: ParsedSymbol | EnrichedSymbol) -> str:
    """Build the canonical embedding input text for a symbol.

    Per design *Indexer Pipeline -> Embedder*::

        f"[{kind}] {qualified_name}\\n{signature}\\n{docstring}\\n{body_excerpt[:1500]}"

    Parts that are ``None`` or empty are omitted so the embedder sees a clean
    representation even when optional fields are absent.

    Args:
        symbol: Either a :class:`~cognis_indexer.parsers.base.ParsedSymbol` or
            an :class:`~cognis_indexer.enricher.enricher.EnrichedSymbol`.
            ``EnrichedSymbol`` wraps ``ParsedSymbol`` in its ``.symbol``
            attribute; this function unwraps automatically.

    Returns:
        A newline-joined string ready to be fed to the embedder.
    """
    # EnrichedSymbol wraps ParsedSymbol in a .symbol attribute; unwrap if present.
    # We use hasattr + cast so mypy can follow the type narrowing.
    from cognis_indexer.parsers.base import ParsedSymbol as _ParsedSymbol

    if hasattr(symbol, "symbol"):
        # EnrichedSymbol — extract the inner ParsedSymbol.
        # The quoted union type resolves to Any at runtime (EnrichedSymbol is
        # TYPE_CHECKING-only), so mypy doesn't flag attribute access here.
        sym: _ParsedSymbol = symbol.symbol
    else:
        sym = symbol
    parts: list[str] = [f"[{sym.kind}] {sym.qualified_name}"]
    if sym.signature:
        parts.append(sym.signature)
    if sym.docstring:
        parts.append(sym.docstring)
    if sym.body_excerpt:
        parts.append(sym.body_excerpt[:1500])
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# LocalEmbedder (tasks 10.2, 10.5, 10.6)
# ---------------------------------------------------------------------------


def _resolve_local_model_name(model_name: str) -> str:
    """Map short config aliases to Hugging Face repo ids.

    ``sentence-transformers`` treats unqualified names like ``bge-small-en-v1.5``
    as ``sentence-transformers/bge-small-en-v1.5``, which does not exist.  The
    canonical public model is ``BAAI/bge-small-en-v1.5``.
    """
    if "/" in model_name:
        return model_name
    aliases = {
        "bge-small-en-v1.5": "BAAI/bge-small-en-v1.5",
    }
    return aliases.get(model_name, model_name)


def _load_sentence_transformer(
    sentence_transformer_cls: Callable[..., object],
    model_name: str,
    device: str | None,
) -> object:
    """Load a SentenceTransformer, preferring the local cache.

    ``sentence-transformers`` revalidates every model file against the Hugging
    Face Hub on construction. When the model is already cached this still costs
    one network round-trip per file (~30 serial HTTPS calls for bge-small),
    which adds tens of seconds to the first semantic query — and hangs outright
    when offline.

    We first attempt a fully offline load (``local_files_only=True``). If the
    model is not cached yet, we fall back to a normal (online) load so the very
    first install can still download the weights. The offline preference can be
    forced or disabled via ``COGNIS_EMBED_OFFLINE`` (``1``/``0``).

    Honors ``HF_HUB_OFFLINE`` / ``TRANSFORMERS_OFFLINE`` when already set by the
    operator (we never downgrade an explicit offline request to online).
    """
    offline_pref = os.environ.get("COGNIS_EMBED_OFFLINE", "").strip().lower()
    force_offline = offline_pref in {"1", "true", "yes"} or any(
        os.environ.get(var, "").strip().lower() in {"1", "true", "yes"}
        for var in ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE")
    )
    allow_offline_attempt = offline_pref not in {"0", "false", "no"}

    started = time.perf_counter()
    if allow_offline_attempt:
        try:
            model = sentence_transformer_cls(model_name, device=device, local_files_only=True)
            logger.info(
                "embedder model %r loaded from local cache in %.1fs",
                model_name,
                time.perf_counter() - started,
            )
            return model
        except Exception:
            if force_offline:
                # Operator explicitly demanded offline; surface the real error
                # instead of silently reaching out to the network.
                raise
            # Model not cached yet — fall through to an online load so the
            # first run can download the weights. This is the slow, first-run
            # path (download + per-file Hub revalidation): tell the operator
            # plainly so an opaque multi-second wait has a visible reason.
            logger.warning(
                "embedder model %r not in local cache; downloading/revalidating "
                "from Hugging Face (one-time first-run step, can take tens of "
                "seconds)…",
                model_name,
            )

    online_started = time.perf_counter()
    model = sentence_transformer_cls(model_name, device=device)
    total = time.perf_counter() - started
    emit = logger.warning if total >= _SLOW_LOAD_WARN_S else logger.info
    emit(
        "embedder model %r loaded via online path in %.1fs (download/encode %.1fs)",
        model_name,
        total,
        time.perf_counter() - online_started,
    )
    return model


def _detect_model_dim(model: object, default: int) -> int:
    """Return the embedding dimension reported by *model*, or *default*.

    Newer ``sentence-transformers`` exposes ``get_embedding_dimension()``; older
    versions use ``get_sentence_embedding_dimension()``. We probe the new name
    first (avoiding the deprecation warning), then fall back, then to *default*
    so a backend that implements neither degrades gracefully.
    """
    for attr in ("get_embedding_dimension", "get_sentence_embedding_dimension"):
        getter = getattr(model, attr, None)
        if not callable(getter):
            continue
        try:
            value = getter()
        except Exception:
            continue
        if isinstance(value, int) and value > 0:
            return value
    return default


class LocalEmbedder:
    """Embedding backend using ``sentence-transformers`` with ``bge-small-en-v1.5``.

    Implements the :class:`Embedder` protocol.

    Model: ``BAAI/bge-small-en-v1.5``, 384-dimensional, Apache-2.0 license.
    Dim is **pinned at 384** for MVP per the resolved design Q-2.

    Worker pool (task 10.5):
        ``ThreadPoolExecutor(max_workers=min(os.cpu_count() or 1, 4))`` with a
        bounded queue size of 256.  The pool is created lazily on first embed
        call so the constructor is cheap even if ``sentence-transformers`` takes
        a moment to load.

    LRU cache (task 10.6):
        :meth:`embed_cached` uses ``functools.lru_cache(maxsize=50_000)`` keyed
        on ``(content_hash, text)``.  The hash is the primary discriminator;
        the ``text`` argument is the cache-miss value used to compute the
        actual embedding.  Whitespace-only changes that don't alter the parser's
        normalized ``content_hash`` therefore hit the cache (CP-6).

    Args:
        model_name: HuggingFace model identifier.
            Defaults to ``"BAAI/bge-small-en-v1.5"``.
        batch_size: Number of texts to embed in one model call.
            Defaults to ``32`` per design configuration.
        device: Device passed to ``SentenceTransformer``.
            Defaults to ``None`` (auto-select CPU/GPU).
    """

    embedding_dim: int = EMBEDDING_DIM
    """Vector size, derived from the loaded model at construction.

    Defaults to the class-level :data:`EMBEDDING_DIM` (384, bge-small) but is
    overridden per-instance from the SentenceTransformer's reported dimension,
    so plugging in a different-sized model just works without editing constants.
    """

    _DEFAULT_MODEL = "BAAI/bge-small-en-v1.5"
    _QUEUE_MAX = 256

    def __init__(
        self,
        model_name: str = _DEFAULT_MODEL,
        batch_size: int = 32,
        device: str | None = None,
    ) -> None:
        model_name = _resolve_local_model_name(model_name)
        try:
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise ImportError(
                "sentence-transformers is required for LocalEmbedder. "
                "Install it via: pip install cognis-engine[embed-local]"
            ) from exc

        self._model: object = _load_sentence_transformer(SentenceTransformer, model_name, device)
        self._batch_size = batch_size
        self._model_name = model_name

        # Let the vector size flow from the model rather than a pinned constant.
        # ``get_sentence_embedding_dimension`` is the canonical accessor; fall
        # back to the class default if the backend doesn't expose it.
        self.embedding_dim = _detect_model_dim(self._model, EMBEDDING_DIM)

        # Worker pool: min(cpu_count, 4) threads (task 10.5).
        # The pool is used for concurrent batch preparation; the actual
        # SentenceTransformer encode call is synchronous within each task.
        n_workers = min(os.cpu_count() or 1, 4)
        self._pool = ThreadPoolExecutor(max_workers=n_workers)

        # Bound the internal submit queue to 256 pending futures to apply
        # backpressure when the indexer is faster than the embedder (task 10.5).
        self._queue_semaphore = threading.Semaphore(self._QUEUE_MAX)

        # Warm up the lru_cache method so the same instance reuses it.
        # The lru_cache is applied per-instance via _make_embed_cached.
        self._embed_cached_fn = _make_embed_cached(self)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def embed_batch(self, texts: list[str]) -> NDArray[np.float32]:
        """Embed a list of texts; returns ``(N, 384)`` float32 array.

        Args:
            texts: Non-empty list of strings.

        Returns:
            ``numpy.ndarray`` of shape ``(len(texts), 384)``, dtype float32.
        """
        if not texts:
            return np.empty((0, self.embedding_dim), dtype=np.float32)

        from sentence_transformers import SentenceTransformer

        # ``self._model`` is annotated ``object`` to keep this module importable
        # without the optional ``embed-local`` extra. Narrow it to the concrete
        # type here via ``cast`` rather than an annotated assignment: ``cast`` is
        # stable whether ``sentence-transformers`` exposes types (newer releases
        # ship ``py.typed``) or is treated as ``Any``, so it never trips
        # ``warn_unused_ignores`` when the dependency version floats.
        model = cast(SentenceTransformer, self._model)
        result = model.encode(
            texts,
            batch_size=self._batch_size,
            convert_to_numpy=True,
            show_progress_bar=False,
            normalize_embeddings=True,
        )
        arr: NDArray[np.float32] = np.asarray(result, dtype=np.float32)
        return arr

    def embed_text(self, text: str) -> NDArray[np.float32]:
        """Embed a single text; returns ``(384,)`` float32 array."""
        result = self.embed_batch([text])
        return result[0]  # type: ignore[no-any-return]

    def embed_cached(self, content_hash: str, text: str) -> NDArray[np.float32]:
        """Embed *text* with an LRU cache keyed on *content_hash*.

        Whitespace-only edits that share the same parser-level ``content_hash``
        (computed over normalized AST) are resolved as cache hits (CP-6).

        Args:
            content_hash: The ``ParsedSymbol.content_hash`` value.
                Used as the cache key.
            text: The embedding text (from :func:`build_embedding_text`).
                Used on a cache miss to compute the embedding.

        Returns:
            ``numpy.ndarray`` of shape ``(384,)``, dtype float32.
        """
        return self._embed_cached_fn(content_hash, text)

    # ------------------------------------------------------------------
    # Pool submission helper
    # ------------------------------------------------------------------

    def submit_batch(self, texts: list[str]) -> concurrent.futures.Future[NDArray[np.float32]]:
        """Submit *texts* to the worker pool with bounded-queue backpressure.

        Blocks when the queue is at capacity (256 pending tasks) to prevent
        unbounded memory growth during large full-index runs (task 10.5).

        Returns:
            A :class:`~concurrent.futures.Future` resolving to an ``(N, 384)``
            float32 array.
        """
        self._queue_semaphore.acquire()
        future: concurrent.futures.Future[NDArray[np.float32]] = self._pool.submit(
            self.embed_batch, texts
        )

        def _release(_f: concurrent.futures.Future[NDArray[np.float32]]) -> None:
            self._queue_semaphore.release()

        future.add_done_callback(_release)
        return future

    def __del__(self) -> None:
        """Shut down the thread pool gracefully on GC / process exit."""
        pool = getattr(self, "_pool", None)
        if pool is not None:
            pool.shutdown(wait=False)


# ---------------------------------------------------------------------------
# Per-instance LRU cache factory (task 10.6)
# ---------------------------------------------------------------------------


def _make_embed_cached(
    embedder: LocalEmbedder,
) -> Callable[[str, str], NDArray[np.float32]]:
    """Return an ``lru_cache``-wrapped embed function bound to *embedder*.

    We can't attach ``lru_cache`` directly to an instance method (the ``self``
    arg would prevent sharing the cache across calls).  Instead, we create a
    free function that closes over *embedder* and cache that.

    Cache size 50 000 (task 10.6).
    """

    @lru_cache(maxsize=50_000)
    def _cached(content_hash: str, text: str) -> NDArray[np.float32]:
        return embedder.embed_text(text)

    return _cached


# ---------------------------------------------------------------------------
# VoyageEmbedder stub (task 10.3)
# ---------------------------------------------------------------------------


class VoyageEmbedder:
    """Stub Voyage-code-3 embedding backend (opt-in, feature-flagged off by default).

    Activated only when:
    1. The ``voyageai`` package is installed, AND
    2. The ``embedder.backend = "voyage"`` config flag is set.

    At MVP this returns **zero vectors** for every input.  A real implementation
    should call the Voyage API.

    TODO: implement actual ``voyage-code-3`` API call when activating in Phase 2.
          The actual dim for voyage-code-3 is 1024, but for schema compat at MVP
          this stub uses ``EMBEDDING_DIM = 384``.  Update DDL migration + this
          constant when switching.

    Args:
        api_key: Voyage API key.  If ``None``, falls back to ``VOYAGE_API_KEY``
            env var.
        model: Voyage model name.  Defaults to ``"voyage-code-3"``.
    """

    embedding_dim: int = EMBEDDING_DIM
    """384 at MVP for schema compatibility; change to 1024 when activating."""

    _DEFAULT_MODEL = "voyage-code-3"

    def __init__(
        self,
        api_key: str | None = None,
        model: str = _DEFAULT_MODEL,
    ) -> None:
        self._api_key = api_key or os.environ.get("VOYAGE_API_KEY", "")
        self._model = model

        # Soft-check: voyageai package is optional.
        try:
            import voyageai  # type: ignore[import-not-found]  # noqa: F401

            self._voyageai_available = True
        except ImportError:
            self._voyageai_available = False

    def embed_batch(self, texts: list[str]) -> NDArray[np.float32]:
        """Return zero vectors at MVP (stub).

        TODO: Replace with real Voyage API call when activating.
        """
        # TODO: call voyageai.Client(api_key=self._api_key).embed(texts, model=self._model)
        return np.zeros((len(texts), self.embedding_dim), dtype=np.float32)

    def embed_text(self, text: str) -> NDArray[np.float32]:
        """Return a zero vector at MVP (stub)."""
        return np.zeros(self.embedding_dim, dtype=np.float32)


# ---------------------------------------------------------------------------
# OpenAIEmbedder stub (config backend "openai")
# ---------------------------------------------------------------------------


class OpenAIEmbedder:
    """Stub OpenAI embedding backend (opt-in, feature-flagged off by default).

    Exists so the ``embedder.backend = "openai"`` config value (declared in
    :data:`cognis.config.EmbedderBackend`) has a concrete implementation and is
    selectable through the embedder registry.  At MVP this returns **zero
    vectors** for every input — wiring the registry is intentionally decoupled
    from the network implementation so the engine flow is identical regardless
    of which backend is active.

    Activated only when:
    1. The ``openai`` package is installed, AND
    2. The ``embedder.backend = "openai"`` config flag is set.

    TODO: implement the real ``text-embedding-3-*`` API call when activating.
          ``text-embedding-3-small`` is 1536-d; this stub uses
          ``EMBEDDING_DIM = 384`` for schema compatibility at MVP.  Update the
          DDL migration + ``embedding_dim`` when switching.

    Args:
        api_key: OpenAI API key.  If ``None``, falls back to ``OPENAI_API_KEY``
            env var.
        model: OpenAI embedding model name.
            Defaults to ``"text-embedding-3-small"``.
    """

    embedding_dim: int = EMBEDDING_DIM
    """384 at MVP for schema compatibility; change when activating."""

    _DEFAULT_MODEL = "text-embedding-3-small"

    def __init__(
        self,
        api_key: str | None = None,
        model: str = _DEFAULT_MODEL,
    ) -> None:
        self._api_key = api_key or os.environ.get("OPENAI_API_KEY", "")
        self._model = model

        # Soft-check: openai package is optional. Use find_spec so this works
        # whether or not the dependency is installed (no env-dependent import).
        import importlib.util

        self._openai_available = importlib.util.find_spec("openai") is not None

    def embed_batch(self, texts: list[str]) -> NDArray[np.float32]:
        """Return zero vectors at MVP (stub).

        TODO: Replace with real OpenAI API call when activating.
        """
        return np.zeros((len(texts), self.embedding_dim), dtype=np.float32)

    def embed_text(self, text: str) -> NDArray[np.float32]:
        """Return a zero vector at MVP (stub)."""
        return np.zeros(self.embedding_dim, dtype=np.float32)


# ---------------------------------------------------------------------------
# Startup dimension assertion (task 10.8)
# ---------------------------------------------------------------------------

# Regex to extract the dimension from FLOAT[N] in a vec0 CREATE TABLE statement.
_VEC_DIM_RE = re.compile(r"FLOAT\[(\d+)\]", re.IGNORECASE)


def assert_vec_dim(db: Database, embedder: Embedder) -> None:
    """Assert that the ``symbol_vec`` DDL dimension matches *embedder.embedding_dim*.

    Should be called once at startup (before any embedding or KNN query).
    Raises :class:`AssertionError` with a descriptive message when there is a
    mismatch, so operators see a clear failure rather than silent NaN/shape errors.

    The check reads ``sqlite_master`` for the ``symbol_vec`` table definition.
    It handles two shapes:

    1. ``CREATE VIRTUAL TABLE symbol_vec USING vec0(... embedding FLOAT[N] ...)``
       — produced when sqlite-vec is loaded (the common production path).
    2. Any other DDL without a ``FLOAT[N]`` token — treated as the plain-table
       fallback path (no assertion needed since the fallback has no hard-coded
       dim constraint; we skip the check).

    Args:
        db: The :class:`~cognis.db.Database` to inspect.
        embedder: Any object satisfying the :class:`Embedder` protocol.

    Raises:
        AssertionError: When a ``FLOAT[N]`` token is found and N ≠
            ``embedder.embedding_dim``.
    """
    conn: sqlite3.Connection = db.connect()
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type IN ('table', 'shadow') AND name = 'symbol_vec'"
    ).fetchone()

    if row is None:
        # Table doesn't exist yet (first boot before migration ran); skip.
        return

    ddl: str = str(row[0] or "")
    m = _VEC_DIM_RE.search(ddl)
    if m is None:
        # Plain fallback table — no FLOAT[N] token; nothing to assert.
        return

    ddl_dim = int(m.group(1))
    assert ddl_dim == embedder.embedding_dim, (
        f"symbol_vec DDL dimension ({ddl_dim}) does not match "
        f"embedder.embedding_dim ({embedder.embedding_dim}). "
        f"Run a schema migration or switch to a {ddl_dim}-dimensional embedder. "
        f"DDL: {ddl!r}"
    )
