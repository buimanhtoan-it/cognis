"""Reranking seam for the retrieval mesh.

The retrieval layers (lexical / semantic / structural / CSAR) each produce a
:class:`~cognis_retrieval.base.Hit` list. A *reranker* takes the fused candidate
set and reorders it with a stronger (usually cross-encoder) relevance model
before the capsule composer trims to the token budget.

Why a dedicated seam
--------------------
``config.reranker`` (backend / model / enabled) has existed since the MVP config
schema, but there was no interface or implementation behind it — turning it on
would have meant editing the capsule/retrieval flow directly. This module closes
that gap so a reranking model can be plugged in (or swapped) without touching
the engine flow:

- :class:`Reranker` — the structural protocol every backend satisfies.
- :class:`NoOpReranker` — the **default**. A Null-Object implementation that
  returns hits unchanged (optionally truncated to *k*). When
  ``reranker.enabled`` is ``false`` the engine calls this, so the flow is byte
  -for-byte identical to having no reranker at all (zero regression).
- :class:`CrossEncoderReranker` — opt-in cross-encoder stub
  (``bge-reranker-v2-m3``) activated only when ``reranker.enabled`` is ``true``.
- :func:`build_reranker` — the single factory call sites use; it returns
  :class:`NoOpReranker` whenever reranking is disabled.

Adding a backend is the same one-decorator pattern as the embedder registry::

    @register_reranker("my_backend")
    def _build(config: RerankerConfig) -> Reranker:
        from cognis_retrieval.reranker import MyReranker

        return MyReranker(model=config.model)
"""

from __future__ import annotations

from collections.abc import Callable
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from cognis_retrieval.base import Hit

if TYPE_CHECKING:
    from cognis.config import RerankerConfig

__all__ = [
    "CrossEncoderReranker",
    "NoOpReranker",
    "Reranker",
    "RerankerFactory",
    "UnknownRerankerBackendError",
    "available_reranker_backends",
    "build_reranker",
    "register_reranker",
]


# ---------------------------------------------------------------------------
# Protocol
# ---------------------------------------------------------------------------


@runtime_checkable
class Reranker(Protocol):
    """Structural protocol every reranking backend satisfies.

    Any object with a ``name`` attribute and a ``rerank`` method of the correct
    signature satisfies this protocol — no inheritance required.
    """

    name: str
    """Backend identifier, e.g. ``"noop"`` or ``"local"``."""

    def rerank(self, query: str, hits: list[Hit], k: int) -> list[Hit]:
        """Reorder *hits* by relevance to *query* and return the top *k*.

        Args:
            query: The original natural-language query.
            hits: Fused candidate hits from the retrieval layers.
            k: Maximum number of hits to return.

        Returns:
            A list of at most *k* :class:`Hit` ordered by descending relevance.
        """
        ...


# ---------------------------------------------------------------------------
# NoOpReranker — the default (Null Object)
# ---------------------------------------------------------------------------


class NoOpReranker:
    """Pass-through reranker used whenever reranking is disabled.

    Returns the input hits unchanged apart from truncating to *k*. This keeps
    the engine's call shape uniform — the composer always calls
    ``reranker.rerank(...)`` — without changing behaviour when the feature is
    off.
    """

    name: str = "noop"

    def rerank(self, query: str, hits: list[Hit], k: int) -> list[Hit]:
        """Return the first *k* hits unchanged (no reordering)."""
        if k <= 0:
            return []
        return hits[:k]


# ---------------------------------------------------------------------------
# CrossEncoderReranker — opt-in stub
# ---------------------------------------------------------------------------


class CrossEncoderReranker:
    """Cross-encoder reranking backend (stub at MVP).

    Intended to wrap a ``sentence-transformers`` ``CrossEncoder`` such as
    ``BAAI/bge-reranker-v2-m3``: score each ``(query, hit_text)`` pair and sort
    by the cross-encoder score. At MVP this is a stub that preserves the input
    order so enabling it never regresses ranking quality before the model wiring
    lands.

    TODO: load ``CrossEncoder(model_name)`` lazily and score pairs built from the
          hydrated symbol text (signature + docstring + body excerpt). Hydration
          needs DB access, so the real implementation will take a ``Database``
          (or a text-resolver callable) — wire that through ``build_reranker``
          when activating.

    Args:
        model_name: Hugging Face cross-encoder id. Defaults to
            ``"BAAI/bge-reranker-v2-m3"``.
    """

    name: str = "local"

    _DEFAULT_MODEL = "BAAI/bge-reranker-v2-m3"

    def __init__(self, model_name: str = _DEFAULT_MODEL) -> None:
        self._model_name = model_name

    def rerank(self, query: str, hits: list[Hit], k: int) -> list[Hit]:
        """Return the top *k* hits.

        Stub behaviour: preserve input order (already score-sorted by the fusion
        step). Replace with cross-encoder scoring when activating.
        """
        if k <= 0:
            return []
        return hits[:k]


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------


RerankerFactory = Callable[["RerankerConfig"], Reranker]
"""Signature every registered reranker factory must satisfy."""


class UnknownRerankerBackendError(ValueError):
    """Raised when ``config.reranker.backend`` has no registered factory."""


_RERANKER_FACTORIES: dict[str, RerankerFactory] = {}


def register_reranker(name: str) -> Callable[[RerankerFactory], RerankerFactory]:
    """Register *factory* under the reranker backend id *name*."""

    def _decorator(factory: RerankerFactory) -> RerankerFactory:
        _RERANKER_FACTORIES[name] = factory
        return factory

    return _decorator


def available_reranker_backends() -> list[str]:
    """Return the sorted list of registered reranker backend ids."""
    return sorted(_RERANKER_FACTORIES)


def build_reranker(config: RerankerConfig) -> Reranker:
    """Construct the reranker selected by *config*.

    Returns a :class:`NoOpReranker` whenever ``config.enabled`` is ``false`` so
    callers can unconditionally call ``rerank(...)`` without branching on the
    flag themselves.

    Args:
        config: The ``reranker:`` config section.

    Returns:
        A concrete object satisfying the :class:`Reranker` protocol.

    Raises:
        UnknownRerankerBackendError: When reranking is enabled but
            ``config.backend`` is not registered.
        ImportError: When the backend's optional dependency is missing
            (propagated from the factory).
    """
    if not config.enabled:
        return NoOpReranker()

    factory = _RERANKER_FACTORIES.get(config.backend)
    if factory is None:
        raise UnknownRerankerBackendError(
            f"unknown reranker backend {config.backend!r}; "
            f"available backends: {available_reranker_backends()}"
        )
    return factory(config)


# ---------------------------------------------------------------------------
# Built-in backend registrations
# ---------------------------------------------------------------------------


@register_reranker("local")
def _build_local_reranker(config: RerankerConfig) -> Reranker:
    """Local cross-encoder reranker (stub at MVP)."""
    return CrossEncoderReranker(model_name=config.model)
