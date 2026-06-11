"""Full-flow coverage runner.

Runs the whole test suite (unit + integration + property + cross-process e2e)
under coverage, merges in-process and subprocess data, and prints the gap list.

Why a script (not just ``pytest --cov``): the e2e suite spawns the real apps as
subprocesses. Measuring *those* requires ``COVERAGE_PROCESS_START`` plus a
``sitecustomize`` hook on the children's ``PYTHONPATH`` (see
``tests/coverage/``). This script wires that up, runs ``coverage run
--parallel-mode -m pytest``, then ``coverage combine`` + ``report``.

Usage:
    python scripts/coverage_full.py [-m MARKER_EXPR] [--html] [--fail-under N]

Defaults to the markers that exercise the full product flow:
    "unit or integration or pbt or e2e"
(benchmark/eval are excluded — perf + slow nightly, not flow coverage.)
"""

from __future__ import annotations

import argparse
import contextlib
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PYPROJECT = REPO_ROOT / "pyproject.toml"
COV_BOOTSTRAP_DIR = REPO_ROOT / "tests" / "coverage"
DEFAULT_MARKERS = "unit or integration or pbt or e2e"


def _child_env() -> dict[str, str]:
    """Env that makes spawned app subprocesses record coverage too."""
    env = dict(os.environ)
    env["COVERAGE_PROCESS_START"] = str(PYPROJECT)
    # Prepend the bootstrap dir so children import tests/coverage/sitecustomize.
    existing = env.get("PYTHONPATH", "")
    parts = [str(COV_BOOTSTRAP_DIR)] + ([existing] if existing else [])
    env["PYTHONPATH"] = os.pathsep.join(parts)
    return env


def _run(cmd: list[str], env: dict[str, str] | None = None) -> int:
    print(f"\n$ {' '.join(cmd)}", flush=True)
    return subprocess.run(cmd, cwd=str(REPO_ROOT), env=env).returncode


def _clean_coverage_data() -> None:
    for p in REPO_ROOT.glob(".coverage*"):
        with contextlib.suppress(OSError):
            p.unlink()


def main() -> int:
    ap = argparse.ArgumentParser(description="Run the full suite under coverage.")
    ap.add_argument(
        "-m",
        "--markers",
        default=DEFAULT_MARKERS,
        help=f"pytest marker expression (default: {DEFAULT_MARKERS!r})",
    )
    ap.add_argument("--html", action="store_true", help="also write htmlcov/")
    ap.add_argument("--xml", action="store_true", help="also write coverage.xml")
    ap.add_argument(
        "--fail-under", type=float, default=None, help="override the fail_under gate from pyproject"
    )
    ap.add_argument("--pytest-args", default="", help="extra args passed through to pytest")
    args = ap.parse_args()

    py = sys.executable
    env = _child_env()

    _clean_coverage_data()

    # 1. Run the suite under coverage in parallel mode (one data file per process).
    pytest_cmd = [
        py,
        "-m",
        "coverage",
        "run",
        "--parallel-mode",
        "-m",
        "pytest",
        "-m",
        args.markers,
        "-p",
        "no:cacheprovider",
    ]
    if args.pytest_args:
        pytest_cmd.extend(args.pytest_args.split())
    test_rc = _run(pytest_cmd, env=env)
    # Do not abort on test failures: we still want the coverage report so the
    # gaps are visible. We propagate the worst exit code at the end.

    # 2. Merge in-process + subprocess data files.
    _run([py, "-m", "coverage", "combine"], env=env)

    # 3. Reports.
    report_cmd = [py, "-m", "coverage", "report", "--show-missing"]
    if args.fail_under is not None:
        report_cmd += ["--fail-under", str(args.fail_under)]
    report_rc = _run(report_cmd, env=env)

    if args.html:
        _run([py, "-m", "coverage", "html"], env=env)
        print(f"\nHTML report: {REPO_ROOT / 'htmlcov' / 'index.html'}")
    if args.xml:
        _run([py, "-m", "coverage", "xml"], env=env)

    # Exit non-zero if either tests failed or the coverage gate failed.
    return test_rc or report_rc


if __name__ == "__main__":
    raise SystemExit(main())
