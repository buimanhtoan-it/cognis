"""Unit tests for Reciprocal Rank Fusion (cognis_retrieval.fusion)."""

from __future__ import annotations

import pytest
from cognis_retrieval.base import Hit
from cognis_retrieval.fusion import (
    DEFAULT_RRF_K,
    fuse_rankings,
    reciprocal_rank_fusion,
)


def _hit(sid: str, score: float, layer: str) -> Hit:
    return Hit(symbol_id=sid, score=score, layer=layer, reason="t")


@pytest.mark.unit
class TestReciprocalRankFusion:
    def test_empty(self) -> None:
        assert fuse_rankings([]) == []
        assert reciprocal_rank_fusion([], 5) == []

    def test_scale_invariance(self) -> None:
        """A huge-magnitude lexical score must not dominate a top semantic rank.

        Old max-merge would rank 'a' first purely because 1000 > 0.9. RRF ranks
        on position, so a symbol that is rank-1 in both layers wins.
        """
        hits = [
            _hit("a", 1000.0, "lexical"),  # rank 1 lexical only
            _hit("b", 5.0, "lexical"),  # rank 2 lexical
            _hit("b", 0.9, "semantic"),  # rank 1 semantic
            _hit("c", 0.8, "semantic"),  # rank 2 semantic
        ]
        ranked = reciprocal_rank_fusion(hits, 3)
        # 'b' appears rank-2 lexical + rank-1 semantic => highest fused score.
        assert ranked[0] == "b"

    def test_appears_in_both_layers_beats_single_layer(self) -> None:
        hits = [
            _hit("x", 0.9, "lexical"),
            _hit("x", 0.9, "semantic"),
            _hit("y", 1.0, "lexical"),
        ]
        ranked = reciprocal_rank_fusion(hits, 2)
        assert ranked[0] == "x"  # 2 contributions > y's single rank-1

    def test_deterministic_tie_break_by_symbol_id(self) -> None:
        hits = [_hit("b", 0.5, "lexical"), _hit("a", 0.5, "lexical")]
        # equal score -> within-layer order by symbol_id 'a' then 'b';
        # fused scores differ by rank, 'a' (rank1) outranks 'b' (rank2).
        assert reciprocal_rank_fusion(hits, 2) == ["a", "b"]

    def test_k_truncation(self) -> None:
        hits = [_hit(c, 1.0, "lexical") for c in "abcde"]
        assert len(reciprocal_rank_fusion(hits, 3)) == 3
        assert reciprocal_rank_fusion(hits, 0) == []

    def test_score_formula(self) -> None:
        hits = [_hit("a", 1.0, "lexical")]
        fused = fuse_rankings(hits)
        assert fused[0][0] == "a"
        assert fused[0][1] == pytest.approx(1.0 / (DEFAULT_RRF_K + 1))

    def test_invalid_rrf_k(self) -> None:
        with pytest.raises(ValueError):
            fuse_rankings([_hit("a", 1.0, "lexical")], rrf_k=0)
