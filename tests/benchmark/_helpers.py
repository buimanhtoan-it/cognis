"""Shared helpers for benchmark tests (Task 18.1 / MCP latency).

These utilities stay lightweight and deterministic so CI does not depend on
downloading sentence-transformers or loading a real embedding model.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Any

import numpy as np
from numpy.typing import NDArray


def percentile_ms(samples_ms: list[float], pct: float = 0.95) -> float:
    """Return the *pct* percentile from a list of millisecond samples."""
    if not samples_ms:
        raise ValueError("samples_ms must not be empty")
    ordered = sorted(samples_ms)
    idx = min(int(pct * len(ordered)), len(ordered) - 1)
    return ordered[idx]


def time_call_ms(fn: Callable[[], Any], *, rounds: int = 50) -> list[float]:
    """Run *fn* *rounds* times and return per-call latency in milliseconds."""
    samples: list[float] = []
    for _ in range(rounds):
        t0 = time.perf_counter()
        fn()
        samples.append((time.perf_counter() - t0) * 1000)
    return samples


class CountingEmbedder:
    """Deterministic embedder stub that tracks ``embed_text`` calls.

    Optional ``delay_us`` simulates model inference cost without loading
    sentence-transformers, so semantic LRU-cache benchmarks stay stable in CI.
    """

    embedding_dim: int = 384

    def __init__(self, *, delay_us: float = 50.0) -> None:
        self.delay_us = delay_us
        self.embed_text_calls = 0

    def embed_text(self, text: str) -> NDArray[np.float32]:
        self.embed_text_calls += 1
        if self.delay_us:
            time.sleep(self.delay_us / 1_000_000)
        # Hash the query so different strings produce different vectors.
        seed = sum(ord(c) for c in text) % 997
        vec = np.zeros(self.embedding_dim, dtype=np.float32)
        vec[seed % self.embedding_dim] = 1.0
        return vec

    def embed_batch(self, texts: list[str]) -> NDArray[np.float32]:
        return np.stack([self.embed_text(t) for t in texts], axis=0)
