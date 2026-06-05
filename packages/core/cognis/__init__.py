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

# Single source of truth for runtime version. PEP 621 metadata in pyproject is
# the canonical tag; this constant tracks it for display in ``cognis-cli health``.
__version__: str = "0.3.0"
