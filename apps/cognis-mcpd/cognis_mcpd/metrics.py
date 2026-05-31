"""In-process metrics collection for cognis-mcpd — Task 18.2.

Provides simple counter and histogram implementations using only the Python
standard library. No external dependencies required at MVP.

Phase 2 note: replace these in-memory implementations with ``prometheus_client``
(``prometheus_client.Counter``, ``prometheus_client.Histogram``) and expose a
proper ``/metrics`` HTTP endpoint. See ``docs/observability.md``.

Usage::

    from cognis_mcpd.metrics import METRICS

    # Record a tool call:
    METRICS.tool_calls.inc("symbol_lookup")

    # Record latency:
    with METRICS.tool_latency.time("symbol_lookup"):
        result = symbol_lookup(...)

    # Record a cache hit:
    METRICS.cache_hits.inc("embedding_lru")

    # Get current values (for health endpoint / debug):
    snapshot = METRICS.snapshot()
"""

from __future__ import annotations

import contextlib
import os
import threading
import time
from collections import defaultdict, deque
from collections.abc import Generator
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Counter
# ---------------------------------------------------------------------------


class Counter:
    """Thread-safe monotonically increasing integer counter.

    Phase 2: replace with ``prometheus_client.Counter``.
    """

    def __init__(self, name: str, description: str = "") -> None:
        self.name = name
        self.description = description
        self._lock = threading.Lock()
        self._values: dict[str, int] = defaultdict(int)

    def inc(self, label: str = "", amount: int = 1) -> None:
        """Increment counter by *amount* for the given *label*."""
        with self._lock:
            self._values[label] += amount

    def get(self, label: str = "") -> int:
        """Return current count for *label*."""
        with self._lock:
            return self._values[label]

    def snapshot(self) -> dict[str, int]:
        """Return a copy of all label → count pairs."""
        with self._lock:
            return dict(self._values)


# ---------------------------------------------------------------------------
# Histogram
# ---------------------------------------------------------------------------


class Histogram:
    """Thread-safe latency histogram (in seconds).

    Stores a bounded sliding window of recent observations plus an unbounded
    total count. This keeps long-running local servers from accumulating
    latency samples forever while still preserving useful recent percentiles.

    Phase 2: replace with ``prometheus_client.Histogram``.
    """

    def __init__(
        self,
        name: str,
        description: str = "",
        buckets: tuple[float, ...] = (0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 5.0),
        max_observations: int = int(os.environ.get("COGNIS_METRICS_MAX_OBSERVATIONS", "4096")),
    ) -> None:
        self.name = name
        self.description = description
        self.buckets = buckets
        self._max_observations = max(1, max_observations)
        self._lock = threading.Lock()
        self._observations: dict[str, deque[float]] = {}
        self._counts: dict[str, int] = defaultdict(int)

    def observe(self, value_seconds: float, label: str = "") -> None:
        """Record a latency observation (in seconds)."""
        with self._lock:
            bucket = self._observations.get(label)
            if bucket is None:
                bucket = deque(maxlen=self._max_observations)
                self._observations[label] = bucket
            bucket.append(value_seconds)
            self._counts[label] += 1

    @contextlib.contextmanager
    def time(self, label: str = "") -> Generator[None, None, None]:
        """Context manager that records the wall time of the enclosed block."""
        t0 = time.perf_counter()
        try:
            yield
        finally:
            elapsed = time.perf_counter() - t0
            self.observe(elapsed, label)

    def percentile(self, p: float, label: str = "") -> float | None:
        """Return the *p*-th percentile (0-100) for *label*, or None if no data."""
        with self._lock:
            data = sorted(self._observations.get(label, ()))
        if not data:
            return None
        idx = int((p / 100) * len(data))
        idx = min(idx, len(data) - 1)
        return data[idx]

    def snapshot(self) -> dict[str, dict[str, float | None]]:
        """Return a summary (count, p50, p95, p99, max) for each label."""
        with self._lock:
            labels = {
                label: (list(values), self._counts.get(label, 0))
                for label, values in self._observations.items()
            }
        result: dict[str, dict[str, float | None]] = {}
        for label, (values, total_count) in labels.items():
            if not values:
                continue
            sv = sorted(values)
            n = len(sv)
            result[label or "_total"] = {
                "count": float(total_count),
                "p50_ms": sv[int(0.50 * n)] * 1000 if n else None,
                "p95_ms": sv[int(0.95 * n)] * 1000 if n else None,
                "p99_ms": sv[int(0.99 * n)] * 1000 if n else None,
                "max_ms": sv[-1] * 1000 if n else None,
            }
        return result


# ---------------------------------------------------------------------------
# Gauge
# ---------------------------------------------------------------------------


class Gauge:
    """Thread-safe floating-point gauge (point-in-time value).

    Phase 2: replace with ``prometheus_client.Gauge``.
    """

    def __init__(self, name: str, description: str = "") -> None:
        self.name = name
        self.description = description
        self._lock = threading.Lock()
        self._values: dict[str, float] = defaultdict(float)

    def set(self, value: float, label: str = "") -> None:
        """Set gauge to *value*."""
        with self._lock:
            self._values[label] = value

    def inc(self, amount: float = 1.0, label: str = "") -> None:
        """Increment gauge by *amount*."""
        with self._lock:
            self._values[label] += amount

    def dec(self, amount: float = 1.0, label: str = "") -> None:
        """Decrement gauge by *amount*."""
        with self._lock:
            self._values[label] -= amount

    def get(self, label: str = "") -> float:
        """Return current gauge value."""
        with self._lock:
            return self._values[label]

    def snapshot(self) -> dict[str, float]:
        """Return a copy of all label → value pairs."""
        with self._lock:
            return dict(self._values)


# ---------------------------------------------------------------------------
# Registry — single application-wide metrics collection point
# ---------------------------------------------------------------------------


@dataclass
class _MetricsRegistry:
    """All application metrics for cognis-mcpd.

    Attributes mirror the Prometheus-style naming convention so they can be
    drop-in replaced with ``prometheus_client`` objects at Phase 2.
    """

    # Per-tool call counts: label = tool name.
    tool_calls: Counter = field(
        default_factory=lambda: Counter(
            "cognis_tool_calls_total",
            "Total number of MCP tool calls by tool name.",
        )
    )

    # Per-tool error counts: label = tool name.
    tool_errors: Counter = field(
        default_factory=lambda: Counter(
            "cognis_tool_errors_total",
            "Total number of MCP tool calls that returned an error envelope.",
        )
    )

    # Per-tool latency histograms: label = tool name.
    tool_latency: Histogram = field(
        default_factory=lambda: Histogram(
            "cognis_tool_duration_seconds",
            "MCP tool call latency in seconds.",
        )
    )

    # Cache hit/miss counts: label = cache name (e.g. "embedding_lru").
    cache_hits: Counter = field(
        default_factory=lambda: Counter(
            "cognis_cache_hits_total",
            "Number of cache hits by cache name.",
        )
    )
    cache_misses: Counter = field(
        default_factory=lambda: Counter(
            "cognis_cache_misses_total",
            "Number of cache misses by cache name.",
        )
    )

    # Index size gauge: label = table name (e.g. "symbol", "edge").
    index_size: Gauge = field(
        default_factory=lambda: Gauge(
            "cognis_index_size_rows",
            "Current number of rows in each UCKG table.",
        )
    )

    def snapshot(self) -> dict[str, object]:
        """Return a complete metrics snapshot as a plain dict (for debug/health endpoint)."""
        return {
            "tool_calls": self.tool_calls.snapshot(),
            "tool_errors": self.tool_errors.snapshot(),
            "tool_latency_ms": self.tool_latency.snapshot(),
            "cache_hits": self.cache_hits.snapshot(),
            "cache_misses": self.cache_misses.snapshot(),
            "index_size_rows": self.index_size.snapshot(),
        }


# ---------------------------------------------------------------------------
# Module-level singleton
# ---------------------------------------------------------------------------

#: Application-wide metrics registry.
METRICS: _MetricsRegistry = _MetricsRegistry()

__all__ = ["METRICS", "Counter", "Gauge", "Histogram"]
