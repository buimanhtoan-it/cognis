# Performance

This document summarizes current latency targets, how to run benchmarks, and
where to look when performance drops.

## Current latency targets

The following p95 targets define the expected user-facing performance envelope:

| Area | Target |
| --- | --- |
| Planner | under 30 ms |
| Lexical retrieval | under 50 ms |
| Semantic retrieval | under 100 ms |
| Structural retrieval | under 150 ms |
| End-to-end capsule build | under 400 ms |
| Incremental re-index after commit | under 5 s |
| Cold index of 100k LOC | under 5 min |
| Cold index of 1M LOC | under 45 min |

Some of these are already benchmarked in CI; others remain targets that need
continued measurement on larger repositories.

## Benchmarking

Run the benchmark suite locally with:

```bash
pytest -m benchmark --benchmark-only
```

Useful variants:

```bash
pytest -m benchmark --benchmark-compare --benchmark-autosave
pytest tests/benchmark/test_latency.py -k planner
pytest tests/benchmark/test_latency.py -k "semantic or capsule"
```

The nightly benchmark workflow stores benchmark output under
`benchmark-reports/`.

### MCP and agent workflow benchmarks

Several benchmarks in `tests/benchmark/test_latency.py` focus on MCP tool
efficiency for coding agents:

| Test | What it checks |
| --- | --- |
| `test_semantic_query_embed_cache_reuse` | SemanticLayer query LRU — repeated identical queries avoid re-embedding |
| `test_mcp_capsule_repeated_task_warm_latency` | Steady-state `retrieve_context_capsule` latency on a warm lexical path |
| `test_mcp_capsule_vs_multi_tool_round_trips` | One capsule call vs three serial tool calls on the same fixture |
| `test_mcp_semantic_search_repeated_queries_optional` | Real embedder warmup (opt-in via `COGNIS_BENCH_REAL_EMBEDDER=1`) |

These use a deterministic stub embedder in CI so they do not download Hugging
Face models. Run the optional semantic benchmark locally against your indexed
DB:

```bash
set COGNIS_BENCH_REAL_EMBEDDER=1
set COGNIS_DB_PATH=.cognis\uckg.db
pytest tests/benchmark/test_latency.py -k semantic_search_repeated -m benchmark
```

## MCP tools and agent workflows

Coding agents pay for Cognis in **round trips** and **model load time**, not
just raw retrieval milliseconds.

### Prefer fewer round trips

Each MCP tool call crosses JSON-RPC, validation, audit logging, and result
serialization. For task-oriented work, **`retrieve_context_capsule`** is
usually more agent-efficient than chaining:

1. `symbol_lookup`
2. `semantic_search`
3. `dependency_trace`

The capsule path runs the planner once, merges hits, and returns a single
token-budgeted package — one round trip instead of three or more.

### Reuse the embedder and query cache

Semantic retrieval embeds the natural-language query before KNN search. Two
caches matter:

1. **Query LRU (SemanticLayer)** — keyed on the raw query string (capacity
   1 000). Within a reused `SemanticLayer` instance, identical queries incur
   one embed call.
2. **Indexer embedder LRU (LocalEmbedder)** — keyed on symbol `content_hash`
   during indexing; separate from query-time embedding.

Loading `sentence-transformers` into memory can still take seconds on the
**first** call. `cognis-mcpd` now reuses a process-wide embedder and semantic
layer, so warm queries avoid repeated model construction, but operators should
still keep the MCP server as a long-lived process and avoid frequent restarts.

The remaining cold-start risk is **initial model load**, especially on Windows.
Generated MCP config now includes safer default values for
`COGNIS_MCP_SOFT_TIMEOUT_S`, `COGNIS_MCP_HARD_TIMEOUT_S`, and
`COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S` on Windows, and operators can still
override them when an editor needs a larger startup budget.

### Practical agent guidance

- Start with **`retrieve_context_capsule`** for bugfix/explain tasks; add
  narrow follow-ups (`symbol_lookup`, `dependency_trace`) only when needed.
- Repeat the **same query string** when iterating — the semantic query cache
  is keyed literally, not semantically.
- Identical **MCP search tool** arguments (`symbol_search`, `semantic_search`,
  `discover_symbols`) are cached in-process for 60 seconds by default
  (`COGNIS_MCP_CACHE_TTL_S`), skipping repeated embedder and hydration work.
- Expect the **first** semantic query after server start to be slow; treat
  later queries as the steady-state budget (< 100 ms semantic target on a
  warm index). `cognis-mcpd` also does a best-effort background semantic warm-up
  on startup, but agents should still tolerate one cold-start interval.
- Set `COGNIS_DB_PATH` once in MCP config so every tool hits the same index
  without rediscovery overhead.

## Where time goes

### Planner

The current planner is rule-based. It is expected to be a very small share of
overall latency.

### Lexical retrieval

Lexical search depends on SQLite FTS5 and scales with corpus size and query
shape. Watch for complex rewritten queries or unusually large symbol tables.

### Semantic retrieval

Semantic search depends on `sqlite-vec`, hardware characteristics, and whether
the database pages are already warm in the OS cache.

### Structural retrieval

Structural traversal is usually fast, but latency can grow around symbols with
very high fan-out.

### Capsule composition

Capsule composition is CPU-bound and largely driven by result merging,
deduplication, and token counting. When the embedder is unavailable, the
capsule path skips semantic retrieval and stays lexical + structural only —
still useful, but agents lose concept-level recall.

## Profiling

To profile the built-in performance command:

```bash
cognis-cli profile --target capsule --iterations 50
```

For Python-level profiling:

```bash
python -m cProfile -s cumtime -m cognis.cli.main profile --target capsule
```

For sampling-based profiling:

```bash
pip install py-spy
py-spy record -o profile.svg -- cognis-mcpd
```

## Practical tuning ideas

When performance degrades, common checks include:

- reduce unnecessary retrieval depth
- inspect query rewriting behavior
- verify the database is on fast local storage
- confirm the embedder and vector backend are available as expected
- measure cold-start versus warm-cache behavior separately

## Known gaps

The current performance documentation is strongest for retrieval latency and
weaker for large-repository indexing measurements. Cold-index timings for larger
codebases should continue to be recorded and compared over time.

Additional gaps:

- Semantic benchmarks in CI use a stub embedder; real-model timings require the
  opt-in `COGNIS_BENCH_REAL_EMBEDDER=1` benchmark locally.
- Cold-start latency still depends heavily on local hardware, disk cache, and
  Windows Python/torch startup characteristics.

## Future work

Areas likely to improve later versions:

- faster indexer hot paths
- larger-scale vector backends
- result caching for capsule composition
- optional reranking with documented cost trade-offs
