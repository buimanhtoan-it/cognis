# Performance & Latency Guide

This document summarizes Cognis latency targets, benchmark suites, caching layers, and agent workflow recommendations.

## Latency Envelope

| Subsystem | Target (Warm) | Notes |
| --- | --- | --- |
| **Speculative Shadow AST + Complexity + `OPT-001..007`** | **under 10 ms** | Pure in-memory tree-sitter parse + AST diff + semiring lattice; zero SQLite writes |
| **CSIA Reverse-Push Blast Radius (`trace_blast_radius`)** | **under 25 ms** | Loop-amplified reverse PPR over the UCKG call graph |
| **Full `analyze_patch_impact` (Shadow AST + CSIA)** | **under 40 ms** | Combined complexity delta, optimization scan, breaking call sites, entrypoints & tests |
| **Lexical retrieval (`symbol_search` / FTS5)** | **under 50 ms** | SQLite FTS5 BM25 |
| **CSAR `diffuse_context` (forward-push)** | **under 90 ms** | Work bounded by $O(1/(\alpha\varepsilon))$, independent of repository size |
| **Semantic retrieval (`semantic_search` warm)** | **under 100 ms** | Local `bge-small-en-v1.5` ONNX + vector KNN |
| **End-to-end `retrieve_context_capsule`** | **under 400 ms** | Planner + RRF hybrid + CSAR on-path context + token budgeting |
| **Incremental re-index on save (`indexd`)** | **under 2 s** | Content-hash diff + dependent caller re-resolution |

## Running Benchmarks

Run the Criterion benchmark suite from the repository root:

```bash
cargo bench
```

Targeted benchmark suites:

```bash
cargo bench -p cognis-embed --bench embed_latency
cargo bench -p cognis-eval --bench diffuse_latency
```

To benchmark `diffuse_context` against a real indexed workspace:

```powershell
$env:COGNIS_DIFFUSE_DB = ".cognis\uckg.db"
cargo bench -p cognis-eval --bench diffuse_latency
```

Criterion HTML and JSON reports are written under `target/criterion/`.

## Caching & Memory Hygiene

1. **In-Memory Shadow AST:** `analyze_patch_impact` parses only the edited buffer(s) in memory and reads pre-computed caller complexity/edges from SQLite WAL, avoiding re-indexing on every agent iteration.
2. **Semantic Query LRU (`SemanticLayer`):** Caches up to 1,000 query vectors in memory so repeated agent queries bypass ONNX tokenization and inference.
3. **MCP Tool Result Cache:** Short-lived TTL cache (`COGNIS_MCP_CACHE_TTL_S`, default `60` seconds) for identical read-only search calls.
4. **Lazy vs Eager ONNX Warm Policy (`COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP`):**
   - `0` (default in extension-generated configs): zero ONNX memory footprint at idle until the first semantic tool call; guardrail, AST complexity, lexical, and structural tools respond immediately without loading ONNX.
   - `1`: pre-loads the ONNX session at `mcpd` startup for zero cold-hit latency on the first semantic query.
5. **Thin-Proxy Topology (`cognis.mcpStdioMode = "proxy"`):** Deduplicates heavy repository daemons across multiple editor windows/hosts attached to the same canonical repository.

## Profiling

Profile hot paths on release builds using [`samply`](https://github.com/mstange/samply) or [`cargo-flamegraph`](https://github.com/flamegraph-rs/flamegraph):

```bash
cargo flamegraph -p cognis --bin cognis -- index --full .
```
