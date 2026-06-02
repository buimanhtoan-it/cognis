"""Invoke task recipes — Windows-friendly mirror of the Makefile.

Run with ``invoke <task>``. Examples::

    invoke lint
    invoke typecheck
    invoke test
    invoke bench
    invoke eval --queries=.cognis/eval/golden.jsonl
"""

from __future__ import annotations

import contextlib
import sys

from invoke import Context, task

# Strict-typing scope per Task 1 — keep narrow until each package stabilizes.
_MYPY_STRICT_PATHS = "packages/core"


def _py(ctx: Context, *args: str, **kwargs: object) -> object:
    """Run a python module command, falling back to ``python`` on PATH."""
    cmd = " ".join((sys.executable, *args))
    return ctx.run(cmd, **kwargs)  # type: ignore[arg-type]


@task(help={"check_only": "Skip writes; only report problems."})
def lint(ctx: Context, check_only: bool = True) -> None:
    """Run ruff format + lint. Default: check-only (CI-safe)."""
    if check_only:
        _py(ctx, "-m", "ruff", "format", "--check", ".")
        _py(ctx, "-m", "ruff", "check", ".")
    else:
        _py(ctx, "-m", "ruff", "format", ".")
        _py(ctx, "-m", "ruff", "check", "--fix", ".")


@task
def fmt(ctx: Context) -> None:
    """Apply ruff formatting and autofix safe lint issues."""
    lint(ctx, check_only=False)


@task(help={"paths": "Override the strict-typing path set."})
def typecheck(ctx: Context, paths: str = _MYPY_STRICT_PATHS) -> None:
    """Run mypy with strict-mode scope on the given paths."""
    _py(ctx, "-m", "mypy", paths)


@task(help={"args": "Extra args forwarded to pytest."})
def test(ctx: Context, args: str = "") -> None:
    """Run unit + property-based tests (skips benchmark/eval/e2e markers)."""
    _py(
        ctx,
        "-m",
        "pytest",
        '-m "not benchmark and not eval and not e2e"',
        args,
    )


@task(help={"args": "Extra args forwarded to pytest."})
def e2e(ctx: Context, args: str = "") -> None:
    """Run the full cross-app end-to-end suite (CLI + indexd + mcpd over real processes)."""
    _py(ctx, "-m", "pytest", "-m e2e", args)


@task(help={"args": "Extra args forwarded to pytest."})
def bench(ctx: Context, args: str = "") -> None:
    """Run the pytest-benchmark suite only."""
    _py(ctx, "-m", "pytest", "-m benchmark --benchmark-only", args)


@task(help={"args": "Extra args forwarded to cognis-cli eval."})
def run_eval(ctx: Context, args: str = "") -> None:
    """Run the golden-set eval harness via cognis-cli."""
    _py(ctx, "-m", "cognis.cli.main", "eval", args)


@task(help={"package": "Also produce a .vsix after compile."})
def install_extension(ctx: Context, package: bool = False) -> None:
    """Install npm deps and compile apps/cognis-vscode."""
    args = ["scripts/setup_extension.py"]
    if package:
        args.append("--package")
    _py(ctx, *args)


@task(help={"package_extension": "Also produce a .vsix after compile."})
def install_dev(ctx: Context, package_extension: bool = False) -> None:
    """Editable install, dev deps, pre-commit hooks, and extension compile."""
    _py(ctx, "-m", "pip", "install", "-e", ".[indexer,embed-local,vector,tokenizers,mcp]")
    _py(
        ctx,
        "-m",
        "pip",
        "install",
        "--group",
        "dev",
        warn=True,
    )
    ctx.run("pre-commit install", warn=True)
    _py(ctx, "scripts/prepare-branding.py", warn=True)
    install_extension(ctx, package=package_extension)


@task
def clean(ctx: Context) -> None:
    """Remove build artifacts and tooling caches."""
    import pathlib
    import shutil

    for entry in (
        "build",
        "dist",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".hypothesis",
        ".benchmarks",
        "htmlcov",
    ):
        shutil.rmtree(entry, ignore_errors=True)
    root = pathlib.Path(".")
    for cache in root.rglob("__pycache__"):
        shutil.rmtree(cache, ignore_errors=True)
    for cached in root.rglob("*.py[co]"):
        with contextlib.suppress(OSError):
            cached.unlink()
