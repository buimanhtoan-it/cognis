"""Property-based tests for the watcher debounce invariants (CP-12, task 7.5).

**Validates: Requirements 2.1** (REQ-IDX-2: file_changed event handling with
debounce and coalescing).

CP-12 (design.md Correctness Properties §12):

    For any input event stream and debounce window W, output event count ≤
    input event count, and the final state per path equals the last input
    event for that path before the window closed.

Strategy:
- Generate random lists of ``(path, kind, ts)`` triples.
- Apply the :class:`~cognis_indexer.watcher.debounce.Debouncer` to them.
- Assert:
    1. ``len(output) ≤ len(input)``
    2. For each path that appears in the input, the emitted event's ``kind``
       equals the *last* kind seen for that path in the input sequence.
"""

from __future__ import annotations

import asyncio
from collections.abc import Sequence

import pytest
from cognis_indexer.watcher.debounce import Debouncer
from cognis_indexer.watcher.events import EventKind, FileChangeEvent
from hypothesis import given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

# A small set of plausible paths keeps the examples human-readable and
# exercises coalescing without an explosion of unique paths.
_PATHS: list[str] = [
    "src/app.py",
    "src/utils.py",
    "tests/test_app.py",
    "README.md",
    "pyproject.toml",
]

_KIND_ST: st.SearchStrategy[EventKind] = st.sampled_from(
    ["created", "modified", "deleted", "moved"]
)

_EVENT_ST: st.SearchStrategy[tuple[str, EventKind]] = st.tuples(
    st.sampled_from(_PATHS),
    _KIND_ST,
)

_EVENT_STREAM: st.SearchStrategy[list[tuple[str, EventKind]]] = st.lists(
    _EVENT_ST,
    min_size=1,
    max_size=50,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


async def _run_debouncer(
    events: Sequence[tuple[str, EventKind]],
    window_s: float,
) -> list[FileChangeEvent]:
    """Feed *events* into a Debouncer and collect the output after the window."""
    output: list[FileChangeEvent] = []

    def _cb(ev: FileChangeEvent) -> None:
        output.append(ev)

    d = Debouncer(window_s=window_s, callback=_cb)
    for path, kind in events:
        await d.push(path, kind)

    # Wait long enough for the window to expire.
    await asyncio.sleep(window_s * 3 + 0.05)
    return output


def _last_kind_per_path(events: Sequence[tuple[str, EventKind]]) -> dict[str, EventKind]:
    """Return the last ``kind`` seen for each path in *events*."""
    result: dict[str, EventKind] = {}
    for path, kind in events:
        result[path] = kind
    return result


# ---------------------------------------------------------------------------
# Properties
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@settings(max_examples=100, deadline=5000)
@given(events=_EVENT_STREAM)
def test_cp12_output_count_lte_input_count(events: list[tuple[str, EventKind]]) -> None:
    """**Validates: Requirements 2.1** (CP-12, debounce output ≤ input count).

    For any event stream, the debouncer emits no more events than it receives.
    """
    # Use a very short window so tests complete quickly.
    window_s = 0.02
    output = asyncio.run(_run_debouncer(events, window_s))

    assert len(output) <= len(events), (
        f"Output count {len(output)} > input count {len(events)}. "
        f"Events: {events!r}. Output: {output!r}"
    )


@pytest.mark.pbt
@settings(max_examples=100, deadline=5000)
@given(events=_EVENT_STREAM)
def test_cp12_final_state_per_path_correct(events: list[tuple[str, EventKind]]) -> None:
    """**Validates: Requirements 2.1** (CP-12, final state per path = last kind seen).

    After the debounce window closes, each emitted event's ``kind`` must equal
    the last kind seen for that path in the input stream.
    """
    window_s = 0.02
    output = asyncio.run(_run_debouncer(events, window_s))

    expected_last = _last_kind_per_path(events)
    emitted_by_path: dict[str, EventKind] = {}
    for ev in output:
        emitted_by_path[ev.path] = ev.kind

    for path, last_kind in expected_last.items():
        if path in emitted_by_path:
            assert emitted_by_path[path] == last_kind, (
                f"Path {path!r}: expected kind {last_kind!r}, "
                f"got {emitted_by_path[path]!r}. "
                f"Input events: {events!r}"
            )


@pytest.mark.pbt
@settings(max_examples=100, deadline=5000)
@given(events=_EVENT_STREAM)
def test_cp12_at_most_one_event_per_path(events: list[tuple[str, EventKind]]) -> None:
    """**Validates: Requirements 2.1** (CP-12, each path emitted at most once per window).

    Within a single debounce window, a path must produce at most one output event.
    """
    window_s = 0.02
    output = asyncio.run(_run_debouncer(events, window_s))

    path_counts: dict[str, int] = {}
    for ev in output:
        path_counts[ev.path] = path_counts.get(ev.path, 0) + 1

    for path, count in path_counts.items():
        assert count == 1, (
            f"Path {path!r} emitted {count} times (expected 1). Input events: {events!r}"
        )


@pytest.mark.pbt
@settings(max_examples=50, deadline=5000)
@given(
    events=_EVENT_STREAM,
    window_ms=st.integers(min_value=10, max_value=100),
)
def test_cp12_invariant_holds_across_window_sizes(
    events: list[tuple[str, EventKind]], window_ms: int
) -> None:
    """**Validates: Requirements 2.1** (CP-12, invariants hold for any window size).

    The output-count ≤ input-count and correct-final-state invariants must
    hold regardless of the debounce window duration.
    """
    window_s = window_ms / 1000.0
    output = asyncio.run(_run_debouncer(events, window_s))

    # Invariant 1: output ≤ input.
    assert len(output) <= len(events)

    # Invariant 2: final state per path = last kind seen.
    expected_last = _last_kind_per_path(events)
    emitted_by_path: dict[str, EventKind] = {}
    for ev in output:
        emitted_by_path[ev.path] = ev.kind

    for path, last_kind in expected_last.items():
        if path in emitted_by_path:
            assert emitted_by_path[path] == last_kind
