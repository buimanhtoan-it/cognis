"""Event dataclasses for the watcher pipeline.

Defines the two event types emitted into the ``asyncio.Queue`` by
:class:`~cognis_indexer.watcher.watcher.RepoWatcher`:

- :class:`FileChangeEvent` — a file was created, modified, deleted, or moved.
- :class:`BranchChangeEvent` — the git HEAD changed (branch switch / detach).

These are frozen dataclasses so they are hashable and immutable once queued.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Literal

# ---------------------------------------------------------------------------
# EventKind
# ---------------------------------------------------------------------------

EventKind = Literal["created", "modified", "deleted", "moved"]
"""The four filesystem change operations that the watcher surfaces."""


# ---------------------------------------------------------------------------
# FileChangeEvent
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class FileChangeEvent:
    """A single (debounced) filesystem change.

    Attributes:
        path:      Repo-relative path using forward slashes, e.g. ``src/auth/jwt.ts``.
        kind:      One of ``"created"``, ``"modified"``, ``"deleted"``, ``"moved"``.
        timestamp: Wall-clock sample taken via :func:`time.monotonic` at the
                   moment the *debounced* event is emitted.
    """

    path: str
    kind: EventKind
    timestamp: float = field(default_factory=time.monotonic)


# ---------------------------------------------------------------------------
# BranchChangeEvent
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BranchChangeEvent:
    """Emitted when ``.git/HEAD`` changes (branch switch or detach).

    Attributes:
        old_ref:   Previous branch/ref name, or ``None`` on first observation.
        new_ref:   Newly active branch/ref name (e.g. ``"main"``, ``"feature/foo"``).
                   For a detached HEAD this is the raw SHA.
        timestamp: Wall-clock sample via :func:`time.monotonic`.
    """

    old_ref: str | None
    new_ref: str
    timestamp: float = field(default_factory=time.monotonic)


__all__ = [
    "BranchChangeEvent",
    "EventKind",
    "FileChangeEvent",
]
