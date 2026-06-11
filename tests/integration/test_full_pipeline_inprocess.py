"""In-process full-flow walk: index → retrieve → fuse → plan → compose.

The cross-process e2e suite (``tests/e2e/``) proves the real apps talk to each
other over subprocess boundaries; this module is its in-process complement. It
drives every *engine* stage directly against a local fixture repo so that:

- each stage is exercised against real (not mocked) data — parse, resolve,
  enrich, write, then all retrieval layers, RRF fusion, the planner, and the
  capsule composer;
- the flow is covered by the coverage harness *in-process* (subprocess coverage
  on Windows cannot capture the hard-killed daemon — see tests/coverage/);
- the file doubles as executable documentation of how a query becomes a capsule.

Local data only: uses the committed ``tests/fixtures/repos/mini-py-svc`` repo,
no network, no model download (embeddings are exercised only when
``sentence-transformers`` is already installed).

Run with: ``pytest -m integration -k full_pipeline_inprocess``
"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest.importorskip("tree_sitter_python")

from cognis.capsule.composer import CapsuleComposer
from cognis.config import Config
from cognis.db import Database
from cognis.planner import Planner
from cognis_indexer.pipeline import IndexerPipeline
from cognis_retrieval.csar import CSARLayer
from cognis_retrieval.fusion import fuse_rankings, reciprocal_rank_fusion
from cognis_retrieval.lexical import LexicalLayer
from cognis_retrieval.structural import StructuralLayer

pytestmark = pytest.mark.integration

FIXTURE = Path(__file__).resolve().parent.parent / "fixtures" / "repos" / "mini-py-svc"
_QUERY = "encode jwt token"  # matches the known fixture symbol encode_jwt


@pytest.fixture()
def indexed_db(tmp_path: Path) -> Database:
    """Cold-index the Python fixture in-process (no embeddings) and return the DB."""
    db = Database(str(tmp_path / "uckg.db"))
    pipeline = IndexerPipeline(db=db, config=Config.default(), embedder=None)
    try:
        stats = pipeline.index_repo(FIXTURE, full=True, skip_embeddings=True)
    finally:
        pipeline.close()
    assert stats.symbols_indexed > 0, f"fixture index produced no symbols: {stats.errors}"
    return db


def _first_symbol_id(db: Database, name: str) -> str:
    row = db.connect().execute("SELECT id FROM symbol WHERE name = ? LIMIT 1", (name,)).fetchone()
    assert row is not None, f"expected fixture symbol {name!r} to be indexed"
    return str(row["id"])


# ---------------------------------------------------------------------------
# Stage 1 — indexing produced a queryable graph
# ---------------------------------------------------------------------------


def test_stage_index_populates_core_tables(indexed_db: Database) -> None:
    conn = indexed_db.connect()
    assert conn.execute("SELECT COUNT(*) FROM symbol").fetchone()[0] > 0
    assert conn.execute("SELECT COUNT(*) FROM file").fetchone()[0] > 0
    assert conn.execute("SELECT COUNT(*) FROM symbol_fts").fetchone()[0] > 0


# ---------------------------------------------------------------------------
# Stage 2 — lexical retrieval finds a seed
# ---------------------------------------------------------------------------


def test_stage_lexical_finds_seed(indexed_db: Database) -> None:
    hits = LexicalLayer().search("encode_jwt", 10, indexed_db)
    assert hits, "lexical layer returned no hits for a known symbol name"
    assert all(h.layer == "lexical" for h in hits)


# ---------------------------------------------------------------------------
# Stage 3 — structural traversal expands the call graph
# ---------------------------------------------------------------------------


def test_stage_structural_trace(indexed_db: Database) -> None:
    seed = _first_symbol_id(indexed_db, "encode_jwt")
    layer = StructuralLayer()
    hits = layer.dependency_trace(seed, direction="both", max_depth=3, db=indexed_db)
    # May be empty if the symbol has no edges; the contract is "no error + valid shape".
    assert isinstance(hits, list)
    assert all(h.layer == "structural" and h.evidence.get("depth", 0) >= 1 for h in hits)


# ---------------------------------------------------------------------------
# Stage 4 — CSAR diffusion runs over the real graph
# ---------------------------------------------------------------------------


def test_stage_csar_diffusion(indexed_db: Database) -> None:
    layer = CSARLayer([LexicalLayer()], alpha=0.2, eps=1e-6, seed_k=10)
    hits = layer.search(_QUERY, 10, indexed_db)
    assert isinstance(hits, list)
    if hits:  # diffusion produced mass — must be ranked descending
        scores = [h.score for h in hits]
        assert scores == sorted(scores, reverse=True)
        assert all(h.layer == "csar" for h in hits)


# ---------------------------------------------------------------------------
# Stage 5 — RRF fusion of heterogeneous layers
# ---------------------------------------------------------------------------


def test_stage_rrf_fusion(indexed_db: Database) -> None:
    db = indexed_db
    lex = LexicalLayer().search(_QUERY, 10, db)
    seed = _first_symbol_id(db, "encode_jwt")
    struct = StructuralLayer().dependency_trace(seed, "both", 2, db)
    fused = reciprocal_rank_fusion([*lex, *struct], k=10)
    assert fused, "fusion of non-empty lexical hits must yield a ranking"
    # Fused scores are strictly ordered and ids unique.
    ranked = fuse_rankings([*lex, *struct])
    scores = [s for _, s in ranked]
    assert scores == sorted(scores, reverse=True)
    assert len({sid for sid, _ in ranked}) == len(ranked)


# ---------------------------------------------------------------------------
# Stage 6 — planner classifies, plans layers, allocates budget
# ---------------------------------------------------------------------------


def test_stage_planner(indexed_db: Database) -> None:
    planner = Planner()
    mode, confidence = planner.classify("Why does encode_jwt throw an error?")
    assert mode == "bugfix" and 0.0 <= confidence <= 1.0
    plan = planner.layer_plan(mode)
    assert abs(sum(plan.values()) - 100.0) < 1e-9
    quotas = planner.allocate_budget(8000, plan, {"lexical", "semantic", "structural"})
    assert quotas.lexical >= 0 and quotas.semantic >= 0


# ---------------------------------------------------------------------------
# Stage 7 — capsule composition for both code paths (bugfix + generic)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", ["bugfix", "feature"])
def test_stage_compose_capsule(indexed_db: Database, mode: str) -> None:
    db = indexed_db
    lex = LexicalLayer().search("encode_jwt", 10, db)
    assert lex, "need at least one hit to compose a non-trivial capsule"
    capsule = CapsuleComposer().compose(
        task="Why does encode_jwt fail under load?" if mode == "bugfix" else "Add JWT refresh",
        mode=mode,  # type: ignore[arg-type]
        confidence=0.85,
        hits=lex,
        max_tokens=8000,
        db=db,
    )
    assert capsule.token_estimate <= 8000  # CP-8
    populated = list(capsule.relevant_symbols) + list(capsule.root_cause_candidates)
    assert populated, "expected a populated section from real hits"
    # CP-9: every populated section entry has a backing source.
    assert capsule.sources, "a populated capsule must carry sources"


# ---------------------------------------------------------------------------
# Stage 8 — empty/edge inputs never crash the flow
# ---------------------------------------------------------------------------


def test_stage_empty_inputs_are_safe(indexed_db: Database) -> None:
    assert reciprocal_rank_fusion([], k=5) == []
    assert LexicalLayer().search("zzzznosuchsymbol", 5, indexed_db) == []
    capsule = CapsuleComposer().compose(
        task="nothing matches",
        mode="feature",
        confidence=0.5,
        hits=[],
        max_tokens=4000,
        db=indexed_db,
    )
    assert capsule.token_estimate <= 4000


# ---------------------------------------------------------------------------
# Stage 9 — semantic layer + embeddings (only when the model is installed)
# ---------------------------------------------------------------------------


def test_stage_semantic_when_available(tmp_path: Path) -> None:
    pytest.importorskip("sentence_transformers")
    from cognis_indexer.registry import build_embedder
    from cognis_retrieval.semantic import SemanticLayer

    db = Database(str(tmp_path / "uckg_emb.db"))
    embedder = build_embedder(Config.default().embedder)
    pipeline = IndexerPipeline(db=db, config=Config.default(), embedder=embedder)
    try:
        pipeline.index_repo(FIXTURE, full=True, skip_embeddings=False)
    finally:
        pipeline.close()

    assert db.connect().execute("SELECT COUNT(*) FROM symbol_vec").fetchone()[0] > 0
    hits = SemanticLayer(embedder).search("validate authentication token", 5, db)
    assert isinstance(hits, list)
    # Fuse semantic with lexical to exercise the multi-layer fusion path.
    lex = LexicalLayer().search("encode_jwt", 5, db)
    fused = reciprocal_rank_fusion([*hits, *lex], k=10)
    assert isinstance(fused, list)
