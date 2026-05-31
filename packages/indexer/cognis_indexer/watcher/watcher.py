"""RepoWatcher — cross-platform file watcher using watchdog.

Implements tasks 7.1, 7.3, 7.4 of ``.kiro/specs/cognis/tasks.md``.

Architecture:
- Uses :class:`watchdog.observers.Observer` (inotify on Linux, FSEvents on
  macOS, ReadDirectoryChanges on Windows; falls back to polling when the
  native backend is unavailable via ``watchdog.observers.polling``).
- Runs the watchdog observer in a background thread.
- Converts raw watchdog events to :class:`FileChangeEvent` /
  :class:`BranchChangeEvent` via an asyncio-aware bridge.
- Applies the 200 ms debounce via :class:`~cognis_indexer.watcher.debounce.Debouncer`.
- Filters paths through :class:`~cognis_indexer.watcher.gitignore.GitignoreFilter`.
- Specially monitors ``.git/HEAD`` and ``.git/packed-refs`` for branch
  changes; those two paths bypass the gitignore filter.

Thread-safety note:
- watchdog callbacks fire from a background thread.
- All asyncio interactions are dispatched via
  :func:`asyncio.get_event_loop().call_soon_threadsafe` so the asyncio event
  loop is never touched from the watchdog thread.
"""

from __future__ import annotations

import asyncio
import threading
from pathlib import Path
from typing import Any

from cognis_indexer.watcher.debounce import DEFAULT_WINDOW_S, Debouncer
from cognis_indexer.watcher.events import BranchChangeEvent, EventKind, FileChangeEvent
from cognis_indexer.watcher.gitignore import GitignoreFilter, parse_head_ref

# Watchdog imports — stubs may or may not be present depending on environment.
try:
    from watchdog.events import (
        DirCreatedEvent,
        DirDeletedEvent,
        DirModifiedEvent,
        DirMovedEvent,
        FileSystemEvent,
        FileSystemEventHandler,
    )
    from watchdog.observers import Observer
except ImportError as exc:  # pragma: no cover
    raise ImportError(
        "watchdog>=4.0 is required for cognis_indexer.watcher. "
        "Install it with: pip install 'cognis[indexer]'"
    ) from exc


# ---------------------------------------------------------------------------
# Type alias for events emitted into the asyncio queue.
# ---------------------------------------------------------------------------

WatcherEvent = FileChangeEvent | BranchChangeEvent

# Git-internal paths that trigger branch detection (relative, forward slashes).
_GIT_HEAD = ".git/HEAD"
_GIT_PACKED_REFS = ".git/packed-refs"
_INDEX_STATUS_PREFIX = ".cognis/indexd-status.json"


# ---------------------------------------------------------------------------
# Internal watchdog event handler
# ---------------------------------------------------------------------------


class _RepoEventHandler(FileSystemEventHandler):
    """Bridge between watchdog's thread-based callbacks and the async pipeline.

    Converts raw :class:`watchdog.events.FileSystemEvent` objects to
    :class:`FileChangeEvent` / :class:`BranchChangeEvent` and schedules them
    on the asyncio event loop.
    """

    def __init__(
        self,
        repo_root: Path,
        gitignore_filter: GitignoreFilter,
        loop: asyncio.AbstractEventLoop,
        debouncer: Debouncer,
        queue: asyncio.Queue[WatcherEvent],
    ) -> None:
        super().__init__()
        self._repo_root = repo_root
        self._filter = gitignore_filter
        self._loop = loop
        self._debouncer = debouncer
        self._queue = queue
        # Track last observed HEAD to compute old_ref.
        self._last_head: str | None = self._read_head()

    # ------------------------------------------------------------------
    # watchdog callbacks (called from watchdog thread)
    # ------------------------------------------------------------------

    def on_created(self, event: FileSystemEvent) -> None:
        self._handle(event, "created")

    def on_modified(self, event: FileSystemEvent) -> None:
        self._handle(event, "modified")

    def on_deleted(self, event: FileSystemEvent) -> None:
        self._handle(event, "deleted")

    def on_moved(self, event: FileSystemEvent) -> None:
        # For moves, use the destination path (what will exist afterward).
        self._handle(event, "moved")

    # ------------------------------------------------------------------
    # Internal dispatch helpers
    # ------------------------------------------------------------------

    def _handle(self, event: FileSystemEvent, kind: EventKind) -> None:
        """Classify the event and dispatch to the appropriate handler."""
        # watchdog uses os.path separators; normalise to forward slashes.
        src_path = str(getattr(event, "src_path", "") or "")
        rel = self._make_relative(src_path)

        # Branch change detection: bypass gitignore filter.
        if rel in (_GIT_HEAD, _GIT_PACKED_REFS):
            self._loop.call_soon_threadsafe(self._loop.create_task, self._emit_branch_change())
            return

        # Skip everything else under .git/.
        if rel.startswith(".git/") or rel == ".git":
            return

        # The daemon writes a small status file for IDEs; indexing that file
        # would create a feedback loop.
        if rel.startswith(_INDEX_STATUS_PREFIX):
            return

        # Skip directories — we only emit events for files.
        if isinstance(event, (DirCreatedEvent, DirDeletedEvent, DirModifiedEvent, DirMovedEvent)):
            return

        # Apply gitignore + config.repo.ignore filter.
        if self._filter.is_ignored(rel):
            return

        # Schedule debounced emission on the asyncio loop.
        self._loop.call_soon_threadsafe(self._loop.create_task, self._debounce(rel, kind))

    async def _debounce(self, path: str, kind: EventKind) -> None:
        await self._debouncer.push(path, kind)

    async def _emit_branch_change(self) -> None:
        old_ref = self._last_head
        new_ref = self._read_head()
        if new_ref is None or new_ref == old_ref:
            return
        self._last_head = new_ref
        await self._queue.put(
            BranchChangeEvent(
                old_ref=old_ref,
                new_ref=new_ref,
            )
        )

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _make_relative(self, abs_path: str) -> str:
        """Convert an absolute path to a repo-relative forward-slash path."""
        try:
            rel = Path(abs_path).relative_to(self._repo_root)
            return rel.as_posix()
        except ValueError:
            # Path is outside repo root — shouldn't happen in practice.
            return abs_path.replace("\\", "/")

    def _read_head(self) -> str | None:
        """Read and parse ``.git/HEAD``. Returns ``None`` if the file doesn't exist."""
        head_path = self._repo_root / ".git" / "HEAD"
        if not head_path.is_file():
            return None
        try:
            content = head_path.read_text(encoding="utf-8")
            return parse_head_ref(content)
        except OSError:
            return None


# ---------------------------------------------------------------------------
# RepoWatcher
# ---------------------------------------------------------------------------


class RepoWatcher:
    """Cross-platform watcher for a git repository.

    Emits :class:`FileChangeEvent` and :class:`BranchChangeEvent` into an
    :class:`asyncio.Queue`.

    Args:
        repo_root:   Absolute path to the repository root.
        config:      Cognis configuration (used for ``repo.ignore`` patterns).
        queue:       asyncio queue that receives :data:`WatcherEvent` objects.
        window_s:    Debounce window in seconds (default 200 ms).

    Example::

        queue: asyncio.Queue[WatcherEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root="/path/to/repo", config=cfg, queue=queue)
        await watcher.start()
        # ... process events from queue ...
        await watcher.stop()
    """

    def __init__(
        self,
        repo_root: str | Path,
        config: object,  # cognis.config.Config — kept as `object` to avoid circular import
        queue: asyncio.Queue[WatcherEvent],
        window_s: float = DEFAULT_WINDOW_S,
    ) -> None:
        self._repo_root = Path(repo_root).resolve()
        self._config = config
        self._queue = queue
        self._window_s = window_s
        self._observer: Any = None
        self._handler: _RepoEventHandler | None = None
        self._debouncer: Debouncer | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._lock = threading.Lock()

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        """Start the watchdog observer and begin emitting events."""
        with self._lock:
            if self._observer is not None:
                return  # Already running.

            self._loop = asyncio.get_running_loop()

            # Build gitignore filter.
            extra_patterns = self._extra_patterns()
            gitignore_filter = GitignoreFilter.from_repo(
                self._repo_root,
                extra_patterns=extra_patterns,
            )

            # Debouncer that puts FileChangeEvents into the queue.
            self._debouncer = Debouncer(
                window_s=self._window_s,
                callback=self._queue.put_nowait,
            )

            # Build the watchdog event handler.
            self._handler = _RepoEventHandler(
                repo_root=self._repo_root,
                gitignore_filter=gitignore_filter,
                loop=self._loop,
                debouncer=self._debouncer,
                queue=self._queue,
            )

            # Start the observer.
            self._observer = Observer()
            self._observer.schedule(
                self._handler,
                str(self._repo_root),
                recursive=True,
            )
            self._observer.start()

    async def stop(self) -> None:
        """Stop the watchdog observer and flush remaining debounced events."""
        with self._lock:
            observer = self._observer
            debouncer = self._debouncer
            self._observer = None
            self._debouncer = None
            self._handler = None

        if debouncer is not None:
            await debouncer.flush()

        if observer is not None:
            observer.stop()
            # Join in a thread-pool executor so we don't block the event loop.
            loop = asyncio.get_running_loop()
            await loop.run_in_executor(None, observer.join)

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _extra_patterns(self) -> list[str]:
        """Extract ``config.repo.ignore`` patterns if available."""
        try:
            # Access via attribute path; avoids importing cognis.config here.
            repo_cfg = getattr(self._config, "repo", None)
            ignore: object = getattr(repo_cfg, "ignore", None)
            if isinstance(ignore, list):
                return [str(p) for p in ignore]
        except AttributeError:
            pass
        return []

    def pending_paths(self, limit: int | None = None) -> list[str]:
        """Return a stable snapshot of paths still inside the debounce window."""
        debouncer = self._debouncer
        if debouncer is None:
            return []
        return debouncer.pending_paths(limit)

    def pending_count(self) -> int:
        """Return how many paths are still buffered by the debouncer."""
        debouncer = self._debouncer
        if debouncer is None:
            return 0
        return debouncer.pending_count()


__all__ = [
    "RepoWatcher",
    "WatcherEvent",
]
