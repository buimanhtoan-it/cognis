# Architecture Overview

This document describes the runtime structure of `cognis` and the flow from
source files to MCP tool responses.

## High-level design

`cognis` has three main responsibilities:

1. index a repository into a local knowledge store
2. retrieve relevant code context from that store
3. expose the retrieval layer through MCP tools

The current implementation is local-first. It uses a single SQLite database and
does not require external services for the default workflow.

## Runtime components

Three processes share one SQLite database at `.cognis/uckg.db`:

| Component | Responsibility |
| --- | --- |
| `cognis-cli` | One-off commands such as `init`, `index`, `bootstrap`, `health`, and `eval` |
| `cognis-indexd` | Long-running watcher and incremental indexing daemon |
| `cognis-mcpd` | MCP server that serves retrieval and capsule-building requests |

The database runs in WAL mode so readers and writers can coexist during normal
operation.

## End-to-end data flow

```text
Source files / Git metadata
        |
        v
Indexer pipeline
        |
        v
UCKG database (.cognis/uckg.db)
        |
        v
Retrieval layers
        |
        v
Planner and capsule composer
        |
        v
MCP tools
```

## Indexer pipeline

The indexer pipeline is responsible for turning raw files into structured data:

1. **Watcher**  
   Detects repository changes and batches file events.

2. **Parser**  
   Uses tree-sitter grammars for TypeScript, Python, and Go.

3. **Resolver**  
   Connects symbols through imports, calls, and other detectable relations.

4. **Enricher**  
   Adds metadata such as side effects, secret redaction, and safety flags.

5. **Embedder**  
   Produces local vector embeddings when semantic retrieval is enabled.

6. **Writer**  
   Persists the result to SQLite using per-file transactions.

## Storage model

The local database acts as the Unified Code Knowledge Graph (UCKG). It combines:

- SQLite tables for core entities and edges
- FTS5 for lexical search
- `sqlite-vec` for vector similarity search when available

The storage layer is designed so the rest of the system can keep the same data
model even if the backend changes later.

## Retrieval model

cognis's primary retrieval engine is **CSAR — Code Spreading-Activation
Retrieval** (`packages/retrieval/cognis_retrieval/csar.py`). CSAR diffuses a
seed relevance distribution across the code knowledge graph using Personalized
PageRank (random walk with restart), unifying the semantic and structural
signals into one tunable operator. Its forward-push solver has a provable work
bound `1/(α·ε)` that is independent of repository size. See
[csar.md](csar.md) for the mathematics and proofs.

CSAR seeds from, and falls back to, three base layers:

| Layer | Backend | Purpose |
| --- | --- | --- |
| Lexical | SQLite FTS5 | Fast keyword and exact-term matching (CSAR seed) |
| Semantic | `sqlite-vec` | Meaning-based retrieval over embeddings (CSAR seed) |
| Structural | Recursive SQL queries | Direct dependency / call-graph traversal |
| **CSAR** | **Personalized PageRank over the UCKG** | **Unified diffusion ranking (flagship)** |

Planned future signals include:

- temporal signals from version control history
- behavioral signals from runtime or tracing data

## Planner and capsule composition

The planner decides which retrieval layers to use and how much result budget to
allocate. The current planner is rule-based and deterministic.

The capsule composer combines the selected evidence into a single MCP response.
Depending on the tool, this may include:

- matching symbols
- call chains
- ranked evidence
- risk areas
- untrusted-content markers

## MCP tool surface

The MCP server exposes eight tools oriented toward agent-efficient retrieval:

| Tool | Purpose |
| --- | --- |
| `diffuse_context` | **CSAR spreading-activation retrieval — flagship, recommended default** |
| `discover_symbols` | Hybrid lexical + semantic discovery (RRF) |
| `symbol_search` | Discover top-k symbols by lexical match |
| `symbol_lookup` | Resolve one symbol by id, qualified name, or fuzzy match |
| `resolve_symbols` | Batch hydrate up to 50 symbols |
| `semantic_search` | Search by concept or intent with enriched payloads |
| `dependency_trace` | Traverse callers or callees; hits include symbol metadata |
| `retrieve_context_capsule` | Build a task-oriented context package (CSAR-powered) |

### Agent-efficient retrieval flow

Agents should prefer `diffuse_context` for "understand or trace this flow"
intents — it seeds from lexical + semantic matches and diffuses across the call
graph, recovering on-path symbols that independent ranking misses, in one round
trip. Use `discover_symbols` for quick exploratory lookup, `resolve_symbols` to
hydrate multiple ids, and `retrieve_context_capsule` when a task description is
already available (its structural stage is CSAR-powered). `dependency_trace`
returns enriched hit metadata (qualified name, kind, file path, line range) so
follow-up lookups are often unnecessary.

Semantic retrieval (`discover_symbols`, `semantic_search`, and the semantic stage
inside `retrieve_context_capsule`) reuses a single process-wide embedder after the
first load. Search tools also use a short-lived in-process result cache (default
60s) to avoid repeated embedder and hydration work within a session.

## Extension points

The architecture intentionally isolates the major backends so they can be
swapped without changing the overall workflow:

- vector backend
- graph traversal backend
- indexer implementation language
- MCP transport
- planner strategy

### Pluggable models (embedder / reranker registry)

Embedding and reranking models sit behind narrow protocols and a registry, so
swapping one is a local change — the retrieval and capsule flow never sees it.

- **Embedder protocol** — `embed_batch`, `embed_text`, and an `embedding_dim`
  attribute (`packages/indexer/.../embedder.py`). The semantic layer only needs
  the narrower `QueryEmbedder` surface (`embed_text` + `embedding_dim`) declared
  in `packages/retrieval/.../base.py`, which keeps retrieval free of any
  dependency on the indexer.
- **Registry / factory** — `cognis_indexer.registry.build_embedder(config)` is
  the single selection point used by the daemon, MCP server, CLI, and eval. Add
  a backend with one factory:

  ```python
  @register_embedder("my_backend")
  def _build(config: EmbedderConfig) -> Embedder:
      from cognis_indexer.embedder import MyEmbedder
      return MyEmbedder(model=config.model)
  ```

  Built-in backends: `local` (production), `voyage` and `openai` (selectable
  stubs returning zero vectors until their API call is implemented).
- **Model-driven dimension** — an embedder reports its own `embedding_dim`;
  `Database.reconcile_embedding_dim` persists it and recreates the `symbol_vec`
  table when a different-sized model is plugged in. No pinned constant.
- **Reranker seam** — `cognis_retrieval.reranker.build_reranker(config)` returns
  a pass-through `NoOpReranker` when `reranker.enabled` is false (flow
  unchanged), or a registered backend otherwise. Register one with
  `@register_reranker(...)`. The bundled `local` cross-encoder backend is a stub
  today.

## Related documents

- [csar.md](csar.md) for the CSAR retrieval method, mathematics, and proofs
- [install.md](install.md) for installation and environment setup
- [quickstart.md](quickstart.md) for the first local run
- [mcp-client-config.md](mcp-client-config.md) for client configuration
- [security.md](security.md) for the security model
- [performance.md](performance.md) for latency targets and profiling guidance
