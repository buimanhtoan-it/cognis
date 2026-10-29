<p align="center">
  <img src="assets/logo.png" alt="Cognis logo" width="96" />
</p>

<h1 align="center">Cognis</h1>

<p align="center">
  <strong>Real-time algorithmic complexity, optimization &amp; change-impact guardrails for AI coding agents.</strong><br/>
  Powered by Speculative Shadow AST, CSIA reverse-push blast-radius math, and CSAR graph-diffusion retrieval — in one self-contained Rust binary + VS Code / Cursor extension.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/version-0.1.0-emerald.svg" alt="Version 0.1.0" />
  <img src="https://img.shields.io/badge/engine-100%25%20Rust-orange.svg" alt="100% Rust" />
  <img src="https://img.shields.io/badge/MCP-12%20tools-purple.svg" alt="12 MCP Tools" />
</p>

---

## Why Cognis?

AI coding agents write syntactically valid code quickly, yet routinely introduce **silent production regressions** while iterating:

1. **Hidden Big-O regressions** — placing a `.find()` / `.includes()` scan, `.sort()`, regex compilation, or sequential `await` DB query inside a loop turns an $O(n)$ path into $O(n^2)$ or an N+1 bottleneck.
2. **Loop-amplified blast radius** — changing a helper's complexity from $O(1)$ to $O(n)$ looks harmless locally, until an upstream worker calls it inside a doubly-nested loop ($\beta^\ell = 4^2 = 16\times$ amplification).
3. **Blind signature & flow edits** — modifying a symbol without seeing every transitive caller, API entrypoint, covering test, or existing repository utility that already solves the problem.

**Cognis** runs locally alongside your editor and AI agent (Antigravity, Cursor, VS Code, Claude Code, Kiro) to evaluate **algorithmic time/space complexity**, **structural optimization prescriptions (`OPT-001..007`)**, and **transitive change impact (`CSIA`)** in **milliseconds** — before bad code is even committed.

---

## Core Architecture

Cognis unifies three mathematical & static-analysis engines over a local SQLite **Unified Code Knowledge Graph (UCKG)** (`.cognis/uckg.db`):

| Engine | Mechanism | Latency | What it gives AI agents & developers |
| --- | --- | --- | --- |
| **Speculative Shadow AST & Complexity Semiring** | In-memory tree-sitter diff + closed `(Time, Space)` lattice ($\oplus = \max$, $\otimes = \text{nest}$) + `OPT-001..007` AST rules | **`< 10 ms`** | Instant Big-O delta (`O(n) -> O(n^2)`), hotspot line numbers, and concrete refactors grounded in existing repo helpers |
| **CSIA (Code Spreading Impact Analysis)** | Loop-amplified **reverse** Personalized PageRank ($w_{\text{impact}} = w_{\text{conf}} \cdot \beta^{\ell_{\text{loop}}}$) over caller graphs | **`~5–25 ms`** | Transitive blast radius, loop-amplified callers, broken signature call sites, affected entrypoints, minimal test targets, and composite risk score |
| **CSAR (Code Spreading-Activation Retrieval)** | **Forward-push** Personalized PageRank ($O(1/(\alpha\varepsilon))$ repo-size-independent work bound) + FTS5 + `bge-small-en-v1.5` ONNX + RRF | **`~15–90 ms`** | Full on-path call-flow context and token-budgeted Context Capsules in a single MCP round trip |

---

## The 12 MCP Tools + Live Resource

`cognis mcpd` serves **12 read-only MCP tools** plus the live resource `cognis://live/guardrails` over `stdio` or loopback HTTP:

### Real-Time Guardrail, Complexity & Impact Tools (4)

| Tool | Purpose |
| --- | --- |
| `analyze_patch_impact` | **Flagship Guardrail** — evaluates unsaved `new_content` (or multi-file `file_edits`) via Speculative Shadow AST + CSIA in one call; returns Big-O deltas, `OPT-001..007` findings, breaking call sites, affected entrypoints, tests to run, and `verdict` |
| `inspect_symbol_complexity` | Deep intra- & inter-procedural time/space Big-O breakdown, loop/recursion depth, fan-in/fan-out, and optimization findings for any symbol |
| `trace_blast_radius` | Runs CSIA reverse-push from a target symbol (with optional `new_complexity` simulation) to rank impacted callers, entrypoints, and covering tests |
| `suggest_optimal_refactor` | Ranks concrete `OPT-001..007` algorithmic refactors for a symbol or file and cross-references reusable utilities/caches already in the repository |

### Graph-Diffusion & Code Intelligence Tools (8)

| Tool | Purpose |
| --- | --- |
| `diffuse_context` | **Flagship CSAR Retrieval** — seeds from lexical + semantic hits and diffuses across the call graph to recover on-path flow in one call |
| `retrieve_context_capsule` | Composes a token-budgeted Context Capsule (RRF-ranked symbols, CSAR on-path flow, risk areas, and untrusted-content markers) |
| `discover_symbols` | Hybrid FTS5 lexical + ONNX semantic symbol discovery fused with Reciprocal Rank Fusion (RRF) |
| `symbol_lookup` | Exact or fuzzy resolution of a symbol by ID or qualified name |
| `symbol_search` | Fast SQLite FTS5 lexical search across symbol names, signatures, and docstrings |
| `semantic_search` | Meaning-based vector KNN search over local `bge-small-en-v1.5` embeddings |
| `resolve_symbols` | Batch-hydrates up to 50 symbol IDs into full source records in one round trip |
| `dependency_trace` | Traverses upstream callers (`in`), downstream callees (`out`), or `both` up to depth 8 |

---

## Built-In Optimization Rules (`OPT-001` – `OPT-007`)

| Rule | Detected Anti-Pattern | Prescribed Optimization |
| --- | --- | --- |
| `OPT-001` | Linear scan (`.find`, `.includes`, `.indexOf`, `in list`) inside loop | Pre-index into `HashSet` / `HashMap` / `Map` outside loop ($O(n \cdot m) \to O(n + m)$) |
| `OPT-002` | Sequential `await` / DB query / HTTP call inside loop (N+1) | Batch query (`WHERE id IN (...)`) or parallelize with `Promise.all` / `join_all` |
| `OPT-003` | String or array concatenation (`+=`, spread copy) inside loop | Accumulate into pre-allocated buffer / `String::with_capacity` / `.join()` |
| `OPT-004` | `.sort()` or `Regex::new` / `new RegExp` inside loop | Hoist loop-invariant sort or compiled regex outside the loop |
| `OPT-005` | Chained multi-pass `.map().filter().map()` intermediate allocations | Fuse into a single-pass iterator / comprehension / `filter_map` |
| `OPT-006` | Unmemoized branching self-recursion ($O(2^n)$) | Add memoization cache or convert to bottom-up dynamic programming ($O(n)$) |
| `OPT-007` | Unbounded `SELECT *` / `.find()` without `LIMIT` in handler | Enforce explicit pagination / streaming cursor bounds |

---

## Supported Languages

- **10 Tree-sitter Code Languages:** TypeScript / JavaScript / TSX, Python, Go, C#, Java, Rust, C, C++, Ruby, PHP
- **5 Structured Artifact Formats:** YAML, TOML, SQL, HTML, Markdown

---

## Quick Start

### 1. Build from Source (Rust + VS Code / Cursor Extension)

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis

# Build the single self-contained `cognis` binary (bundled SQLite)
cargo build --release -p cognis --bin cognis

# Optional: statically link ONNX Runtime for local semantic embeddings
cargo build --release -p cognis --bin cognis --features onnx-download
```

Package and install the VS Code / Cursor extension:

```bash
cd apps/cognis-mcp
npm install
npm run package
# Install the generated cognis-mcp-0.1.0.vsix in VS Code or Cursor
```

### 2. Index a Repository via CLI

```bash
cognis bootstrap /path/to/your/repo
cognis health --repo-root /path/to/your/repo
```

### 3. Connect Your AI Agent (Antigravity / Cursor / VS Code / Claude Code)

Run **Cognis: Set Up Workspace** in the VS Code / Cursor extension (which writes workspace MCP config automatically and enables live **Big-O CodeLens** + **Guardrail Diagnostics**), or add `cognis mcpd` directly to your MCP config:

```json
{
  "mcpServers": {
    "cognis": {
      "command": "/absolute/path/to/cognis",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "/absolute/path/to/repo/.cognis/uckg.db",
        "COGNIS_AUDIT_LOG": "/absolute/path/to/repo/.cognis/audit.log",
        "COGNIS_REPO_ROOT": "/absolute/path/to/repo"
      }
    }
  }
}
```

See [docs/mcp-client-config.md](docs/mcp-client-config.md) for Antigravity, Cursor, VS Code, Claude Code, and Kiro examples.

---

## Documentation

- [Installation Guide](docs/install.md)
- [Quickstart](docs/quickstart.md)
- [MCP Client Configuration](docs/mcp-client-config.md)
- [Architecture Overview](docs/architecture.md)
- [CSAR & CSIA Mathematical Foundations](docs/csar.md)
- [Performance & Benchmarks](docs/performance.md)
- [Security Model](docs/security.md)
- [Observability & Diagnostics](docs/observability.md)
- [End-to-End Testing](docs/e2e-testing.md)
- [VS Code / Cursor Extension Guide](apps/cognis-mcp/README.md)

---

## Privacy & Security

- **100% Local-First:** All AST parsing, complexity analysis, SQLite indexing, ONNX embedding inference, and MCP tool execution run locally on your machine. No source code ever leaves your device.
- **Read-Only MCP Surface:** All 12 MCP tools are strictly read-only, bounded by concurrency and wall-time caps, with automatic secret redaction and SHA-256 hashed-argument audit logs (`.cognis/audit.log`).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
