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


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Stop benign cross-process ResourceWarnings from flaking the e2e suite.

    The e2e tests spawn real subprocesses (cognis-cli / indexd / mcpd) and drive
    them through async MCP + httpx clients in the host process. On teardown those
    clients (and, on Python 3.14, some asyncio/socket finalizers) can emit
    late ``ResourceWarning``s in the *host* that are unrelated to any product
    bug. The repo-wide ``filterwarnings = error`` plus pytest's unraisable-
    exception plugin turn those into a hard failure — and because they fire
    during GC, the failure is **misattributed** to whichever e2e test happens to
    be running, so a different test "fails" on each full run while every test
    passes in isolation.

    Detecting genuine product resource leaks is the job of the dedicated,
    quantitative guards in ``test_memory.py`` (real-backend RSS + OS-handle
    bounds), not of the blunt warning-as-error mechanism. So for e2e items only
    (unit/integration suites keep the strict filter) we downgrade these two
    warning classes. Scoped by the ``e2e`` marker so nothing else is affected.
    """
    ignore_resource = pytest.mark.filterwarnings("ignore::ResourceWarning")
    ignore_unraisable = pytest.mark.filterwarnings(
        "ignore::pytest.PytestUnraisableExceptionWarning"
    )
    for item in items:
        if item.get_closest_marker("e2e") is not None:
            item.add_marker(ignore_resource)
            item.add_marker(ignore_unraisable)


@pytest.fixture()
def sample_repo(tmp_path: Path) -> Path:
    """A throwaway workspace seeded with a small multi-language repo."""
    repo_root = tmp_path / "workspace"
    repo_root.mkdir()
    write_sample_repo(repo_root)
    return repo_root
