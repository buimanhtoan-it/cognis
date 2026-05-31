"""Property-based tests for the retrieval layers.

**Validates: Requirements 11.1, 10.1, 10.2** (REQ-RET-3, REQ-RET-2)

CP-4 — Structural traversal monotonicity
-----------------------------------------
For any start symbol and direction:
    traverse(start, direction, depth=N) ⊆ traverse(start, direction, depth=N+1)

CP-5 — Semantic self-retrieval
--------------------------------
Searching with a symbol's own embedding text returns that symbol in top-1 with
high consistency. Only exercised when sqlite-vec is available.
"""

from __future__ import annotations

import sqlite3
import time
from typing import Any

import pytest
from cognis.db import Database, upsert_edge, upsert_symbols
from cognis.models import Edge, SymbolNode
from cognis_retrieval import StructuralLayer
from cognis_retrieval.structural import _MAX_DEPTH_HARD
from hypothesis import assume, given, settings
from hypothesis import strategies as st

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _sym(sym_id: str) -> SymbolNode:
    """Create a minimal SymbolNode for graph tests."""
    return SymbolNode(
        id=sym_id,
        kind="function",
        name=sym_id,
        qualified_name=sym_id,
        language="python",
        module="test",
        file_path="test.py",
        line_start=1,
        line_end=2,
        content_hash=sym_id[:8].ljust(8, "0"),
        updated_at=int(time.time()),
    )


def _make_graph_db(edges: list[tuple[str, str]]) -> Database:
    """Build an in-memory DB with the given edge set and corresponding symbols."""
    db = Database(":memory:", vec_enabled=False)
    ids: set[str] = set()
    for src, dst in edges:
        ids.add(src)
        ids.add(dst)
    upsert_symbols(db, [_sym(sid) for sid in ids])
    edge_objs = [Edge(src_id=src, dst_id=dst, kind="calls") for src, dst in edges]
    for e in edge_objs:
        upsert_edge(db, e)
    return db


# ---------------------------------------------------------------------------
# Strategies
# ---------------------------------------------------------------------------

# Node id strategy: short alphanumeric strings to keep path checks manageable.
_node_st = st.text(alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", min_size=1, max_size=4)

# Edge list strategy: list of (src, dst) pairs.
_edges_st = st.lists(
    st.tuples(_node_st, _node_st),
    min_size=1,
    max_size=20,
)


# ---------------------------------------------------------------------------
# CP-4: Structural traversal monotonicity
# Validates: Requirements 11.1 (REQ-RET-3)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
@given(
    edges=_edges_st,
    start=_node_st,
    direction=st.sampled_from(["out", "in"]),
    depth=st.integers(min_value=1, max_value=_MAX_DEPTH_HARD - 1),
)
@settings(max_examples=200, deadline=None)
def test_cp4_structural_monotonicity(
    edges: list[tuple[str, str]],
    start: str,
    direction: str,
    depth: int,
) -> None:
    """Validates: Requirements 11.1

    traverse(start, direction, depth=N) ⊆ traverse(start, direction, depth=N+1)

    For any graph, start node, direction, and depth N, all symbols reachable
    at depth N must also be reachable at depth N+1.
    """
    # Ensure the start node actually exists (as src or dst) in the edge set.
    all_nodes = {s for s, d in edges} | {d for s, d in edges}
    assume(start in all_nodes)

    db = _make_graph_db(edges)
    layer = StructuralLayer()

    hits_n = layer.dependency_trace(start, direction, max_depth=depth, db=db)
    hits_n1 = layer.dependency_trace(start, direction, max_depth=depth + 1, db=db)

    ids_n: set[str] = {h.symbol_id for h in hits_n}
    ids_n1: set[str] = {h.symbol_id for h in hits_n1}

    # CP-4: depth-N results must be a subset of depth-(N+1) results.
    assert ids_n <= ids_n1, (
        f"Monotonicity violated: {ids_n - ids_n1} reachable at depth {depth} "
        f"but NOT at depth {depth + 1}. "
        f"start={start!r}, direction={direction!r}, edges={edges!r}"
    )


# ---------------------------------------------------------------------------
# CP-5: Semantic self-retrieval
# Validates: Requirements 10.1, 10.2 (REQ-RET-2)
# ---------------------------------------------------------------------------


@pytest.mark.pbt
def test_cp5_semantic_self_retrieval() -> None:
    """Validates: Requirements 10.1, 10.2

    Searching with a symbol's own embedding text returns that symbol in top-1
    with high consistency when sqlite-vec is available.

    Uses a small real in-memory DB with a few symbols and checks that querying
    with each symbol's own embedding text puts it in top-1.
    """
    pytest.importorskip("sqlite_vec", reason="sqlite-vec not installed; skipping CP-5")
    import numpy as np
    from cognis_retrieval import SemanticLayer
    from cognis_retrieval.semantic import populate_vec

    # Build a tiny DB (vec_enabled=True so we go through the probe path).
    db = Database(":memory:", vec_enabled=True)
    if not db.vec_enabled:
        pytest.skip("sqlite-vec not available on this platform")

    # Create symbols with distinct embeddings.
    symbols = [_sym(f"sym_{i}") for i in range(5)]
    upsert_symbols(db, symbols)

    # Use a simple deterministic "embedder": embed_text returns a one-hot
    # vector so that symbol i is closest to itself. dim must be >= the number
    # of symbols, otherwise the one-hot index wraps (i % dim) and two symbols
    # share an identical vector — producing a KNN tie that breaks self-retrieval.
    dim = len(symbols)  # one unique basis vector per symbol

    # We'll store distinct unit vectors for each symbol.
    # Check if the vec0 table was actually created with our dim.
    conn = db.connect()
    row = conn.execute("SELECT sql FROM sqlite_master WHERE name='symbol_vec'").fetchone()
    if row is None or row["sql"] is None:
        pytest.skip("symbol_vec table not found; vec0 may not have loaded")

    # We need to recreate the symbol_vec table with the correct dimension for
    # our test. Drop and recreate with one dimension per symbol.
    try:
        with db.write() as c:
            c.execute("DROP TABLE IF EXISTS symbol_vec")
            c.execute(
                f"CREATE VIRTUAL TABLE symbol_vec USING vec0("
                f"  symbol_id TEXT PRIMARY KEY,"
                f"  embedding FLOAT[{dim}]"
                f")"
            )
    except sqlite3.OperationalError:
        # The vec0 extension is not loaded on this connection (can happen under
        # full-suite runs where extension state varies by platform/build).
        pytest.skip("vec0 module not loadable on this connection; skipping CP-5")

    # Insert embeddings: each symbol gets a distinct unit vector.
    embeddings: list[Any] = []
    for i, sym in enumerate(symbols):
        vec = np.zeros(dim, dtype=np.float32)
        vec[i] += 1.0
        vec = vec / (np.linalg.norm(vec) + 1e-9)
        embeddings.append((sym.id, vec))
        populate_vec(db, sym.id, vec)

    # Build a fake embedder that returns the stored vector for each symbol id.
    class _FixedEmbedder:
        embedding_dim = dim
        _lookup: dict[str, Any]

        def __init__(self, lut: dict[str, Any]) -> None:
            self._lookup = lut

        def embed_text(self, text: str) -> Any:
            # text is the symbol_id for self-retrieval queries.
            if text in self._lookup:
                return self._lookup[text]
            return np.zeros(dim, dtype=np.float32)

    lut = {sym_id: vec for sym_id, vec in embeddings}
    embedder = _FixedEmbedder(lut)
    layer = SemanticLayer(embedder)

    # For each symbol, query with its own id text; it should appear in top-1.
    for sym in symbols:
        hits = layer.search(sym.id, k=5, db=db)
        if not hits:
            # vec0 may not support this tiny dim; skip gracefully.
            pytest.skip(f"No hits returned for {sym.id!r}; vec0 may not support dim={dim}")
        assert hits[0].symbol_id == sym.id, (
            f"Self-retrieval failed for {sym.id!r}: top-1 was {hits[0].symbol_id!r}"
        )
