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

Run the benchmark suite locally with `criterion`:

```bash
cargo bench
```

Useful variants:

```bash
cargo bench -p cognis-embed --bench embed_latency      # embedding latency
cargo bench -p cognis-eval --bench diffuse_latency      # CSAR / diffuse_context p50
```

Criterion stores its reports under `target/criterion/`.

### MCP and agent workflow benchmarks

The `cognis-eval` criterion bench (`benches/diffuse_latency.rs`) focuses on the
agent-facing latency that matters most:

| Bench group | What it checks |
| --- | --- |
| `csar_kernel/forward_push` | the forward-push PPR solver in isolation |
| `diffuse_context_resident` | seed-build + push + ranking over a resident graph |
| `diffuse_context_end_to_end` | native graph build + diffuse — the Requirement 11.2 p50 path |

These run on a deterministic synthetic graph in CI so they need no model
download. Point the bench at a real indexed DB for an apples-to-apples p50:

```bash
set COGNIS_DIFFUSE_DB=.cognis\uckg.db
cargo bench -p cognis-eval --bench diffuse_latency
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

Loading the ONNX model into memory can still take time on the **first** demand
(or at process open under eager policy). `cognis mcpd` reuses a process-wide
embedder and semantic layer once loaded, so warm queries avoid repeated model
construction, but operators should still keep the MCP server as a long-lived
process and avoid frequent restarts.

Warm policy is controlled by `COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP` (see
[mcp-client-config.md](mcp-client-config.md#eager-vs-lazy-semantic-startup)):

| Value | Effect on cold path |
| --- | --- |
| `0` (lazy) | Model maps on first semantic demand (single-flight); idle private bytes stay lower until then |
| `1` (eager) | Model maps at open; first semantic call avoids the load hitch |
| absent | Eager (legacy / direct-launch) |

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
- Expect the **first** semantic query after a lazy start (or after idle eviction)
  to pay model load; treat later queries as the steady-state budget (< 100 ms
  semantic target on a warm index). Eager mode (`COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=1`)
  shifts that cost to process open. Agents should still tolerate one cold-start
  interval.
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

The criterion benches above are the first stop for latency regressions
(`cargo bench`, reports under `target/criterion/`).

For a flamegraph of a hot path, use a Rust sampling profiler such as
[`cargo flamegraph`](https://github.com/flamegraph-rs/flamegraph) or
[`samply`](https://github.com/mstange/samply):

```bash
cargo install flamegraph
cargo flamegraph -p cognis --bin cognis -- index --full .
```

Build with the `release` profile (the default for benches) so the numbers
reflect the shipped binary.

## Practical tuning ideas

When performance degrades, common checks include:

- reduce unnecessary retrieval depth
- inspect query rewriting behavior
- verify the database is on fast local storage
- confirm the embedder and vector backend are available as expected
- measure cold-start versus warm-cache behavior separately
- prefer workspace MCP scope + thin-proxy stdio when idle process count or private bytes climb

## Process cardinality and private bytes

Latency is not the only resource signal. Idle multi-process topologies can
inflate private bytes by mapping ONNX in every `mcpd`/`indexd`. Acceptance
measurement for process cardinality and private bytes is documented in
[`tests/e2e/private-bytes/README.md`](../tests/e2e/private-bytes/README.md) and
indexed in [development-criteria.md](development-criteria.md).

Important labeling rules (preservation of evidence discipline):

- Distinguish **process cardinality**, **idle private bytes**, **active-load
  peak private bytes**, model mappings, and run variance.
- Windows private bytes over the process tree are authoritative for the recorded
  defect baseline (~1.23 GiB aggregate on one multi-process idle topology).
- The median **target** of ≤ 0.615 GiB on an equivalent stabilized-idle
  reproduction is a **gate target**, not a claim that every machine already
  achieves it. Publish only measured medians for named hardware/build/topology
  with `n ≥ 5` clean runs, labeled **empirical**.

## Known gaps

The current performance documentation is strongest for retrieval latency and
weaker for large-repository indexing measurements. Cold-index timings for larger
codebases should continue to be recorded and compared over time.

Additional gaps:

- Semantic benchmarks use a synthetic graph by default; real-model/real-DB
  timings require pointing the bench at an indexed DB via `COGNIS_DIFFUSE_DB`.
- Cold-start latency still depends heavily on local hardware, disk cache, and
  Windows model-load characteristics.
- Private-byte / process-cardinality medians are machine-specific; re-run the
  private-bytes harness when claiming progress against the 0.615 GiB target.

## Future work

Areas likely to improve later versions:

- faster indexer hot paths
- larger-scale vector backends
- result caching for capsule composition
- optional reranking with documented cost trade-offs
