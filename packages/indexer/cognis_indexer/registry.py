"""Embedder registry — single source of truth for backend selection.

Centralises the ``config.embedder.backend`` → concrete :class:`Embedder`
mapping that was previously duplicated across ``cognis-indexd``,
``cognis-mcpd``, ``cognis-cli``, and the eval harness. Adding a new embedding
model now means registering one factory here — no call-site edits, no
``if backend == ...`` chains to keep in sync (OCP).

Usage
-----
.. code-block:: python

    from cognis.config import Config
    from cognis_indexer.registry import build_embedder

    cfg = Config.load(repo_root)
    embedder = build_embedder(cfg.embedder)  # raises on unknown / missing deps

Callers choose their own failure policy around the raised exceptions:

- ``cognis-indexd`` / ``cognis-mcpd`` catch and degrade to ``embedder=None``
  so lexical + structural retrieval keep working.
- ``cognis-cli index`` surfaces the error and exits non-zero.

Registering a new backend
--------------------------
.. code-block:: python

    @register_embedder("my_backend")
    def _build_my_backend(config: EmbedderConfig) -> Embedder:
        from cognis_indexer.embedder import MyEmbedder

        return MyEmbedder(model=config.model)

The import of the concrete class stays *inside* the factory so optional
dependencies (sentence-transformers, voyageai, openai) are only imported when
that backend is actually selected.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from cognis.config import EmbedderConfig

    from cognis_indexer.embedder import Embedder

__all__ = [
    "EmbedderFactory",
    "UnknownEmbedderBackendError",
    "available_backends",
    "build_embedder",
    "register_embedder",
]


EmbedderFactory = Callable[["EmbedderConfig"], "Embedder"]
"""Signature every registered embedder factory must satisfy."""


class UnknownEmbedderBackendError(ValueError):
    """Raised when ``config.embedder.backend`` has no registered factory."""


# Backend name → factory. Populated by the ``@register_embedder`` decorator
# at import time (see the registrations at the bottom of this module).
_EMBEDDER_FACTORIES: dict[str, EmbedderFactory] = {}


def register_embedder(name: str) -> Callable[[EmbedderFactory], EmbedderFactory]:
    """Register *factory* under the backend id *name*.

    Args:
        name: The ``config.embedder.backend`` value this factory handles.

    Returns:
        A decorator that records the factory and returns it unchanged.
    """

    def _decorator(factory: EmbedderFactory) -> EmbedderFactory:
        _EMBEDDER_FACTORIES[name] = factory
        return factory

    return _decorator


def available_backends() -> list[str]:
    """Return the sorted list of registered embedder backend ids."""
    return sorted(_EMBEDDER_FACTORIES)


def build_embedder(config: EmbedderConfig) -> Embedder:
    """Construct the embedder selected by ``config.backend``.

    Args:
        config: The ``embedder:`` config section.

    Returns:
        A concrete object satisfying the :class:`Embedder` protocol.

    Raises:
        UnknownEmbedderBackendError: When ``config.backend`` is not registered.
        ImportError: When the backend's optional dependency is not installed
            (propagated from the factory so callers can choose their policy).
    """
    factory = _EMBEDDER_FACTORIES.get(config.backend)
    if factory is None:
        raise UnknownEmbedderBackendError(
            f"unknown embedder backend {config.backend!r}; "
            f"available backends: {available_backends()}"
        )
    return factory(config)


# ---------------------------------------------------------------------------
# Built-in backend registrations
# ---------------------------------------------------------------------------


@register_embedder("local")
def _build_local(config: EmbedderConfig) -> Embedder:
    """``sentence-transformers`` local backend (default)."""
    from cognis_indexer.embedder import LocalEmbedder

    return LocalEmbedder(model_name=config.model, batch_size=config.batch_size)


@register_embedder("voyage")
def _build_voyage(config: EmbedderConfig) -> Embedder:
    """Voyage-code-3 backend (stub at MVP)."""
    from cognis_indexer.embedder import VoyageEmbedder

    return VoyageEmbedder(model=config.model)


@register_embedder("openai")
def _build_openai(config: EmbedderConfig) -> Embedder:
    """OpenAI text-embedding backend (stub at MVP)."""
    from cognis_indexer.embedder import OpenAIEmbedder

    return OpenAIEmbedder(model=config.model)
