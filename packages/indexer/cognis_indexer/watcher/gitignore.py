"""Gitignore-style path filter.

A lightweight, pure-Python (no external dependency) implementation of
`.gitignore` pattern matching suitable for the watcher's filtering needs.

Design constraints
------------------
- Only parses the **repo-root** ``.gitignore`` at MVP (multi-level gitignore
  support is Phase 2).
- Uses :mod:`fnmatch` for glob matching — sufficient for the common patterns
  (``*.pyc``, ``node_modules/``, ``dist/``).
- Paths passed to :meth:`GitignoreFilter.is_ignored` must be **repo-relative**
  and use forward slashes (as stored in :attr:`.FileChangeEvent.path`).

Usage::

    filt = GitignoreFilter.from_repo(
        repo_root="/path/to/repo",
        extra_patterns=["generated/", "*.tmp"],
    )
    filt.is_ignored("src/main.py")  # False
    filt.is_ignored("node_modules/react")  # True
    filt.is_ignored(".git/config")  # True  (always)
"""

from __future__ import annotations

import fnmatch
import re
from pathlib import Path


class GitignoreFilter:
    """Predicate that returns ``True`` for paths that should be ignored.

    Patterns follow a simplified subset of .gitignore semantics:

    - Blank lines and lines starting with ``#`` are skipped.
    - Trailing ``/`` is stripped (directory marker — we treat as a prefix).
    - ``!`` negations are *not* supported at MVP.
    - A pattern containing ``/`` (after stripping trailing ``/``) is matched
      against the full repo-relative path; otherwise it is matched against
      the last component (filename) only.
    """

    # Internal: pre-compiled (pattern_string, match_full_path) pairs.
    _patterns: list[tuple[str, bool]]

    def __init__(self, patterns: list[str]) -> None:
        """Build a filter from a list of raw pattern strings.

        Args:
            patterns: Raw lines from a ``.gitignore`` file or a config list.
                      Comments and blank lines are filtered out automatically.
        """
        compiled: list[tuple[str, bool]] = []
        for raw in patterns:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            # Strip trailing slash (directory indicator).
            if line.endswith("/"):
                line = line.rstrip("/")
            # Decide whether to match against the full path or just filename.
            match_full = "/" in line
            compiled.append((line, match_full))
        self._patterns = compiled

    # ------------------------------------------------------------------
    # Factory
    # ------------------------------------------------------------------

    @classmethod
    def from_repo(
        cls,
        repo_root: str | Path,
        extra_patterns: list[str] | None = None,
    ) -> GitignoreFilter:
        """Load patterns from ``<repo_root>/.gitignore`` plus any extras.

        If ``.gitignore`` does not exist, only ``extra_patterns`` are used.

        Args:
            repo_root:       Repository root directory.
            extra_patterns:  Additional patterns (e.g. ``config.repo.ignore``).
        """
        gitignore_path = Path(repo_root) / ".gitignore"
        patterns: list[str] = []
        if gitignore_path.is_file():
            patterns.extend(gitignore_path.read_text(encoding="utf-8").splitlines())
        if extra_patterns:
            patterns.extend(extra_patterns)
        return cls(patterns)

    # ------------------------------------------------------------------
    # Predicate
    # ------------------------------------------------------------------

    def is_ignored(self, repo_relative_path: str) -> bool:
        """Return ``True`` if *repo_relative_path* should be ignored.

        ``.git/`` and everything under it is **always** ignored (except the
        specific git-internal paths the branch-detection watcher needs; that
        filtering is done in :class:`~cognis_indexer.watcher.watcher.RepoWatcher`
        before calling this method).

        Args:
            repo_relative_path: Forward-slash path relative to repo root,
                                 e.g. ``"src/auth/jwt.ts"``.
        """
        # Normalise: use forward slashes, strip a leading "./" prefix only.
        normalized = repo_relative_path.replace("\\", "/")
        if normalized.startswith("./"):
            normalized = normalized[2:]

        # .git/ is always ignored.
        if normalized == ".git" or normalized.startswith(".git/"):
            return True

        parts = normalized.split("/")
        filename = parts[-1]

        for pattern, match_full in self._patterns:
            if match_full:
                # Match pattern against full path.
                if fnmatch.fnmatch(normalized, pattern):
                    return True
                # Match if the path *starts with* the pattern as a directory prefix
                # (e.g. pattern "docs/build" matches "docs/build/index.html").
                if normalized.startswith(pattern + "/") or normalized == pattern:
                    return True
                # Also check each sub-path suffix (for anchored-below-root patterns).
                for i in range(len(parts)):
                    sub = "/".join(parts[i:])
                    if fnmatch.fnmatch(sub, pattern):
                        return True
                    if sub.startswith(pattern + "/") or sub == pattern:
                        return True
            else:
                # Pattern has no slash → match against filename or any component.
                if fnmatch.fnmatch(filename, pattern):
                    return True
                # Also check if any directory component matches (e.g. ``node_modules``).
                for part in parts:
                    if fnmatch.fnmatch(part, pattern):
                        return True

        return False

    def __repr__(self) -> str:
        return f"GitignoreFilter(patterns={[p for p, _ in self._patterns]!r})"


# ---------------------------------------------------------------------------
# Utility: parse HEAD content
# ---------------------------------------------------------------------------

# Matches "ref: refs/heads/<branch>"
_REF_RE: re.Pattern[str] = re.compile(r"^ref:\s+refs/heads/(.+)$", re.MULTILINE)


def parse_head_ref(head_content: str) -> str:
    """Extract the branch name (or raw SHA) from ``.git/HEAD`` content.

    Returns the branch name (e.g. ``"main"``) for a normal HEAD, or the full
    SHA string for a detached HEAD.
    """
    m = _REF_RE.match(head_content.strip())
    if m:
        return m.group(1).strip()
    # Detached HEAD: the content is the bare SHA.
    return head_content.strip()


__all__ = [
    "GitignoreFilter",
    "parse_head_ref",
]
