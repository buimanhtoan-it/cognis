"""Per-path debouncer for filesystem events.

Implements task 7.2 of ``.kiro/specs/cognis/tasks.md``.

Design:
- Per-path 200 ms window (configurable for testing).
- Multiple events on the same path within the window collapse into a single
  output event carrying the *latest* ``EventKind``.
- Implemented via ``asyncio.create_task`` + cancellation — **not**
  ``threading.Timer`` — so it integrates cleanly with the async pipeline.

Typical usage (inside a running event loop)::

    async def process():
        queue: asyncio.Queue[FileChangeEvent] = asyncio.Queue()
        debouncer = Debouncer(window_s=0.2, callback=queue.put_nowait)
        await debouncer.push("src/auth.ts", "modified")
        await debouncer.push("src/auth.ts", "modified")  # collapsed
        await asyncio.sleep(0.3)  # window passes
        event = queue.get_nowait()  # one event
        await debouncer.flush()
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from typing import Final

from cognis_indexer.watcher.events import EventKind, FileChangeEvent

# Default debounce window per design spec.
DEFAULT_WINDOW_S: Final[float] = 0.2


class Debouncer:
    """Coalesces rapid filesystem events per path within a sliding window.

    Args:
        window_s:  Debounce window in seconds (default 200 ms).
        callback:  Sync or async callable invoked with a :class:`FileChangeEvent`
                   once the window expires with no further events on that path.
    """

    def __init__(
        self,
        window_s: float = DEFAULT_WINDOW_S,
        callback: Callable[[FileChangeEvent], object] | None = None,
    ) -> None:
        self._window_s = window_s
        self._callback = callback
        # Per-path state: latest kind seen within the current window.
        self._pending_kind: dict[str, EventKind] = {}
        # Per-path handle to the pending asyncio Task (for cancellation).
        self._tasks: dict[str, asyncio.Task[None]] = {}

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def push(self, path: str, kind: EventKind) -> None:
        """Record a new event for *path*, resetting the debounce window.

        If a pending task already exists for *path* it is cancelled and a new
        one is created so the window restarts from *now*.
        """
        # Store / update the latest kind for this path.
        self._pending_kind[path] = kind

        # Cancel any existing timer task for this path.
        existing = self._tasks.get(path)
        if existing is not None and not existing.done():
            existing.cancel()

        # Schedule a new timer.
        self._tasks[path] = asyncio.create_task(self._fire_after(path))

    async def flush(self) -> None:
        """Cancel all pending timers and emit events for each buffered path immediately.

        Useful for clean shutdown so no events are silently dropped.
        """
        paths = list(self._tasks.keys())
        for path in paths:
            task = self._tasks.pop(path, None)
            if task is not None and not task.done():
                task.cancel()
            kind = self._pending_kind.pop(path, None)
            if kind is not None:
                await self._emit(path, kind)

    def pending_paths(self, limit: int | None = None) -> list[str]:
        """Return a stable snapshot of currently buffered paths."""
        paths = sorted(self._pending_kind)
        if limit is None:
            return paths
        return paths[:limit]

    def pending_count(self) -> int:
        """Return the number of buffered paths waiting for debounce expiry."""
        return len(self._pending_kind)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    async def _fire_after(self, path: str) -> None:
        """Wait for the debounce window, then emit."""
        try:
            await asyncio.sleep(self._window_s)
        except asyncio.CancelledError:
            return  # Window restarted; don't emit.
        kind = self._pending_kind.pop(path, None)
        self._tasks.pop(path, None)
        if kind is not None:
            await self._emit(path, kind)

    async def _emit(self, path: str, kind: EventKind) -> None:
        """Build a :class:`FileChangeEvent` and invoke the callback."""
        event = FileChangeEvent(path=path, kind=kind, timestamp=time.monotonic())
        if self._callback is not None:
            result = self._callback(event)
            if asyncio.iscoroutine(result):
                await result


__all__ = [
    "DEFAULT_WINDOW_S",
    "Debouncer",
]
