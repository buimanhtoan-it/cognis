"""cognis core namespace.

Subpackages land in later tasks:

- ``cognis.config`` (task 2.1) — Pydantic config loader.
- ``cognis.cli`` (task 2.2) — Click-based ``cognis-cli`` entry points.
- ``cognis.db`` (task 3) — SQLite connection factory + migrations + UCKG CRUD.
- ``cognis.models`` (task 3) — Pydantic data models for the UCKG schema.
- ``cognis.planner`` (task 13) — Cognitive Context Planner.
- ``cognis.capsule`` (task 14) — Capsule composer + JSON Schema.
"""

from __future__ import annotations

import os

if os.name == "nt":
    # Local Windows runs commonly combine pytest workers, MCP threads, numpy,
    # and embedding/model libraries in one process tree. Keep BLAS thread
    # defaults conservative unless the operator explicitly overrides them.
    for _var in (
        "OPENBLAS_NUM_THREADS",
        "OMP_NUM_THREADS",
        "MKL_NUM_THREADS",
        "NUMEXPR_NUM_THREADS",
    ):
        os.environ.setdefault(_var, "1")

from cognis.config import Config

__all__ = ["Config", "__version__"]


def _resolve_version() -> str:
    """Return the engine version from a single source of truth.

    ``pyproject.toml`` (PEP 621) is canonical. In a source or editable checkout
    we read it directly so the repo's declared version always wins (installed
    editable metadata can go stale). For a shipped wheel — where no
    ``pyproject.toml`` sits beside the installed package — we fall back to
    ``importlib.metadata`` (which was populated from that same pyproject at build
    time). Finally ``"0+unknown"`` so import never fails.
    """
    try:
        import tomllib
        from pathlib import Path

        for parent in Path(__file__).resolve().parents:
            candidate = parent / "pyproject.toml"
            if candidate.is_file():
                data = tomllib.loads(candidate.read_text(encoding="utf-8"))
                name = str(data.get("project", {}).get("name", ""))
                if name == "cognis-engine":
                    return str(data["project"]["version"])
    except Exception:
        pass
    try:
        from importlib.metadata import PackageNotFoundError, version

        return version("cognis-engine")
    except PackageNotFoundError:
        return "0+unknown"
    except Exception:
        return "0+unknown"


# Single source of truth for runtime version: PEP 621 metadata in pyproject.
__version__: str = _resolve_version()
