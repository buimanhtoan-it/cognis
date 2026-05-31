"""Tiny scaffold test that asserts task-1 wiring is sane.

Once task 2.1 lands, the scaffold tests broaden to import cognis.config; until
then this module just validates the package import surface so CI has a green
``pytest`` run from day one.
"""

from __future__ import annotations

import pytest


@pytest.mark.unit
def test_cognis_imports_and_exposes_version() -> None:
    """``cognis`` is importable and exposes a non-empty ``__version__``."""
    import cognis

    assert isinstance(cognis.__version__, str)
    assert cognis.__version__  # non-empty


@pytest.mark.unit
def test_cli_main_returns_zero_on_version_flag(capsys: pytest.CaptureFixture[str]) -> None:
    """The CLI shim exits 0 and prints the version when given ``--version``."""
    from cognis.cli.main import main

    rc = main(["--version"])
    captured = capsys.readouterr()

    assert rc == 0
    assert captured.out.strip()
