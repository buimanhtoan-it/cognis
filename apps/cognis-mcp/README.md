# Cognis — AI Complexity, Optimization & Blast-Radius Guardrails

**Cognis** equips VS Code, Cursor, and MCP-compatible AI agents (Antigravity, Cursor Agent, GitHub Copilot, Claude Code, Kiro) with **real-time algorithmic complexity analysis**, **optimization prescriptions (`OPT-001..007`)**, **loop-amplified blast-radius guardrails (`CSIA`)**, and **graph-diffusion code retrieval (`CSAR`)** — powered by a single local Rust binary.

---

## What You Get in the Editor

1. **Live Big-O CodeLens** — Inline `Cognis: O(n^2) · space O(1) · 1 optimization` badges above functions and methods, click-wired to inspect full complexity and optimization prescriptions.
2. **Real-Time Guardrail Diagnostics** — Automatic **Problems** panel warnings when a saved change introduces quadratic loops (`OPT-001`), N+1 sequential `await` calls (`OPT-002`), quadratic string copies (`OPT-003`), loop-invariant `.sort()`/regex (`OPT-004`), multi-pass allocations (`OPT-005`), unmemoized exponential recursion (`OPT-006`), unbounded queries (`OPT-007`), or high `CSIA` caller blast radius.
3. **One-Click Workspace & MCP Setup** — Downloads the checksum-verified `cognis` Rust engine + `bge-small-en-v1.5` semantic model, builds `.cognis/uckg.db`, starts incremental `indexd` sync, and writes workspace MCP config (`.cursor/mcp.json`, `.vscode/mcp.json`, `.kiro/settings/mcp.json`) without clobbering your other MCP servers.
4. **Interactive MCP Inspector ("Try MCP Tools")** — Test all **12 Cognis MCP tools** directly inside the Cognis sidebar panel (Advanced view).

---

## Quick Start

1. Open the **Cognis** icon in the Activity Bar.
2. Click **Install engine** (or point `cognis.binaryPath` at a local `cognis` binary built from source).
3. Open a repository folder and click **Set up workspace** (or run **Cognis: Set Up Workspace** from the Command Palette).
4. Reload your MCP host / agent session once so it picks up the **12 Cognis MCP tools**.

---

## All 12 MCP Tools Exposed to Your AI Agent

### Real-Time Guardrail & Optimization Tools
- **`analyze_patch_impact`** — Speculative Shadow AST + CSIA blast-radius evaluation on unsaved code (`new_content` or `file_edits`), returning Big-O deltas, `OPT-001..007` findings, broken call sites, affected entrypoints, and minimal covering tests in milliseconds.
- **`inspect_symbol_complexity`** — Intra- and inter-procedural time/space Big-O, loop/recursion depth, and optimization advisories for any symbol.
- **`trace_blast_radius`** — Loop-amplified reverse Personalized PageRank (`CSIA`) ranking impacted callers, entrypoints, and tests.
- **`suggest_optimal_refactor`** — Concrete `OPT-001..007` optimization prescriptions grounded in existing utilities/helpers in your repository.

### Graph-Diffusion & Retrieval Tools
- **`diffuse_context`** — Flagship CSAR forward-push spreading-activation retrieval recovering full on-path call flow in one round trip.
- **`retrieve_context_capsule`** — Token-budgeted Context Capsule for bugfixes, features, and explanations.
- **`discover_symbols`** — Hybrid FTS5 lexical + ONNX semantic search fused via Reciprocal Rank Fusion (RRF).
- **`symbol_lookup`** — Resolve one symbol by ID, qualified name, or fuzzy name.
- **`symbol_search`** — Fast SQLite FTS5 lexical symbol search.
- **`semantic_search`** — Local `bge-small-en-v1.5` vector similarity search.
- **`resolve_symbols`** — Batch-hydrate up to 50 symbol IDs into full source records.
- **`dependency_trace`** — Walk callers (`in`), callees (`out`), or `both` across the call graph.

Plus live MCP resource: **`cognis://live/guardrails`**.

---

## Supported Languages

- **Code (10 languages via Tree-sitter):** TypeScript / JavaScript / TSX, Python, Go, C#, Java, Rust, C, C++, Ruby, PHP
- **Structured Artifacts (5 formats):** YAML, TOML, SQL, HTML, Markdown

---

## Key Extension Settings

| Setting | Default | Description |
| --- | --- | --- |
| `cognis.guardrails.enabled` | `true` | Enable real-time complexity, `OPT-001..007`, and blast-radius guardrails in the editor |
| `cognis.guardrails.codeLens` | `true` | Show live Big-O complexity and optimization count CodeLens above functions/methods |
| `cognis.guardrails.analyzeOnSave` | `true` | Automatically run `analyze_patch_impact` when supported source files are saved |
| `cognis.mcpHost` | `"auto"` | Target MCP host config (`auto`, `cursor`, `vscode`, `kiro`, `claude`) |
| `cognis.mcpConfigScope` | `"workspace"` | Write workspace-local MCP config (`workspace`) or user-level config (`global`) |
| `cognis.mcpStdioMode` | `"proxy"` | Use lightweight thin stdio proxy (`proxy`) to deduplicate heavy repository daemons |
| `cognis.mcpWarmSemanticOnStartup` | `false` | Lazy-load (`false`) vs eager-load (`true`) the local ONNX semantic model |
| `cognis.binaryPath` | `""` | Optional explicit path to a custom `cognis` binary |

---

## Commands

| Command | Purpose |
| --- | --- |
| **Cognis: Set Up Workspace** | Initialize `.cognis/`, build the index, start live sync, and wire MCP config |
| **Cognis: Analyze Current File Complexity & Impact** | Run `analyze_patch_impact` on the active editor buffer (including unsaved edits) |
| **Cognis: Rebuild Index** | Perform a full cold re-index of the workspace |
| **Cognis: Troubleshoot & Repair** | Check health, repair MCP wiring, and restart stalled sync daemons |
| **Cognis: Show Diagnostics Log** | Open structured extension diagnostics (`diagnostics.jsonl`) |
| **Cognis: Remove Everything (Prepare for Uninstall)** | Stop daemons, clean `.cognis/`, remove Cognis MCP entries, and delete managed binaries |

---

## Privacy & License

100% local execution — your source code, `.cognis/uckg.db` graph, and ONNX embeddings never leave your machine. Released under the [Apache-2.0 License](LICENSE.txt).
