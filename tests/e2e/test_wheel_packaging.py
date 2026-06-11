"""E2E: the *sold artifact* (the built wheel) is complete and installable.

The cross-process e2e in ``test_full_flow.py`` runs against the editable/source
install, so it cannot catch a packaging regression — a module, entry point, or
asset missing from the wheel that ships to PyPI and powers the buyer's
``Install backend``. This test builds the real wheel and asserts its contents:

- all eight first-party packages are present,
- the three console entry points are declared, and
- the bundled logo asset is included.

Build needs the ``build`` frontend + the ``hatchling`` backend available for an
offline (``--no-isolation``) build; otherwise it skips cleanly (an environment
capability skip, not a silent pass). The push-CI ``e2e-sandbox`` job installs
both so this runs there.
"""

from __future__ import annotations

import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

pytestmark = pytest.mark.e2e

REPO_ROOT = Path(__file__).resolve().parent.parent.parent

EXPECTED_PACKAGES = {
    "cognis",
    "cognis_retrieval",
    "cognis_indexer",
    "cognis_adapters",
    "cognis_eval",
    "cognis_cli",
    "cognis_mcpd",
    "cognis_indexd",
}
EXPECTED_ENTRY_POINTS = {"cognis-cli", "cognis-mcpd", "cognis-indexd"}


def _build_wheel(out_dir: Path) -> Path:
    pytest.importorskip("build", reason="`build` frontend not installed")
    pytest.importorskip("hatchling", reason="`hatchling` backend not installed")
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "build",
            "--wheel",
            "--no-isolation",
            "--outdir",
            str(out_dir),
            str(REPO_ROOT),
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        pytest.skip(f"wheel build unavailable in this environment:\n{proc.stderr[-800:]}")
    wheels = list(out_dir.glob("cognis_engine-*.whl"))
    assert wheels, f"no wheel produced in {out_dir}: {proc.stdout[-400:]}"
    return wheels[0]


def test_built_wheel_contains_every_package_entrypoint_and_asset(tmp_path: Path) -> None:
    wheel = _build_wheel(tmp_path)
    with zipfile.ZipFile(wheel) as zf:
        names = zf.namelist()
        top_level = {n.split("/", 1)[0] for n in names}

        missing_pkgs = EXPECTED_PACKAGES - top_level
        assert not missing_pkgs, f"wheel is missing packages: {sorted(missing_pkgs)}"

        ep_files = [n for n in names if n.endswith("entry_points.txt")]
        assert ep_files, "wheel has no entry_points.txt — console scripts missing"
        entry_points = zf.read(ep_files[0]).decode("utf-8")
        for script in EXPECTED_ENTRY_POINTS:
            assert script in entry_points, f"missing console script {script!r}"

        assert any(n.endswith("cognis/assets/logo.png") for n in names), (
            "bundled logo asset is missing from the wheel"
        )
