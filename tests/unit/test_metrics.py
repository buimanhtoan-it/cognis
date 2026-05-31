"""Unit tests for in-process MCP metrics helpers."""

from __future__ import annotations

import pytest
from cognis_mcpd.metrics import Histogram


@pytest.mark.unit
def test_histogram_bounds_recent_observation_window() -> None:
    """Histogram should cap retained samples while keeping total count."""
    hist = Histogram("tool_latency", max_observations=3)

    for value in (0.1, 0.2, 0.3, 0.4, 0.5):
        hist.observe(value, "semantic_search")

    snapshot = hist.snapshot()["semantic_search"]

    assert snapshot["count"] == pytest.approx(5.0)
    assert snapshot["max_ms"] == pytest.approx(500.0)
    assert hist.percentile(0, "semantic_search") == pytest.approx(0.3)
    assert hist.percentile(50, "semantic_search") == pytest.approx(0.4)
    assert len(hist._observations["semantic_search"]) == 3


@pytest.mark.unit
def test_histogram_keeps_independent_label_windows() -> None:
    """Each label should maintain its own bounded observation window."""
    hist = Histogram("tool_latency", max_observations=2)

    hist.observe(0.1, "semantic_search")
    hist.observe(0.2, "semantic_search")
    hist.observe(0.3, "semantic_search")
    hist.observe(0.4, "discover_symbols")

    assert list(hist._observations["semantic_search"]) == [0.2, 0.3]
    assert list(hist._observations["discover_symbols"]) == [0.4]
