"""File watcher subpackage - debounced, gitignore-aware watchdog integration.

Implements task 7 (7.1-7.5) of ``.kiro/specs/cognis/tasks.md``.

Public surface::

    from cognis_indexer.watcher import (
        EventKind,
        FileChangeEvent,
        BranchChangeEvent,
        GitignoreFilter,
        Debouncer,
        RepoWatcher,
    )
"""

from cognis_indexer.watcher.debounce import Debouncer
from cognis_indexer.watcher.events import BranchChangeEvent, EventKind, FileChangeEvent
from cognis_indexer.watcher.gitignore import GitignoreFilter
from cognis_indexer.watcher.watcher import RepoWatcher

__all__ = [
    "BranchChangeEvent",
    "Debouncer",
    "EventKind",
    "FileChangeEvent",
    "GitignoreFilter",
    "RepoWatcher",
]
