"""Live retrieval strategies for the eval harness.

Provides :class:`HybridStrategy`, which runs lexical + semantic retrieval
against a populated UCKG database (same merge logic as the MCP capsule path).
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING

from cognis.db import Database
from cognis.planner import Planner

if TYPE_CHECKING:
    from cognis_retrieval.base import Hit


def _merge_hits(hits: list[Hit], k: int) -> list[str]:
    """Fuse layer hits into a single ranked id list via Reciprocal Rank Fusion.

    Lexical (BM25) and semantic (cosine) scores live on incompatible scales, so
    the previous max-score merge let whichever layer emitted larger magnitudes
    dominate. RRF fuses on *ranks* instead — scale-invariant and the strongest
    fusion on the reproducible objective benchmark (see
    ``.benchmarks/public/RESULTS.md`` and ``cognis_retrieval.fusion``).
    """
    from cognis_retrieval.fusion import reciprocal_rank_fusion

    return reciprocal_rank_fusion(hits, k)


class HybridStrategy:
    """DB-backed hybrid retrieval for golden-set evaluation.

    Uses the planner to classify the query, runs lexical search, and adds
    semantic search when the embedder is available. Structural expansion is
    omitted for eval simplicity (symbol ids in golden set are direct targets).
    """

    name: str = "hybrid"

    def __init__(self, db_path: str | os.PathLike[str] | None = None) -> None:
        path = db_path or os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db")
        self._db = Database(str(path))

    def retrieve(self, query: str, k: int) -> list[str]:
        from cognis_retrieval.lexical import LexicalLayer

        if k < 1:
            return []

        planner = Planner()
        mode, _confidence = planner.classify(query)
        plan = planner.layer_plan(mode)
        available = {"lexical", "semantic"}
        quotas = planner.allocate_budget(max(k * 50, 500), plan, available)

        all_hits: list[Hit] = []

        k_lex = max(1, min(k, quotas.lexical // 50 or k))
        try:
            lex_layer = LexicalLayer()
            all_hits.extend(lex_layer.search(query, k_lex, self._db))
        except Exception:
            pass

        k_sem = max(1, min(k, quotas.semantic // 100 or k))
        try:
            from cognis.config import Config
            from cognis_indexer.registry import build_embedder
            from cognis_retrieval.semantic import SemanticLayer

            embedder = build_embedder(Config.default().embedder)
            sem_layer = SemanticLayer(embedder)
            all_hits.extend(sem_layer.search(query, k_sem, self._db))
        except Exception:
            pass

        return _merge_hits(all_hits, k)


def strategy_from_env() -> HybridStrategy | None:
    """Return :class:`HybridStrategy` when ``COGNIS_DB_PATH`` points at a DB."""
    db_path = os.environ.get("COGNIS_DB_PATH", ".cognis/uckg.db")
    if not os.path.isfile(db_path):
        return None
    return HybridStrategy(db_path)


__all__ = ["HybridStrategy", "strategy_from_env"]
