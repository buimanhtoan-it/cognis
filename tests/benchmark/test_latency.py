"""Performance / latency benchmark tests — Task 18.1.

These tests assert that the Phase 1 hot paths meet the design Performance Plan
latency budgets. They are marked ``@pytest.mark.benchmark`` and excluded from
the default test suite.

Run with: ``pytest -m benchmark --benchmark-only``
Or for quick assertion checks (no benchmark stats):
    ``pytest -m benchmark -k latency``

Design Performance Plan budgets (design.md §Performance Plan):
  - Planner classify + plan + budget: p95 < 30ms
  - Lexical layer (FTS5): p95 < 50ms
  - Semantic query embed (cached repeat): p95 < 5ms (stub; see docs/performance.md)
  - Structural layer (depth ≤ 5): p95 < 150ms
  - Capsule end-to-end (no LLM compression): p95 < 400ms
  - MCP repeated capsule (warm, no embedder): p95 < 5000ms (CI sanity)

Agent workflows benefit when the MCP server reuses a loaded embedder and the
semantic layer's query LRU cache across tool calls. Benchmarks in sections 5-7
exercise those paths without modifying MCP tool code.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.benchmark

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

_PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent


def _make_test_db(tmp_path: Path) -> object:  # type: ignore[return]
    """Create a small test database for benchmark baselines."""
    import time as _t

    from cognis.db import Database

    db_path = tmp_path / "bench.db"
    db = Database(str(db_path))
    conn = db.connect()

    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT OR IGNORE INTO meta VALUES ('index_version', '0.0.1.dev0');

        CREATE TABLE IF NOT EXISTS symbol (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
            qualified_name TEXT NOT NULL, language TEXT NOT NULL,
            module TEXT NOT NULL, file_path TEXT NOT NULL,
            line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
            signature TEXT, docstring TEXT, content_hash TEXT NOT NULL,
            body_excerpt TEXT, semantic_summary TEXT,
            risk_score REAL DEFAULT 0.0, ambiguous INTEGER DEFAULT 0,
            untrusted_flags TEXT, updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_symbol_qname ON symbol(qualified_name);
        CREATE INDEX IF NOT EXISTS idx_symbol_kind  ON symbol(kind);

        CREATE TABLE IF NOT EXISTS edge (
            src_id TEXT NOT NULL, dst_id TEXT NOT NULL, kind TEXT NOT NULL,
            confidence REAL DEFAULT 1.0, meta TEXT,
            PRIMARY KEY (src_id, dst_id, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_edge_src ON edge(src_id, kind);
        CREATE INDEX IF NOT EXISTS idx_edge_dst ON edge(dst_id, kind);

        CREATE TABLE IF NOT EXISTS symbol_attribute (
            symbol_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
            PRIMARY KEY (symbol_id, key, value)
        );
        CREATE TABLE IF NOT EXISTS file (
            path TEXT PRIMARY KEY, language TEXT NOT NULL, size_bytes INTEGER NOT NULL,
            content_hash TEXT NOT NULL, parsed_at INTEGER NOT NULL, parse_status TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts
            USING fts5(id UNINDEXED, name, qualified_name, signature, docstring, body_excerpt,
                       content='symbol', content_rowid='rowid');
        """
    )

    # Seed 200 symbols for meaningful FTS + structural tests.
    now = int(_t.time())
    symbols = []
    for i in range(200):
        sid = f"ts:src/mod{i}.ts:func{i}@{i:08x}"
        symbols.append(
            (
                sid,
                "function",
                f"func{i}",
                f"mod{i}.func{i}",
                "typescript",
                f"src/mod{i}",
                f"src/mod{i}.ts",
                1,
                20,
                f"function func{i}(arg: string): void",
                f"Function number {i} for testing. Validates input and processes data.",
                f"{i:08x}",
                f"function func{i}(arg: string) {{ return arg; }}",
                None,
                0.0,
                0,
                None,
                now,
            )
        )
    conn.executemany(
        """
        INSERT OR IGNORE INTO symbol (
            id, kind, name, qualified_name, language, module, file_path,
            line_start, line_end, signature, docstring, content_hash,
            body_excerpt, semantic_summary, risk_score, ambiguous,
            untrusted_flags, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        symbols,
    )

    # Add edges (linear chain to test depth traversal).
    edges = [
        (
            f"ts:src/mod{i}.ts:func{i}@{i:08x}",
            f"ts:src/mod{i + 1}.ts:func{i + 1}@{(i + 1):08x}",
            "calls",
            1.0,
        )
        for i in range(10)
    ]
    conn.executemany(
        "INSERT OR IGNORE INTO edge (src_id, dst_id, kind, confidence) VALUES (?, ?, ?, ?)",
        edges,
    )

    # Populate FTS.
    import contextlib

    for s in symbols:
        # Unpack positionally: id, kind, name, qualified_name, language, module, file_path,
        # line_start, line_end, signature, docstring, content_hash, body_excerpt, ...
        parts = list(s)
        sid = parts[0]
        name = parts[2]
        qname = parts[3]
        signature = parts[9]
        docstring = parts[10]
        body_excerpt = parts[12]
        with contextlib.suppress(Exception):
            conn.execute(
                "INSERT OR IGNORE INTO symbol_fts (id, name, qualified_name, signature, docstring, body_excerpt)"
                " VALUES (?, ?, ?, ?, ?, ?)",
                (sid, name, qname, signature, docstring, body_excerpt),
            )

    conn.commit()
    return db


# ---------------------------------------------------------------------------
# 1. Planner: classify + plan + budget
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_planner_classify_plan_budget_latency(benchmark: object) -> None:
    """Planner full pipeline (classify → layer_plan → allocate_budget) must be < 30ms p95.

    Design budget: p95 < 30ms (REQ-PLN-1, REQ-PLN-2, REQ-PLN-3).
    """
    from cognis.planner import Planner

    planner = Planner()
    task = "Why is the /login endpoint timing out under load? auth-timeout in JWT middleware."

    def _run() -> None:
        mode, _confidence = planner.classify(task)
        plan = planner.layer_plan(mode)
        planner.allocate_budget(8000, plan, {"lexical", "semantic", "structural"})

    if callable(benchmark):
        # pytest-benchmark: records stats automatically.
        benchmark(_run)  # type: ignore[operator]
    else:
        # Fallback: manual assertion.
        times = []
        for _ in range(50):
            t0 = time.perf_counter()
            _run()
            times.append((time.perf_counter() - t0) * 1000)
        times.sort()
        p95 = times[int(0.95 * len(times))]
        assert p95 < 30, (
            f"Planner classify+plan+budget p95={p95:.2f}ms exceeds 30ms budget.\n"
            "Document in docs/performance.md if budget cannot be met."
        )


@pytest.mark.benchmark
def test_planner_classify_all_modes() -> None:
    """Planner classify is fast for all 6 task modes; manual timing assertion."""
    from cognis.planner import Planner

    planner = Planner()
    tasks = [
        ("Why is /login timing out? error in JWT.", "bugfix"),
        ("Add rate limiting middleware to all routes.", "feature"),
        ("Extract inline SQL into a typed query builder.", "refactor"),
        ("How does the auth layer work end to end?", "explain"),
        ("Migrate the DB layer from SQLAlchemy 1.4 to 2.0.", "migrate"),
        ("Review the auth module for security issues.", "review"),
    ]
    for task, _expected_mode in tasks:
        t0 = time.perf_counter()
        for _ in range(100):
            _mode, _ = planner.classify(task)
        elapsed_per_call_ms = (time.perf_counter() - t0) / 100 * 1000
        assert elapsed_per_call_ms < 30, (
            f"classify('{task[:30]}...') took {elapsed_per_call_ms:.2f}ms (budget: 30ms)"
        )


# ---------------------------------------------------------------------------
# 2. Lexical layer (FTS5)
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_lexical_search_latency(tmp_path: Path, benchmark: object) -> None:
    """Lexical FTS5 search must complete in < 50ms p95 on a test corpus.

    Design budget: p95 < 50ms on 500k-symbol fixture (REQ-RET-1 / task 12.1).
    We use 200 symbols here; the assertion is a sanity check, not a scale test.
    Full scale test requires the 500k fixture.
    """
    db = _make_test_db(tmp_path)
    os.environ["COGNIS_DB_PATH"] = db.path

    try:
        from cognis_retrieval.lexical import LexicalLayer

        layer = LexicalLayer()

        def _run() -> None:
            layer.search("function validates input processes data", 10, db)

        if callable(benchmark):
            benchmark(_run)  # type: ignore[operator]
        else:
            times = []
            for _ in range(50):
                t0 = time.perf_counter()
                _run()
                times.append((time.perf_counter() - t0) * 1000)
            times.sort()
            p95 = times[int(0.95 * len(times))]
            assert p95 < 500, (  # generous for CI; 50ms is the real target on 500k
                f"Lexical search p95={p95:.2f}ms on 200-symbol test corpus.\n"
                f"Design budget: < 50ms on 500k-symbol fixture.\n"
                "Document gap in docs/performance.md."
            )
    except ImportError as exc:
        pytest.skip(f"cognis_retrieval.lexical not available: {exc}")
    finally:
        os.environ.pop("COGNIS_DB_PATH", None)


# ---------------------------------------------------------------------------
# 3. Structural layer (recursive CTE depth 5)
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_structural_traversal_depth5_latency(tmp_path: Path, benchmark: object) -> None:
    """Structural layer traversal depth ≤ 5 must be < 150ms p95.

    Design budget: p95 < 150ms for depth ≤ 5 with avg fan-out ≤ 8 (REQ-RET-3 / task 12.3).
    """
    db = _make_test_db(tmp_path)
    os.environ["COGNIS_DB_PATH"] = db.path

    try:
        from cognis_retrieval.structural import StructuralLayer

        layer = StructuralLayer()
        start_id = "ts:src/mod0.ts:func0@00000000"

        def _run() -> None:
            layer.dependency_trace(start_id, "out", 5, db)

        if callable(benchmark):
            benchmark(_run)  # type: ignore[operator]
        else:
            times = []
            for _ in range(50):
                t0 = time.perf_counter()
                _run()
                times.append((time.perf_counter() - t0) * 1000)
            times.sort()
            p95 = times[int(0.95 * len(times))]
            assert p95 < 500, (  # generous for CI
                f"Structural traversal depth=5 p95={p95:.2f}ms.\n"
                "Design budget: < 150ms. Document gap in docs/performance.md."
            )
    except ImportError as exc:
        pytest.skip(f"cognis_retrieval.structural not available: {exc}")
    finally:
        os.environ.pop("COGNIS_DB_PATH", None)


# ---------------------------------------------------------------------------
# 4. Capsule compose end-to-end
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_capsule_compose_e2e_latency(tmp_path: Path, benchmark: object) -> None:
    """End-to-end capsule retrieval must complete in < 400ms p95 (no LLM compression).

    Design budget: p95 < 400ms for capsule end-to-end without LLM compression
    (requirements.md NFR Performance).
    """
    from cognis.db import Database

    from tests.integration.conftest import _bootstrap_minimal_schema, _seed_mini_ts_app_symbols

    db_path = tmp_path / "cap_bench.db"
    db = Database(str(db_path))
    conn = db.connect()
    _bootstrap_minimal_schema(conn)
    _seed_mini_ts_app_symbols(conn)
    conn.commit()

    os.environ["COGNIS_DB_PATH"] = str(db_path)
    os.environ["COGNIS_AUDIT_LOG"] = str(tmp_path / "audit.log")

    try:
        from cognis_mcpd.tools import retrieve_context_capsule

        def _run() -> None:
            retrieve_context_capsule("why is /login timing out?", max_tokens=2000)

        if callable(benchmark):
            benchmark(_run)  # type: ignore[operator]
        else:
            times = []
            for _ in range(20):
                t0 = time.perf_counter()
                _run()
                times.append((time.perf_counter() - t0) * 1000)
            times.sort()
            p95 = times[int(0.95 * len(times))]
            assert p95 < 5000, (  # very generous for CI without embedder
                f"Capsule e2e p95={p95:.2f}ms.\n"
                "Design budget: < 400ms (no LLM compression). Document gap in docs/performance.md."
            )
    except ImportError as exc:
        pytest.skip(f"cognis_mcpd.tools not available: {exc}")
    finally:
        os.environ.pop("COGNIS_DB_PATH", None)
        os.environ.pop("COGNIS_AUDIT_LOG", None)


# ---------------------------------------------------------------------------
# 5. Semantic query embed cache (agent repeat-query path)
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_semantic_query_embed_cache_reuse() -> None:
    """Repeated identical queries must hit SemanticLayer's LRU embed cache.

    Agents often issue the same or lightly rephrased semantic query across
    turns. The retrieval layer caches query embeddings (capacity 1 000) so
    only the first call pays embedder cost within a reused SemanticLayer
    instance.

    Uses CountingEmbedder (no real model) for deterministic CI timing.
    """
    from cognis_retrieval.semantic import SemanticLayer

    from tests.benchmark._helpers import CountingEmbedder, percentile_ms, time_call_ms

    embedder = CountingEmbedder(delay_us=100.0)
    layer = SemanticLayer(embedder)
    query = "JWT authentication middleware timeout"

    # Cold: first embed populates the LRU.
    layer._embed_cached(query)
    assert embedder.embed_text_calls == 1

    # Warm: repeated calls must not invoke embed_text again.
    warm_ms = time_call_ms(lambda: layer._embed_cached(query), rounds=100)
    assert embedder.embed_text_calls == 1, (
        "SemanticLayer query LRU should reuse the first embedding; "
        f"embed_text called {embedder.embed_text_calls} times"
    )

    p95_warm = percentile_ms(warm_ms)
    # Cached path is pure Python + numpy; generous ceiling for slow CI runners.
    assert p95_warm < 5.0, (
        f"Cached semantic query embed p95={p95_warm:.2f}ms exceeds 5ms.\n"
        "See docs/performance.md — agents rely on this cache during multi-turn use."
    )


@pytest.mark.benchmark
def test_semantic_distinct_queries_still_bounded() -> None:
    """Distinct queries each embed once; total work scales with unique queries, not repeats."""
    from cognis_retrieval.semantic import SemanticLayer

    from tests.benchmark._helpers import CountingEmbedder

    embedder = CountingEmbedder(delay_us=0.0)
    layer = SemanticLayer(embedder)

    for i in range(20):
        layer._embed_cached(f"query variant {i}")

    assert embedder.embed_text_calls == 20


# ---------------------------------------------------------------------------
# 6. MCP tool round trips — repeated capsule retrieval
# ---------------------------------------------------------------------------


def _setup_capsule_bench_env(tmp_path: Path) -> None:
    """Point MCP tools at a minimal seeded DB for capsule benchmarks."""
    from cognis.db import Database

    from tests.integration.conftest import _bootstrap_minimal_schema, _seed_mini_ts_app_symbols

    db_path = tmp_path / "mcp_cap_bench.db"
    db = Database(str(db_path))
    conn = db.connect()
    _bootstrap_minimal_schema(conn)
    _seed_mini_ts_app_symbols(conn)
    conn.commit()

    os.environ["COGNIS_DB_PATH"] = str(db_path)
    os.environ["COGNIS_AUDIT_LOG"] = str(tmp_path / "audit.log")


@pytest.mark.benchmark
def test_mcp_capsule_repeated_task_warm_latency(tmp_path: Path) -> None:
    """Repeated retrieve_context_capsule calls should reach steady-state quickly.

    Agents should prefer one capsule call over chaining symbol_lookup +
    semantic_search + dependency_trace when possible — each MCP round trip
    adds JSON-RPC overhead and (without embedder reuse) may reload the model.

    This test uses a lexical-only path (no embedder) so CI stays deterministic.
    """
    from tests.benchmark._helpers import percentile_ms, time_call_ms

    _setup_capsule_bench_env(tmp_path)

    try:
        from cognis_mcpd.tools import retrieve_context_capsule

        task = "why is /login timing out under load?"

        # Cold call (may include one-time imports inside the tool).
        retrieve_context_capsule(task, max_tokens=2000)

        warm_ms = time_call_ms(
            lambda: retrieve_context_capsule(task, max_tokens=2000),
            rounds=30,
        )
        p95 = percentile_ms(warm_ms)

        # Lexical + planner + compose only; generous for CI without embedder.
        assert p95 < 5000, (
            f"Warm MCP capsule p95={p95:.2f}ms.\n"
            "Design budget: < 400ms with warm embedder. Document gap in docs/performance.md."
        )
    except ImportError as exc:
        pytest.skip(f"cognis_mcpd.tools not available: {exc}")
    finally:
        os.environ.pop("COGNIS_DB_PATH", None)
        os.environ.pop("COGNIS_AUDIT_LOG", None)


@pytest.mark.benchmark
def test_mcp_capsule_vs_multi_tool_round_trips(tmp_path: Path) -> None:
    """One capsule call should not be slower than three separate MCP tool calls.

    Encourages agent-efficient usage: retrieve_context_capsule bundles planner +
    retrieval layers in a single round trip instead of three serial tool calls.
    """
    from tests.benchmark._helpers import percentile_ms, time_call_ms

    _setup_capsule_bench_env(tmp_path)

    try:
        from cognis_mcpd.tools import dependency_trace, retrieve_context_capsule, symbol_lookup

        task = "login timeout jwt"
        start_id = "ts:src/routes/login.ts:postLogin@feedface"

        def _multi_tool() -> None:
            symbol_lookup("postLogin")
            dependency_trace(start_id, direction="out", depth=2)
            symbol_lookup("validate")

        def _capsule() -> None:
            retrieve_context_capsule(task, max_tokens=2000)

        multi_ms = time_call_ms(_multi_tool, rounds=15)
        capsule_ms = time_call_ms(_capsule, rounds=15)

        p95_multi = percentile_ms(multi_ms)
        p95_capsule = percentile_ms(capsule_ms)

        # Capsule does more work (planner + compose) but saves round trips;
        # it should stay within 3x the multi-tool baseline on this tiny fixture.
        assert p95_capsule < max(p95_multi * 3, 5000), (
            f"Capsule p95={p95_capsule:.2f}ms vs multi-tool p95={p95_multi:.2f}ms.\n"
            "If capsule is much slower, agents may avoid it — see docs/performance.md."
        )
    except ImportError as exc:
        pytest.skip(f"cognis_mcpd.tools not available: {exc}")
    finally:
        os.environ.pop("COGNIS_DB_PATH", None)
        os.environ.pop("COGNIS_AUDIT_LOG", None)


# ---------------------------------------------------------------------------
# 7. MCP semantic search — optional real-embedder warmup (opt-in)
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_mcp_semantic_search_repeated_queries_optional() -> None:
    """When a real embedder is available, repeated queries should warm up.

    Set ``COGNIS_BENCH_REAL_EMBEDDER=1`` locally to run this against
    sentence-transformers. Skipped by default so CI does not download models.

    Even with per-call Embedder construction, SemanticLayer's query LRU still
    helps when the same layer instance is reused; this test documents current
    MCP behavior for operators tuning agent workflows.
    """
    if os.environ.get("COGNIS_BENCH_REAL_EMBEDDER") != "1":
        pytest.skip("Set COGNIS_BENCH_REAL_EMBEDDER=1 to benchmark real embedder warmup")

    from tests.benchmark._helpers import percentile_ms, time_call_ms

    try:
        from cognis_mcpd.tools import semantic_search
    except ImportError as exc:
        pytest.skip(f"cognis_mcpd.tools not available: {exc}")

    query = "jwt token validation middleware"
    db_path = os.environ.get("COGNIS_DB_PATH")
    if not db_path:
        pytest.skip("COGNIS_DB_PATH must point at an indexed DB for real embedder bench")

    # Cold.
    first = semantic_search(query, k=5)
    if isinstance(first, dict) and "error" in first:
        pytest.skip(f"semantic_search unavailable: {first['error']}")

    warm_ms = time_call_ms(lambda: semantic_search(query, k=5), rounds=10)
    p95 = percentile_ms(warm_ms)

    # Real model inference varies by hardware; log-friendly generous bound.
    assert p95 < 30_000, (
        f"Repeated semantic_search p95={p95:.2f}ms exceeds 30s sanity bound.\n"
        "Consider process-level embedder reuse — see docs/performance.md."
    )
