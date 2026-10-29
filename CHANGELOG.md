# Changelog

All notable changes to Cognis are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-29

### Added

- **Real-Time Algorithmic Complexity & Optimization Engine (`cognis-core`, `cognis-indexer`)**
  - Closed **Complexity Semiring Lattice** (`O(1)` through `O(2^n)` and `O(n!)`) with exact sequential ($\oplus = \max$) and nested ($\otimes$) composition plus inter-procedural call-graph propagation.
  - **Speculative Shadow AST** (`shadow.rs`) evaluating unsaved buffers and agent patches in memory (`< 10 ms`) without mutating SQLite until save.
  - **7 Structural Anti-Pattern & Optimization Detectors (`OPT-001`..`OPT-007`)**:
    - `OPT-001` Nested linear scan inside loop (`O(n * m)` -> `HashSet`/`HashMap` `O(n + m)`)
    - `OPT-002` Sequential `await` / DB / network I/O inside loop (N+1 query / serial I/O -> batch or `Promise.all` / `join_all`)
    - `OPT-003` Quadratic string or slice concatenation in loop (`O(n^2)` -> pre-allocated buffer / `.join()`)
    - `OPT-004` Repeated `.sort()` or regex compilation inside loop (`O(k * n log n)` -> hoist invariant outside loop)
    - `OPT-005` Unnecessary multi-pass collection allocation (`.map().filter().collect()` chains -> single-pass iterator)
    - `OPT-006` Unmemoized branching self-recursion (`O(2^n)` -> memoization / dynamic programming)
    - `OPT-007` Unbounded collection growth / missing pagination in request handlers
- **CSIA — Code Spreading Impact Analysis (`cognis-csar::impact`)**
  - Loop-amplified reverse Personalized PageRank ($\beta^\ell = 4^\ell$ edge weighting by caller loop nesting depth $\ell$) computing transitive blast radius, signature-breaking call sites, affected API/CLI entrypoints, minimal covering tests, and composite risk scores (`Low` / `Medium` / `High` / `Critical`).
- **CSAR — Code Spreading-Activation Retrieval (`cognis-csar`, `cognis-retrieval`)**
  - Forward-push Personalized PageRank solver with repository-size-independent work bound $O(1/(\alpha\varepsilon))$, combined with SQLite FTS5 lexical search, `sqlite-vec` / `onnx-local` (`bge-small-en-v1.5`) semantic embeddings, and Reciprocal Rank Fusion (RRF).
- **12-Tool MCP Server & Live Guardrail Resource (`cognis-mcp`, `cognis-mcpd`)**
  - JSON-RPC 2.0 `stdio` and loopback `http` transports, thin-proxy multi-host deduplication (`cognis-owner`), concurrency/timeout caps, secret redaction, and SHA-256 hashed-argument audit logging.
  - **4 Real-Time Guardrail Tools**: `analyze_patch_impact`, `inspect_symbol_complexity`, `trace_blast_radius`, `suggest_optimal_refactor`.
  - **8 Graph-Diffusion & Retrieval Tools**: `diffuse_context`, `symbol_lookup`, `symbol_search`, `discover_symbols`, `semantic_search`, `resolve_symbols`, `dependency_trace`, `retrieve_context_capsule`.
  - **Live MCP Resource**: `cognis://live/guardrails` exposing the latest real-time complexity & blast-radius snapshot.
- **Unified Multi-Language Indexer (`cognis-indexer`, `cognis-store`)**
  - Bundled SQLite WAL store (`uckg.db`) with incremental file watching (`cognis indexd`).
  - **10 Tree-sitter Code Languages**: TypeScript/JavaScript/TSX, Python, Go, C#, Java, Rust, C, C++, Ruby, and PHP.
  - **5 Structured Artifact Extractors**: YAML, TOML, SQL, HTML, and Markdown.
- **VS Code & Cursor Extension (`cognis-mcp` `v0.1.0`)**
  - Live **Big-O CodeLens** badges above functions and methods.
  - Real-time **Problems Panel Diagnostics** surfacing `OPT-001..007` optimizations and `CSIA` blast-radius warnings on save.
  - One-click managed binary + ONNX model installer, workspace MCP auto-configuration (Cursor, VS Code, Antigravity, Claude Code, Kiro), and interactive **Try MCP Tools** inspector.
