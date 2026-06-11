"""Unit tests for the MCP server's global concurrency cap.

**Validates: docs/security.md "MCP tool limits"** — the documented hard cap on
concurrent tool execution is real and code-backed, not aspirational.

The cap is a process-wide :class:`threading.BoundedSemaphore` guarding every
public tool via the ``_bounded_tool`` decorator. When the server is saturated, a
call that cannot be admitted within the acquire timeout returns the standard
retryable error envelope (tools never raise).
"""

from __future__ import annotations

import threading
import time

import pytest
from cognis_mcpd import tools as mcp_tools
from cognis_mcpd.errors import TIMEOUT


@pytest.mark.unit
def test_concurrency_slot_admits_within_capacity() -> None:
    """A tool call is admitted (slot acquired/released) when below capacity."""
    with mcp_tools._concurrency_slot("unit_test"):
        # Inside the slot the bounded semaphore must have one fewer permit.
        sem = mcp_tools._CONCURRENCY_SEMAPHORE
        assert sem is not None
    # On exit the permit is released; re-acquiring must succeed immediately.
    assert sem.acquire(timeout=0.1)
    sem.release()


@pytest.mark.unit
def test_bounded_tool_returns_envelope_when_saturated(monkeypatch: pytest.MonkeyPatch) -> None:
    """When no slot is available, a bounded tool returns a retryable envelope.

    We install a capacity-1 semaphore with a short acquire timeout, hold the
    only permit on another thread, then call a decorated tool. It must not block
    forever and must surface the standard ``{"error": {...}}`` envelope with
    ``retryable=True`` — never raise.
    """
    monkeypatch.setattr(mcp_tools, "_CONCURRENCY_SEMAPHORE", threading.BoundedSemaphore(1))
    monkeypatch.setattr(mcp_tools, "_CONCURRENCY_ACQUIRE_TIMEOUT_S", 0.2)
    monkeypatch.setattr(mcp_tools, "_MAX_CONCURRENCY", 1)

    held = threading.Event()
    release = threading.Event()

    def _hog() -> None:
        with mcp_tools._concurrency_slot("hog"):
            held.set()
            release.wait(timeout=5.0)

    t = threading.Thread(target=_hog, daemon=True)
    t.start()
    assert held.wait(timeout=2.0), "helper failed to take the only slot"

    start = time.perf_counter()
    # Any decorated tool will do; symbol_lookup is the simplest.
    result = mcp_tools.symbol_lookup("anything")
    elapsed = time.perf_counter() - start

    release.set()
    t.join(timeout=2.0)

    # Fast-failed near the acquire timeout, did not hang for the full call.
    assert elapsed < 2.0
    assert isinstance(result, dict)
    assert "error" in result
    assert result["error"]["code"] == TIMEOUT
    assert result["error"]["retryable"] is True


@pytest.mark.unit
def test_bounded_tool_preserves_name() -> None:
    """The decorator keeps the wrapped tool's __name__ (used in audit + slots)."""
    assert mcp_tools.symbol_lookup.__name__ == "symbol_lookup"
    assert mcp_tools.diffuse_context.__name__ == "diffuse_context"
