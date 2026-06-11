"""Process-wide lazy singletons for semantic retrieval components.

Repeated ``semantic_search`` and ``retrieve_context_capsule`` calls reuse the
same :class:`~cognis_indexer.embedder.LocalEmbedder` and
``cognis_retrieval.semantic.SemanticLayer`` instances instead of re-loading
``sentence-transformers`` or resetting the query-embedding LRU on every call.
"""

from __future__ import annotations

import logging
import os
import threading
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from cognis_indexer.embedder import Embedder

logger = logging.getLogger(__name__)

_lock = threading.RLock()
_embedder: Embedder | None = None
_embedder_init_error: Exception | None = None
_semantic_layer: Any | None = None
_semantic_layer_init_error: Exception | None = None


def _resolve_repo_root() -> str | None:
    """Best-effort repo root for config lookup (mirrors tools._repo_root_for_filters)."""
    raw_root = os.environ.get("COGNIS_REPO_ROOT")
    if raw_root:
        return os.path.abspath(raw_root)
    db_path = os.path.abspath(os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db"))
    candidate = Path(db_path).parent
    if candidate.name == ".cognis":
        candidate = candidate.parent
    if candidate.exists():
        return str(candidate)
    return None


def get_shared_embedder() -> Embedder:
    """Return the process-wide embedder, creating it lazily on first use.

    The backend is selected from ``config.embedder.backend`` via the shared
    :func:`cognis_indexer.registry.build_embedder` factory, so changing the
    configured model takes effect in the MCP server without code edits.

    Raises:
        ImportError: When ``cognis_indexer`` or the backend's optional
            dependency (e.g. ``sentence-transformers``) is unavailable.
        Exception: When embedder construction fails for any other reason.
    """
    global _embedder, _embedder_init_error

    if _embedder is not None:
        return _embedder

    with _lock:
        if _embedder is not None:
            return _embedder
        if _embedder_init_error is not None:
            raise _embedder_init_error
        try:
            from cognis.config import Config
            from cognis_indexer.registry import build_embedder

            repo_root = _resolve_repo_root()
            cfg = Config.load(repo_root) if repo_root is not None else Config.default()
            started = time.perf_counter()
            _embedder = build_embedder(cfg.embedder)
            logger.info(
                "shared embedder ready in %.1fs (backend=%s)",
                time.perf_counter() - started,
                getattr(cfg.embedder, "backend", "?"),
            )
        except Exception as exc:
            _embedder_init_error = exc
            raise
        return _embedder


def reset_shared_embedder_for_tests() -> None:
    """Clear cached semantic components (test helper only)."""
    global _embedder, _embedder_init_error, _semantic_layer, _semantic_layer_init_error
    with _lock:
        _embedder = None
        _embedder_init_error = None
        _semantic_layer = None
        _semantic_layer_init_error = None


def get_shared_semantic_layer() -> Any:
    """Return the process-wide semantic layer using the shared embedder."""
    global _semantic_layer, _semantic_layer_init_error

    if _semantic_layer is not None:
        return _semantic_layer

    with _lock:
        if _semantic_layer is not None:
            return _semantic_layer
        if _semantic_layer_init_error is not None:
            raise _semantic_layer_init_error
        try:
            from cognis_retrieval.semantic import SemanticLayer

            started = time.perf_counter()
            _semantic_layer = SemanticLayer(get_shared_embedder())
            logger.info(
                "semantic layer warm in %.1fs (first semantic query is now hot)",
                time.perf_counter() - started,
            )
        except Exception as exc:
            _semantic_layer_init_error = exc
            raise
        return _semantic_layer


def reset_shared_semantic_layer_for_tests() -> None:
    """Alias for clearing semantic singletons in tests."""
    reset_shared_embedder_for_tests()


__all__ = [
    "get_shared_embedder",
    "get_shared_semantic_layer",
    "reset_shared_embedder_for_tests",
    "reset_shared_semantic_layer_for_tests",
]
