"""Unit tests for the reranking seam (``cognis_retrieval.reranker``).

Confirms the Null-Object default (NoOpReranker) keeps the engine flow unchanged
when reranking is disabled, and that the registry/factory selects backends.
"""

from __future__ import annotations

import pytest
from cognis.config import RerankerConfig
from cognis_retrieval.base import Hit
from cognis_retrieval.reranker import (
    CrossEncoderReranker,
    NoOpReranker,
    Reranker,
    UnknownRerankerBackendError,
    available_reranker_backends,
    build_reranker,
    register_reranker,
)


def _hits(*ids: str) -> list[Hit]:
    return [
        Hit(symbol_id=sid, score=1.0 - i * 0.1, layer="lexical", reason="x")
        for i, sid in enumerate(ids)
    ]


def test_disabled_config_returns_noop() -> None:
    reranker = build_reranker(RerankerConfig(enabled=False))
    assert isinstance(reranker, NoOpReranker)
    assert isinstance(reranker, Reranker)


def test_noop_preserves_order_and_truncates() -> None:
    hits = _hits("a", "b", "c")
    out = NoOpReranker().rerank("q", hits, k=2)
    assert [h.symbol_id for h in out] == ["a", "b"]


def test_noop_zero_k_returns_empty() -> None:
    assert NoOpReranker().rerank("q", _hits("a"), k=0) == []


def test_enabled_local_backend_builds_cross_encoder() -> None:
    reranker = build_reranker(RerankerConfig(enabled=True, backend="local"))
    assert isinstance(reranker, CrossEncoderReranker)
    assert isinstance(reranker, Reranker)


def test_enabled_unknown_backend_raises() -> None:
    class _FakeCfg:
        enabled = True
        backend = "does-not-exist"
        model = "x"

    with pytest.raises(UnknownRerankerBackendError):
        build_reranker(_FakeCfg())  # type: ignore[arg-type]


def test_register_custom_reranker_roundtrip() -> None:
    sentinel = NoOpReranker()

    @register_reranker("_test_rr")
    def _factory(config: object) -> Reranker:
        return sentinel

    class _FakeCfg:
        enabled = True
        backend = "_test_rr"
        model = "x"

    try:
        assert "_test_rr" in available_reranker_backends()
        assert build_reranker(_FakeCfg()) is sentinel  # type: ignore[arg-type]
    finally:
        from cognis_retrieval.reranker import _RERANKER_FACTORIES

        _RERANKER_FACTORIES.pop("_test_rr", None)
