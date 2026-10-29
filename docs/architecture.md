# Architecture Overview

This document describes the runtime structure of `cognis` and the data flow from source buffers and files to real-time MCP guardrail and retrieval responses.

## High-Level Design

`cognis` has four primary responsibilities:

1. **Index** a repository into a local Unified Code Knowledge Graph (`.cognis/uckg.db`) with static complexity and call-graph metadata.
2. **Evaluate** unsaved edits and patches in real time via a Speculative Shadow AST, a closed Complexity Semiring Lattice, and structural optimization detectors (`OPT-001..007`).
3. **Propagate** change impact upstream across callers via **CSIA** (loop-amplified reverse Personalized PageRank) and retrieve on-path code context via **CSAR** (forward-push Personalized PageRank) + RRF hybrid search.
4. **Expose** all capabilities through **12 read-only MCP tools**, the `cognis://live/guardrails` resource, and live VS Code / Cursor CodeLens + Diagnostics.

The engine is a self-contained Rust workspace shipping as a single multi-call binary with bundled SQLite (WAL mode) and optional statically-linked ONNX Runtime.

## Workspace Layout

| Crate | Responsibility |
| --- | --- |
| `cognis-core` | Domain models (`Symbol`, `Edge`), `ComplexityClass` semiring lattice, `GuardrailCapsule` / `OptimizationFinding` / `ImpactReport` contracts, configuration, and handshake constants |
| `cognis-store` | `rusqlite` (bundled SQLite + FTS5), schema migrations, `sqlite-vec` / BLOB vector store, and compressed-sparse-row (`CodeGraph`) builder |
| `cognis-embed` | `Embedder` / `Reranker` traits + factory + local ONNX backend (`bge-small-en-v1.5`) |
| `cognis-indexer` | 10-language tree-sitter parser, 5 artifact extractors, Speculative Shadow AST (`shadow.rs`), static complexity & `OPT-001..007` engine (`complexity.rs`), resolver, secret-redacting enricher, and SQLite writer |
| `cognis-csar` | **CSAR** forward-push Personalized PageRank retrieval solver (`push.rs`, `diffuse.rs`) and **CSIA** loop-amplified reverse-push blast-radius solver (`impact.rs`) |
| `cognis-retrieval` | Lexical, semantic, and structural retrieval layers, Reciprocal Rank Fusion (RRF), and token-budgeted Context Capsule composer |
| `cognis-mcp` | 12 MCP tools, `cognis://live/guardrails` resource, JSON-RPC 2.0 `stdio` & loopback `http` framing, concurrency/timeout caps, and SHA-256 hashed audit logging |
| `cognis-owner` | Canonical repository lease coordinator and lightweight thin-proxy process router |
| `cognis-eval` | Criterion latency benchmarks, golden-set evaluation, and mathematical property suites |
| `cognis-testgen` | Shared `proptest` generators across workspace crates (dev-only) |

## Runtime Surfaces (Single Multi-Call Binary)

All runtime surfaces are subcommands of the single `cognis` binary (`bins/cognis`):

| Surface | Responsibility |
| --- | --- |
| `cognis cli` (or `cognis <cmd>`) | Operator commands: `init`, `index`, `bootstrap`, `health`, `eval`, `paths`, `mcp-config`, and `handshake` |
| `cognis indexd` | File-system watcher and incremental indexing daemon |
| `cognis mcpd` | Read-only MCP server (`stdio`, thin `proxy`, or loopback `http`) serving the 12 tools and `cognis://live/guardrails` |
| `cognis owner` | Background repository daemon owner used when thin-proxy deduplication is active |

## End-to-End Data Flow

```text
  Saved Repository Files                     Unsaved Buffer / Agent Patch
            │                                             │
            ▼                                             ▼
   Tree-sitter Indexer                         Speculative Shadow AST
 (10 Code Langs + 5 Artifacts)               (In-Memory Parse + AST Diff)
            │                                             │
            ▼                                             ▼
  UCKG SQLite (.cognis/uckg.db) ◄────────────► Complexity Semiring + OPT-001..007
  (Symbols, Edges, FTS5, Vectors,                         │
   Complexity Metadata, Loop Depth)                       ▼
            │                                  CSIA Reverse-Push Impact
            ▼                                 (Loop-Amplified Blast Radius)
  RRF Hybrid + CSAR Forward-Push                          │
            │                                             │
            └──────────────────────┬──────────────────────┘
                                   ▼
                 12 MCP Tools + cognis://live/guardrails
                 VS Code / Cursor CodeLens & Diagnostics
```

## Indexer & Supported Languages

The parser stage (`crates/cognis-indexer/src/parser/`) is fault-tolerant per file and supports:

- **10 Tree-sitter Code Languages:** TypeScript / JavaScript / TSX, Python, Go, C#, Java, Rust, C, C++, Ruby, and PHP.
- **5 Structured Artifact Extractors:** YAML, TOML, SQL, HTML, and Markdown.

During indexing and Shadow AST evaluation, `crates/cognis-indexer/src/complexity.rs` computes:

1. **Intra-procedural time & space complexity** over the closed `ComplexityClass` semiring (`O(1)`, `O(log n)`, `O(sqrt n)`, `O(n)`, `O(n log n)`, `O(n^2)`, `O(n^3)`, `O(2^n)`, `O(n!)`), tracking loop nesting depth and call-edge loop depth (`call_loop_depth`).
2. **Inter-procedural complexity propagation** over call-graph SCCs (Tarjan topological propagation + mutual/self recursion escalation).
3. **7 Structural Optimization Rules (`OPT-001`..`OPT-007`)**, cross-referenced against existing repository symbols to ground refactor suggestions in real project utilities.

## Retrieval & Impact Solvers

- **RRF Hybrid Ranking (`cognis-retrieval`):** Combines SQLite FTS5 BM25 ranks and `bge-small-en-v1.5` vector similarity ranks via scale-invariant Reciprocal Rank Fusion.
- **CSAR Forward-Push (`cognis-csar::diffuse`):** Diffuses seed relevance over the symmetrized UCKG with $O(1/(\alpha\varepsilon))$ repo-size-independent cost to recover on-path bridge symbols.
- **CSIA Reverse-Push (`cognis-csar::impact`):** Propagates impact upstream along reversed `calls` / `imports` / `inherits` edges weighted by $w_{\text{impact}}(u \to v) = w_{\text{conf}} \cdot \beta^{\ell(u \to v)}$ ($\beta = 4$), amplifying callers that invoke a modified symbol inside loops.

See [csar.md](csar.md) for formal equations, theorems, and proofs.

## The 12 MCP Tools

| Category | Tool | Summary |
| --- | --- | --- |
| **Guardrail** | `analyze_patch_impact` | Speculative Shadow AST + CSIA change impact, Big-O deltas, `OPT-001..007`, breaking call sites, entrypoints & minimal tests |
| **Guardrail** | `inspect_symbol_complexity` | Deep intra-/inter-procedural time & space Big-O, loop depth, fan-in/fan-out, and optimization findings |
| **Guardrail** | `trace_blast_radius` | CSIA reverse-push blast radius, loop-amplified callers, affected entrypoints, and covering tests |
| **Guardrail** | `suggest_optimal_refactor` | Ranked `OPT-001..007` algorithmic refactors grounded in existing repository helpers |
| **Retrieval** | `diffuse_context` | CSAR forward-push spreading-activation retrieval recovering on-path flow in one call |
| **Retrieval** | `retrieve_context_capsule` | Token-budgeted Context Capsule (RRF-ranked hits + CSAR on-path context + risk areas) |
| **Retrieval** | `discover_symbols` | Hybrid lexical + semantic discovery fused with RRF |
| **Retrieval** | `symbol_lookup` | Exact or fuzzy lookup of one symbol by ID or qualified name |
| **Retrieval** | `symbol_search` | SQLite FTS5 lexical symbol search |
| **Retrieval** | `semantic_search` | Local ONNX vector KNN search |
| **Retrieval** | `resolve_symbols` | Batch-hydrate up to 50 symbol IDs into full source records |
| **Retrieval** | `dependency_trace` | Caller (`in`), callee (`out`), or bidirectional (`both`) graph traversal |
