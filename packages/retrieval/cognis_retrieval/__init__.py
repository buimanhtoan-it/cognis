"""Retrieval mesh package — lexical, semantic, and structural layers (MVP).

Public API
----------
- :class:`~cognis_retrieval.base.Hit` — retrieval result dataclass.
- :class:`~cognis_retrieval.base.RetrievalLayer` — layer Protocol.
- :class:`~cognis_retrieval.lexical.LexicalLayer` — FTS5 BM25 layer.
- :class:`~cognis_retrieval.semantic.SemanticLayer` — sqlite-vec KNN layer.
- :class:`~cognis_retrieval.structural.StructuralLayer` — recursive CTE layer.
- :class:`~cognis_retrieval.csar.CSARLayer` — spreading-activation (PPR) layer.
- :func:`~cognis_retrieval.query_rewriter.rewrite_query` — query rewriter helper.
- :func:`~cognis_retrieval.lexical.populate_fts` — FTS population helper.
- :func:`~cognis_retrieval.semantic.populate_vec` — vector population helper.

Design reference: *Retrieval Mesh* section of design.md (tasks 12.1-12.3).
CSAR reference: ``docs/csar.md``.
"""

from cognis_retrieval.base import Hit, RetrievalLayer
from cognis_retrieval.lexical import LexicalLayer, populate_fts
from cognis_retrieval.query_rewriter import rewrite_query
from cognis_retrieval.semantic import SemanticLayer, populate_vec
from cognis_retrieval.structural import StructuralLayer

# CSAR depends on numpy (shipped with the ``embed-local`` extra). Import it
# lazily-guarded so environments without numpy can still use the other layers.
try:
    from cognis_retrieval.csar import (
        CodeGraph,
        CSARLayer,
        approximate_ppr_push,
        build_code_graph,
        build_seed_distribution,
        diffuse_seed_hits,
        personalized_pagerank_exact,
        personalized_pagerank_power,
        transition_matrix,
    )

    _CSAR_AVAILABLE = True
except ImportError:  # pragma: no cover - only when numpy is absent
    _CSAR_AVAILABLE = False

__all__ = [
    "Hit",
    "LexicalLayer",
    "RetrievalLayer",
    "SemanticLayer",
    "StructuralLayer",
    "populate_fts",
    "populate_vec",
    "rewrite_query",
]

if _CSAR_AVAILABLE:
    __all__ += [
        "CSARLayer",
        "CodeGraph",
        "approximate_ppr_push",
        "build_code_graph",
        "build_seed_distribution",
        "diffuse_seed_hits",
        "personalized_pagerank_exact",
        "personalized_pagerank_power",
        "transition_matrix",
    ]
