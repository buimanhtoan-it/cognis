"""Shared fixtures and guards for the cross-app E2E suite.

These tests spawn real subprocesses (``cognis-cli``, ``cognis-indexd``,
``cognis-mcpd``) and exercise the actual JSON / stdio contracts between apps.
They are marked ``e2e`` and excluded from the default ``make test`` run.

Run with: ``pytest -m e2e``
"""

from __future__ import annotations

from pathlib import Path

import pytest

# E2E indexing needs the tree-sitter grammars; skip the whole suite cleanly on
# a stripped-down install rather than failing with import errors.
pytest.importorskip("tree_sitter_python")
pytest.importorskip("tree_sitter_typescript")

from tests.e2e.harness import write_sample_repo

pytestmark = pytest.mark.e2e


@pytest.fixture()
def sample_repo(tmp_path: Path) -> Path:
    """A throwaway workspace seeded with a small multi-language repo."""
    repo_root = tmp_path / "workspace"
    repo_root.mkdir()
    write_sample_repo(repo_root)
    return repo_root
