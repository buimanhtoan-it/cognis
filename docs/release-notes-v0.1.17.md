# Release Notes — cognis v0.1.17

> **Historical Python-era release.** The install commands and runtime
> architecture below are archival and must not be used for current Cognis.
> Follow [install.md](install.md) for the pure-Rust product and current Polar
> ZIP/source-build distribution.
>
> **Phase 1 MVP** — first public beta release.

## What's in This Release

### CSAR — Code Spreading-Activation Retrieval (primary engine)

cognis's headline capability. CSAR seeds a relevance distribution from cheap
lexical + semantic matches and **diffuses** it across the code knowledge graph
using Personalized PageRank (random walk with restart), recovering symbols on
the call/flow path between matches that independent embedding/lexical ranking
misses. Its forward-push solver has a provable work bound `1/(alpha*eps)` that
is **independent of repository size**. Math, proofs, and verification are in
[docs/csar.md](csar.md).

### Eight MCP Tools (REQ-MCP-1)

cognis exposes 8 tools via the Model Context Protocol (FastMCP, stdio transport):

| Tool | Description |
|------|-------------|
| `diffuse_context(query, k?, alpha?, eps?, ...)` | **Flagship** CSAR spreading-activation retrieval in one round trip |
| `discover_symbols(query, k?, kind?, file_path?)` | Hybrid lexical + semantic discovery (RRF) |
| `symbol_search(query, k?, kind?, file_path?)` | Top-k lexical symbol discovery |
| `symbol_lookup(name_or_id, kind?)` | Resolve any symbol by id, qualified name, or fuzzy search |
| `resolve_symbols(symbol_ids, include_body?)` | Batch hydrate up to 50 symbols |
| `semantic_search(query, k?, mode?)` | Conceptual search using bge-small-en-v1.5 embeddings |
| `dependency_trace(symbol_id, direction, depth)` | Call-graph traversal (callers/callees up to depth 8) |
| `retrieve_context_capsule(task, max_tokens?, include_runtime?)` | Full task-aware context capsule (CSAR-powered) |

### Three Language Parsers

Tree-sitter-based parsers for:
- **TypeScript** — functions, arrow functions, classes, methods, interfaces, exports
- **Python** — sync/async defs, classes, methods, decorators, ALL_CAPS constants
- **Go** — funcs, methods (with receivers), types, interfaces

### Hybrid 4-Layer Retrieval

- **CSAR** (Personalized PageRank diffusion): unified flagship ranking; recovers
  full code flow with repo-size-independent cost
- **Lexical** (FTS5): exact symbol/error-token matching, p95 < 50ms
- **Semantic** (sqlite-vec KNN): conceptual similarity, p95 < 100ms
- **Structural** (recursive CTE): call-graph traversal, p95 < 150ms for depth ≤ 5

### Cognitive Context Planner

Rule-based classifier determines task mode (`bugfix`, `feature`, `refactor`, `explain`,
`migrate`, `review`) and allocates token budget across retrieval layers in < 30ms.

### Context Capsule v1

Structured JSON capsule with:
- `root_cause_candidates`: ranked candidate symbols for the task
- `relevant_symbols`: related symbols with scores
- `call_chain`: traced call path
- `risk_areas`: high fan-in / recently changed symbols
- `sources[]`: every claim backed by a symbol/commit/trace source
- Token budget enforcement (tiktoken cl100k_base + 10% margin)

### Security

- **Secret redaction**: AWS/GCP/Azure keys, JWTs, GitHub tokens, OpenAI keys, PEM headers,
  `password=` patterns detected and replaced with `[REDACTED:<type>]` before indexing
- **Untrusted content tagging**: code comments and docstrings wrapped in `<<<UNTRUSTED>>>` markers
- **Audit logging**: all MCP tool calls logged (args hash only, never raw args)

### Single-File Storage

All data lives in one SQLite database (`.cognis/uckg.db`) in WAL mode.
No external services required for Phase 1 MVP.

## MCP Conformance

Built-in conformance check results:

```
cognis MCP Conformance Report
  harness : builtin
  version : 0.1.17
  overall : PASS

  [PASS] tools_importable                                 All 8 tools imported from cognis_mcpd.tools
  [PASS] diffuse_context_error_envelope                   error envelope well-formed (code=INVALID_ARGUMENT)
  [PASS] symbol_lookup_error_envelope                     error envelope well-formed (code=INVALID_ARGUMENT)
  [PASS] semantic_search_error_envelope                   error envelope well-formed (code=INVALID_ARGUMENT)
  [PASS] dependency_trace_error_envelope                  error envelope well-formed (code=INVALID_ARGUMENT)
  [PASS] retrieve_context_capsule_error_envelope          error envelope well-formed (code=INVALID_ARGUMENT)
  [PASS] symbol_lookup_returns_valid_type                 returned dict
  [PASS] semantic_search_returns_valid_type               returned dict
  [PASS] dependency_trace_returns_valid_type              returned dict
  [PASS] retrieve_context_capsule_returns_valid_type      returned dict
```

Run `cognis-cli mcp-conformance` to verify your installation.

## Phase 1 Exit Criteria Status

| Criterion | Status |
|-----------|--------|
| 8 MCP tools pass conformance | ✅ |
| 3 languages (TS, Py, Go) functional | ✅ |
| Capsule v1 schema valid for golden queries | ✅ |
| Recall@10 >= 0.70 on golden set | pending — requires live index (see docs/eval/phase1-baseline.md) |
| All Performance Plan latency budgets met | 🔲 requires benchmarking (see docs/performance.md) |
| Bugfix demo on mini-ts-app | 🔲 requires live Claude Code session |

## Known Limitations

- **Behavioral layer (runtime)**: Phase 3 — OTel adapter not yet implemented. `include_runtime=True` in `retrieve_context_capsule` is a no-op.
- **Temporal layer (git history)**: Phase 2 — commit/PR linkage not yet indexed.
- **Reranker**: Phase 2 — no cross-encoder reranking at MVP.
- **SSE transport**: Phase 2 — only stdio transport at MVP.
- **Predictive prefetch**: Phase 3.
- **Session memory**: Phase 2.
- **Scale**: tested up to ~500k symbols. For 5M+, migrate to Qdrant (Phase 2 extension point).

## Getting Started

```bash
pip install cognis[indexer,embed-local,tokenizers,mcp]
cd /your/repo
cognis-cli init
cognis-cli index --full .
cognis-cli health
cognis-mcpd  # or configure via docs/mcp-client-config.md
```

## Changelog

See `CHANGELOG.md` for the full change history.

## License

Apache-2.0. See `LICENSE`.
