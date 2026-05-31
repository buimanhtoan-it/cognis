"""CSAR demonstration - empirical evidence for the claims in docs/csar.md.

Run: ``python scripts/csar_demo.py``

Shows, on an isolated in-memory UCKG:

1. **Full-flow recovery**: CSAR surfaces an on-path middleware symbol that pure
   lexical/semantic seeding ranks poorly or misses, because diffusion carries
   relevance along the call graph.
2. **Repo-size-independent cost** (Theorem 5c): forward-push work stays bounded
   by ``1/(alpha*eps)`` as the graph grows 10x and 100x, while a brute-force
   "grep every symbol" baseline grows linearly.
"""

from __future__ import annotations

import os
import tempfile
import time

from cognis.db import Database, upsert_edge, upsert_symbols
from cognis.models import Edge, SymbolNode
from cognis_retrieval import LexicalLayer, populate_fts
from cognis_retrieval.csar import CSARLayer, approximate_ppr_push, build_code_graph


def _sym(sym_id: str, name: str, doc: str) -> SymbolNode:
    return SymbolNode(
        id=sym_id,
        kind="function",
        name=name,
        qualified_name=name,
        language="ts",
        module="m",
        file_path="src/m.ts",
        line_start=1,
        line_end=2,
        docstring=doc,
        content_hash=sym_id[:8].ljust(8, "0"),
        updated_at=int(time.time()),
    )


def _fresh_db() -> Database:
    directory = tempfile.mkdtemp(prefix="csar_demo_")
    return Database(os.path.join(directory, "uckg.db"), vec_enabled=False)


def demo_full_flow_recovery() -> None:
    print("=" * 72)
    print("1. FULL-FLOW RECOVERY: CSAR vs lexical-only")
    print("=" * 72)
    db = _fresh_db()
    symbols = [
        _sym("postLogin", "postLogin", "POST /login handler; validates jwt token."),
        _sym("requireAuth", "requireAuth", "Express middleware guarding routes."),
        _sym("validate", "validate", "Validate a jwt token signature and expiry."),
        _sym("formatCurrency", "formatCurrency", "Formats currency strings."),
    ]
    upsert_symbols(db, symbols)
    populate_fts(db, symbols)
    # Call chain: postLogin -> requireAuth -> validate
    upsert_edge(db, Edge(src_id="postLogin", dst_id="requireAuth", kind="calls"))
    upsert_edge(db, Edge(src_id="requireAuth", dst_id="validate", kind="calls"))

    query = "jwt validate token"
    lexical = LexicalLayer()
    lex_hits = lexical.search(query, 10, db)
    print(f"\nQuery: {query!r}\n")
    print("Lexical-only ranking (today's seed signal):")
    for h in lex_hits:
        print(f"  {h.score:8.4f}  {h.symbol_id}")
    lex_ids = {h.symbol_id for h in lex_hits}
    print(f"  -> 'requireAuth' present in lexical hits? {'requireAuth' in lex_ids}")

    csar = CSARLayer([lexical], alpha=0.2, eps=1e-7, seed_k=10)
    csar_hits = csar.search(query, 10, db)
    print("\nCSAR diffused ranking:")
    for h in csar_hits:
        tag = "seed" if h.evidence.get("seed") else "ON-PATH (recovered via code flow)"
        print(f"  {h.score:8.5f}  {h.symbol_id:14s} [{tag}]")
    csar_ids = {h.symbol_id for h in csar_hits}
    print(f"\n  -> 'requireAuth' recovered by CSAR? {'requireAuth' in csar_ids}")
    print(
        "  -> CSAR connects the full login flow even though the middleware\n"
        "     has no lexical match for the query."
    )


def _ring_db(n: int) -> Database:
    """A ring of n symbols, each calling the next + one chord, for a connected graph."""
    db = _fresh_db()
    syms = [_sym(f"N{i}", f"N{i}", "node") for i in range(n)]
    upsert_symbols(db, syms)
    for i in range(n):
        upsert_edge(db, Edge(src_id=f"N{i}", dst_id=f"N{(i + 1) % n}", kind="calls"))
        upsert_edge(db, Edge(src_id=f"N{i}", dst_id=f"N{(i + 3) % n}", kind="calls"))
    return db


def demo_size_independent_cost() -> None:
    print("\n" + "=" * 72)
    print("2. REPO-SIZE-INDEPENDENT COST (Theorem 5c): work <= 1/(alpha*eps)")
    print("=" * 72)
    alpha, eps = 0.15, 1e-4
    bound = 1.0 / (alpha * eps)
    print(f"\nalpha={alpha}, eps={eps}  =>  theoretical work bound = {bound:,.0f}\n")
    print(
        f"{'symbols (n)':>12} | {'push work sum(d_u)':>18} | "
        f"{'grep baseline (n)':>18} | within bound?"
    )
    print("-" * 76)
    for n in (10, 100, 1000):
        db = _ring_db(n)
        graph = build_code_graph(db)
        push = approximate_ppr_push(graph, {graph.index["N0"]: 1.0}, alpha, eps)
        ok = push.work <= bound + 1e-6
        print(f"{n:>12} | {push.work:>18,.1f} | {n:>18,} | {ok}")
    print(
        "\n  -> CSAR push work is flat as the repo grows 100x; a grep/embed-all\n"
        "     baseline grows linearly with the number of symbols."
    )


if __name__ == "__main__":
    demo_full_flow_recovery()
    demo_size_independent_cost()
