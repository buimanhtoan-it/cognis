"""Unit tests for the embedder registry (``cognis_indexer.registry``).

Validates the single-source-of-truth backend selection that replaced the
duplicated ``if backend == ...`` chains across indexd / mcpd / cli / eval.
"""

from __future__ import annotations

import pytest
from cognis.config import EmbedderConfig
from cognis_indexer.embedder import Embedder, OpenAIEmbedder, VoyageEmbedder
from cognis_indexer.registry import (
    UnknownEmbedderBackendError,
    available_backends,
    build_embedder,
    register_embedder,
)


def test_available_backends_includes_builtins() -> None:
    backends = available_backends()
    assert {"local", "voyage", "openai"} <= set(backends)


def test_build_voyage_returns_protocol_instance() -> None:
    embedder = build_embedder(EmbedderConfig(backend="voyage"))
    assert isinstance(embedder, VoyageEmbedder)
    assert isinstance(embedder, Embedder)


def test_build_openai_returns_protocol_instance() -> None:
    embedder = build_embedder(EmbedderConfig(backend="openai"))
    assert isinstance(embedder, OpenAIEmbedder)
    assert isinstance(embedder, Embedder)


def test_build_unknown_backend_raises() -> None:
    # ``EmbedderConfig`` validates the Literal, so bypass it with a stub object
    # carrying an unregistered backend string.
    class _FakeCfg:
        backend = "does-not-exist"
        model = "x"
        batch_size = 32

    with pytest.raises(UnknownEmbedderBackendError) as exc_info:
        build_embedder(_FakeCfg())  # type: ignore[arg-type]
    assert "does-not-exist" in str(exc_info.value)


def test_register_custom_backend_roundtrip() -> None:
    sentinel = object()

    @register_embedder("_test_custom")
    def _factory(config: object) -> Embedder:
        return sentinel  # type: ignore[return-value]

    class _FakeCfg:
        backend = "_test_custom"
        model = "x"
        batch_size = 32

    try:
        assert "_test_custom" in available_backends()
        result = build_embedder(_FakeCfg())  # type: ignore[arg-type]
        assert result is sentinel
    finally:
        # Keep the global registry clean for other tests.
        from cognis_indexer.registry import _EMBEDDER_FACTORIES

        _EMBEDDER_FACTORIES.pop("_test_custom", None)
