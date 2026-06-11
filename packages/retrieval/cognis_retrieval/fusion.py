"""Cross-layer rank fusion for the retrieval mesh.

Each retrieval layer (lexical / semantic / structural / CSAR) emits hits scored
on its **own scale**: lexical scores are BM25/tf-idf magnitudes, semantic scores
are cosine similarities in ``[-1, 1]``. Combining layers by comparing these raw
scores directly (e.g. ``max`` or sum) is *scale-incoherent* — whichever layer
happens to emit larger-magnitude numbers dominates, regardless of relevance.

Reciprocal Rank Fusion (RRF) avoids this by fusing on **ranks**, not scores::

    rrf_score(d) = Σ_layer  1 / (rrf_k + rank_layer(d))

It is parameter-light (the single constant ``rrf_k = 60`` is the standard value
from Cormack et al., not tuned to any cognis benchmark), scale-invariant, and
robust to a layer producing pathological score magnitudes.

Why this is the engine default
------------------------------
On the reproducible objective (PR-derived, structure-blind) benchmark
(``.benchmarks/public/RESULTS.md``), RRF was the strongest fusion across repos
and languages — it beat raw dense KNN and tied/beat BM25 on Recall@10 while
leading MRR, and decisively beat raw PPR/structural diffusion (which floods
high-degree hubs). This module makes RRF a first-class engine primitive so the
live retrieval path fuses coherently instead of comparing BM25 against cosine.

This is EMPIRICALLY SUPPORTED on a finite sample, not a PROVEN universal claim;
the design choice also rests on the scale-incoherence argument above, which is
independent of the sample.
"""

from __future__ import annotations

from cognis_retrieval.base import Hit

__all__ = ["DEFAULT_RRF_K", "fuse_rankings", "reciprocal_rank_fusion"]

DEFAULT_RRF_K: int = 60
"""Standard RRF damping constant (Cormack et al. 2009). Not tuned to cognis."""


def fuse_rankings(hits: list[Hit], *, rrf_k: int = DEFAULT_RRF_K) -> list[tuple[str, float]]:
    """Fuse per-layer hit lists into one RRF-ranked ``(symbol_id, score)`` list.

    Hits are grouped by their ``layer``. Within each layer they are ranked by
    descending score (ties broken by ``symbol_id`` for determinism), and each
    symbol accrues ``1 / (rrf_k + rank)`` from every layer it appears in. The
    result is sorted by descending fused score (ties broken by ``symbol_id``).

    Args:
        hits: Flat list of hits from one or more layers (may repeat a symbol
            across layers; each layer contributes independently).
        rrf_k: RRF damping constant; larger flattens the rank contribution.

    Returns:
        ``(symbol_id, fused_score)`` pairs, best first, one entry per unique
        symbol. Empty when *hits* is empty.

    Raises:
        ValueError: If ``rrf_k`` is not positive.
    """
    if rrf_k <= 0:
        raise ValueError(f"rrf_k must be > 0; got {rrf_k}")
    if not hits:
        return []

    by_layer: dict[str, list[Hit]] = {}
    for hit in hits:
        by_layer.setdefault(hit.layer, []).append(hit)

    fused: dict[str, float] = {}
    for layer_hits in by_layer.values():
        ranked = sorted(layer_hits, key=lambda h: (-h.score, h.symbol_id))
        rank = 0
        seen: set[str] = set()
        for hit in ranked:
            if hit.symbol_id in seen:  # one rank per symbol per layer
                continue
            seen.add(hit.symbol_id)
            rank += 1
            fused[hit.symbol_id] = fused.get(hit.symbol_id, 0.0) + 1.0 / (rrf_k + rank)

    return sorted(fused.items(), key=lambda kv: (-kv[1], kv[0]))


def reciprocal_rank_fusion(hits: list[Hit], k: int, *, rrf_k: int = DEFAULT_RRF_K) -> list[str]:
    """Return the top-*k* ``symbol_id`` after RRF fusion of *hits*.

    Thin convenience wrapper over :func:`fuse_rankings` for callers that only
    need the ranked id list (e.g. the eval/live retrieval path).

    Args:
        hits: Flat list of layer hits.
        k: Maximum number of symbol ids to return (``<= 0`` yields ``[]``).
        rrf_k: RRF damping constant.

    Returns:
        Up to *k* ``symbol_id`` ranked best-first.
    """
    if k <= 0:
        return []
    return [sid for sid, _ in fuse_rankings(hits, rrf_k=rrf_k)[:k]]
