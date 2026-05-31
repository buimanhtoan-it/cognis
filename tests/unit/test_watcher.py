"""Unit tests for the watcher subpackage (task 7).

Covers:
- GitignoreFilter: path matching and non-matching.
- parse_head_ref: parsing HEAD content for branch / detached SHA.
- Debouncer: rapid events on the same path coalesce; different paths fire separately.
- RepoWatcher: starts and stops cleanly with a temp directory.
- BranchChangeEvent emission via _RepoEventHandler._read_head logic.
"""

from __future__ import annotations

import asyncio
import time
from pathlib import Path
from typing import ClassVar

import pytest
from cognis_indexer.watcher.debounce import Debouncer
from cognis_indexer.watcher.events import BranchChangeEvent, FileChangeEvent
from cognis_indexer.watcher.gitignore import GitignoreFilter, parse_head_ref
from cognis_indexer.watcher.watcher import RepoWatcher

# ---------------------------------------------------------------------------
# GitignoreFilter tests
# ---------------------------------------------------------------------------


class TestGitignoreFilter:
    def test_dot_git_always_ignored(self) -> None:
        filt = GitignoreFilter([])
        assert filt.is_ignored(".git/config") is True
        assert filt.is_ignored(".git/HEAD") is True
        assert filt.is_ignored(".git") is True

    def test_src_file_not_ignored_by_default(self) -> None:
        filt = GitignoreFilter([])
        assert filt.is_ignored("src/main.py") is False

    def test_star_extension_pattern(self) -> None:
        filt = GitignoreFilter(["*.pyc"])
        assert filt.is_ignored("some/path/module.pyc") is True
        assert filt.is_ignored("some/path/module.py") is False

    def test_directory_prefix_pattern(self) -> None:
        filt = GitignoreFilter(["node_modules"])
        assert filt.is_ignored("node_modules/react/index.js") is True
        assert filt.is_ignored("src/node_modules_helper.ts") is False

    def test_trailing_slash_pattern_normalised(self) -> None:
        filt = GitignoreFilter(["dist/"])
        assert filt.is_ignored("dist/bundle.js") is True
        assert filt.is_ignored("src/dist.py") is False

    def test_full_path_pattern_with_slash(self) -> None:
        filt = GitignoreFilter(["docs/build"])
        assert filt.is_ignored("docs/build/index.html") is True
        assert filt.is_ignored("docs/readme.md") is False

    def test_blank_and_comment_lines_ignored(self) -> None:
        filt = GitignoreFilter(["", "# comment", "  ", "*.log"])
        assert filt.is_ignored("server.log") is True
        assert filt.is_ignored("server.py") is False

    def test_extra_patterns_applied(self) -> None:
        filt = GitignoreFilter(["*.tmp"])
        assert filt.is_ignored("scratch.tmp") is True

    def test_from_repo_without_gitignore(self, tmp_path: Path) -> None:
        """No .gitignore → filter is empty (only .git/ built-in)."""
        filt = GitignoreFilter.from_repo(tmp_path)
        assert filt.is_ignored("src/app.py") is False
        assert filt.is_ignored(".git/HEAD") is True

    def test_from_repo_reads_gitignore(self, tmp_path: Path) -> None:
        (tmp_path / ".gitignore").write_text("*.log\n__pycache__/\n", encoding="utf-8")
        filt = GitignoreFilter.from_repo(tmp_path)
        assert filt.is_ignored("app.log") is True
        assert filt.is_ignored("src/__pycache__/module.pyc") is True
        assert filt.is_ignored("src/app.py") is False

    def test_from_repo_with_extra_patterns(self, tmp_path: Path) -> None:
        filt = GitignoreFilter.from_repo(tmp_path, extra_patterns=["generated/"])
        assert filt.is_ignored("generated/schema.py") is True
        assert filt.is_ignored("src/generated_helpers.py") is False


# ---------------------------------------------------------------------------
# parse_head_ref tests
# ---------------------------------------------------------------------------


class TestParseHeadRef:
    def test_normal_branch(self) -> None:
        content = "ref: refs/heads/main\n"
        assert parse_head_ref(content) == "main"

    def test_feature_branch(self) -> None:
        content = "ref: refs/heads/feature/my-feature\n"
        assert parse_head_ref(content) == "feature/my-feature"

    def test_detached_head(self) -> None:
        sha = "abc1234def5678" * 2 + "abcd"
        assert parse_head_ref(sha + "\n") == sha

    def test_strips_whitespace(self) -> None:
        assert parse_head_ref("  ref: refs/heads/dev  \n") == "dev"


# ---------------------------------------------------------------------------
# Debouncer tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
class TestDebouncer:
    async def test_single_event_fires_after_window(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=0.05, callback=events.append)
        await d.push("src/app.py", "modified")
        await asyncio.sleep(0.15)
        assert len(events) == 1
        assert events[0].path == "src/app.py"
        assert events[0].kind == "modified"

    async def test_rapid_events_on_same_path_coalesce(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=0.1, callback=events.append)
        await d.push("src/app.py", "modified")
        await d.push("src/app.py", "modified")
        await d.push("src/app.py", "modified")
        await asyncio.sleep(0.3)
        assert len(events) == 1

    async def test_latest_kind_wins_after_coalesce(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=0.1, callback=events.append)
        await d.push("src/app.py", "created")
        await d.push("src/app.py", "modified")
        await d.push("src/app.py", "deleted")
        await asyncio.sleep(0.3)
        assert len(events) == 1
        assert events[0].kind == "deleted"

    async def test_different_paths_fire_separately(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=0.05, callback=events.append)
        await d.push("src/a.py", "modified")
        await d.push("src/b.py", "modified")
        await asyncio.sleep(0.2)
        paths = {e.path for e in events}
        assert paths == {"src/a.py", "src/b.py"}

    async def test_flush_emits_pending_events(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=10.0, callback=events.append)  # very long window
        await d.push("src/app.py", "modified")
        # Window hasn't expired yet:
        assert len(events) == 0
        await d.flush()
        # Flush should immediately emit.
        assert len(events) == 1
        assert events[0].path == "src/app.py"

    async def test_pending_snapshot_tracks_buffered_paths(self) -> None:
        d = Debouncer(window_s=10.0)
        await d.push("src/b.py", "modified")
        await d.push("src/a.py", "modified")
        assert d.pending_count() == 2
        assert d.pending_paths() == ["src/a.py", "src/b.py"]
        assert d.pending_paths(limit=1) == ["src/a.py"]
        await d.flush()
        assert d.pending_count() == 0
        assert d.pending_paths() == []

    async def test_no_double_emit_after_flush(self) -> None:
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=10.0, callback=events.append)
        await d.push("src/app.py", "modified")
        await d.flush()
        await asyncio.sleep(0.05)
        assert len(events) == 1  # Not emitted again.

    async def test_output_count_lte_input_count(self) -> None:
        """Property-lite: output ≤ input."""
        events: list[FileChangeEvent] = []
        d = Debouncer(window_s=0.1, callback=events.append)
        paths = ["a.py", "b.py", "c.py"]
        for _ in range(5):
            for p in paths:
                await d.push(p, "modified")
        await asyncio.sleep(0.4)
        # At most one event per path.
        assert len(events) <= len(paths) * 5
        # In practice, all coalesce to 1 per path.
        assert len(events) <= len(paths)


# ---------------------------------------------------------------------------
# RepoWatcher integration tests (temp dir, no actual file changes)
# ---------------------------------------------------------------------------


class _DummyConfig:
    """Minimal stand-in for cognis.config.Config."""

    class repo:
        ignore: ClassVar[list[str]] = ["*.tmp", "build/"]


@pytest.mark.asyncio
class TestRepoWatcher:
    async def test_start_and_stop_clean(self, tmp_path: Path) -> None:
        """RepoWatcher starts and stops without errors on a temp dir."""
        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue)
        await watcher.start()
        # Brief pause to let watchdog spin up.
        await asyncio.sleep(0.05)
        await watcher.stop()

    async def test_double_stop_is_safe(self, tmp_path: Path) -> None:
        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue)
        await watcher.start()
        await watcher.stop()
        await watcher.stop()  # Second stop should be a no-op.

    async def test_double_start_is_safe(self, tmp_path: Path) -> None:
        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue)
        await watcher.start()
        await watcher.start()  # Second start should be a no-op.
        await watcher.stop()

    async def test_file_change_emitted(self, tmp_path: Path) -> None:
        """Creating a file inside the repo root emits a FileChangeEvent."""
        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue, window_s=0.05)
        await watcher.start()
        await asyncio.sleep(0.1)  # Let watchdog stabilise.

        # Create a file.
        test_file = tmp_path / "hello.py"
        test_file.write_text("print('hi')\n", encoding="utf-8")

        # Wait for debounce + dispatch.
        deadline = time.monotonic() + 2.0
        event = None
        while time.monotonic() < deadline:
            try:
                event = queue.get_nowait()
                break
            except asyncio.QueueEmpty:
                await asyncio.sleep(0.05)

        await watcher.stop()

        assert event is not None, "Expected a FileChangeEvent but queue was empty"
        assert isinstance(event, FileChangeEvent)
        assert event.path == "hello.py"
        assert event.kind in ("created", "modified")

    async def test_ignored_file_not_emitted(self, tmp_path: Path) -> None:
        """Files matching ignore patterns do not produce events."""
        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue, window_s=0.05)
        await watcher.start()
        await asyncio.sleep(0.1)

        # Create a .tmp file — matches _DummyConfig.repo.ignore.
        (tmp_path / "scratch.tmp").write_text("data", encoding="utf-8")

        # Wait briefly — no event should arrive.
        await asyncio.sleep(0.3)
        await watcher.stop()

        assert queue.empty(), "Ignored file should not produce events"

    async def test_branch_change_emitted(self, tmp_path: Path) -> None:
        """Changing .git/HEAD emits a BranchChangeEvent."""
        # Set up a minimal .git directory.
        git_dir = tmp_path / ".git"
        git_dir.mkdir()
        head_file = git_dir / "HEAD"
        head_file.write_text("ref: refs/heads/main\n", encoding="utf-8")

        queue: asyncio.Queue[FileChangeEvent | BranchChangeEvent] = asyncio.Queue()
        watcher = RepoWatcher(repo_root=tmp_path, config=_DummyConfig(), queue=queue, window_s=0.05)
        await watcher.start()
        await asyncio.sleep(0.1)

        # Simulate a branch switch.
        head_file.write_text("ref: refs/heads/feature/new\n", encoding="utf-8")

        # Wait for the event.
        deadline = time.monotonic() + 2.0
        event = None
        while time.monotonic() < deadline:
            try:
                candidate = queue.get_nowait()
                if isinstance(candidate, BranchChangeEvent):
                    event = candidate
                    break
            except asyncio.QueueEmpty:
                await asyncio.sleep(0.05)

        await watcher.stop()

        assert event is not None, "Expected a BranchChangeEvent"
        assert isinstance(event, BranchChangeEvent)
        assert event.new_ref == "feature/new"
        assert event.old_ref == "main"
