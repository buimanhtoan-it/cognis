"""LSP detection and resolver stub for the cognis indexer pipeline.

Implements task 8.2 and 8.3:

- :func:`detect` — scans ``repo_root`` for language-server configuration files
  (``tsconfig.json``, ``pyproject.toml``, ``pyrightconfig.json``, ``go.mod``)
  to determine whether a language server is likely available.
- :class:`LspResolver` — MVP stub that returns an empty list. The real
  ``textDocument/definition`` / ``textDocument/references`` implementation is
  deferred to post-MVP; the wiring and detection logic are in place so it can
  be activated without restructuring the pipeline.

Design reference: *Indexer Pipeline → Resolver* (design.md), design
*Resolved Open Questions → Q-3 LSP integration* ("User-provided. cognis detects
running LSP via standard config files; ships zero LSPs.  Falls back to heuristic
resolver if absent.").
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from cognis_indexer.resolver.base import ResolvedEdge

# ---------------------------------------------------------------------------
# Config-file markers used to detect LSP support per language.
# Ordered by decreasing specificity so the most explicit signal wins.
# ---------------------------------------------------------------------------

_LSP_MARKERS: tuple[str, ...] = (
    # TypeScript / JavaScript
    "tsconfig.json",
    # Python (Pyright / Pylance)
    "pyrightconfig.json",
    "pyproject.toml",
    # Go
    "go.mod",
)


def detect(repo_root: str | os.PathLike[str]) -> bool:
    """Return ``True`` when at least one LSP configuration file is found.

    Scans the top two levels of *repo_root* for the files listed in
    :data:`_LSP_MARKERS`.  A presence of any marker is taken as evidence that a
    language server *could* be running; the pipeline falls back to heuristic
    resolution if none are found.

    Args:
        repo_root: Root directory of the repository to inspect.

    Returns:
        ``True`` if any LSP marker file is found; ``False`` otherwise.  Never
        raises — any I/O error is treated as "not detected".
    """
    root = Path(repo_root)
    try:
        # Check root-level markers first (fast path for well-structured repos).
        for marker in _LSP_MARKERS:
            if (root / marker).is_file():
                return True

        # Check one level deep for monorepo sub-packages (e.g.
        # ``packages/core/pyproject.toml``).
        for child in root.iterdir():
            if not child.is_dir():
                continue
            for marker in _LSP_MARKERS:
                if (child / marker).is_file():
                    return True
    except OSError:
        return False

    return False


class LspResolver:
    """MVP stub for the LSP-backed edge resolver.

    When a running language server is detected by :func:`detect`, the pipeline
    instantiates this resolver.  The current implementation returns an empty
    list — the ``textDocument/definition`` and ``textDocument/references``
    request/response cycle is scheduled for post-MVP once the LSP client
    transport is wired.

    The stub allows the pipeline to be structured correctly (heuristic + LSP
    merge) without blocking on the full implementation.
    """

    def resolve(self, symbols: list[Any]) -> list[ResolvedEdge]:
        """Return resolved edges via LSP (MVP: always empty).

        Args:
            symbols: List of
                :class:`cognis_indexer.parsers.base.ParsedSymbol` instances.
                Unused at MVP.

        Returns:
            Empty list at MVP.
        """
        # TODO(post-MVP): open LSP connection, send textDocument/definition for
        # each callable node, map returned URIs back to SymbolNode ids, return
        # ResolvedEdge list with confidence=1.0 for unambiguous definitions.
        return []


__all__ = ["LspResolver", "detect"]
