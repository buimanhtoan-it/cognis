# MCP Client Configuration

This guide explains how to connect `cognis` to **Antigravity**, **Cursor**, **VS Code**, **Claude Code / Claude Desktop**, and **Kiro**, and how Cognis manages workspace isolation and daemon lifecycle.

## Prerequisites

Before connecting a client, ensure:

1. The repository has been indexed (`cognis bootstrap .` or **Cognis: Set Up Workspace**) so `.cognis/uckg.db` exists.
2. `cognis health` reports `overall: ok`.
3. You have the path to the `cognis` binary (`cognis.exe` on Windows).

---

## Automatic Setup (VS Code & Cursor Extension)

1. Install the `cognis-mcp` extension.
2. Click **Install engine** in the Cognis sidebar (or set `cognis.binaryPath`).
3. Open your repository and run **Cognis: Set Up Workspace**.

The extension writes a workspace-scoped MCP configuration file with resolved `COGNIS_DB_PATH`, `COGNIS_AUDIT_LOG`, and `COGNIS_REPO_ROOT` paths while preserving all non-Cognis MCP servers in your config file.

### Scope & Process Topology Settings

| Setting | Values | Default | Behavior |
| --- | --- | --- | --- |
| `cognis.mcpConfigScope` | `workspace`, `global` | **`workspace`** | Writes `.cursor/mcp.json`, `.vscode/mcp.json`, or `.kiro/settings/mcp.json` inside the repo so hosts only start daemons for open workspaces |
| `cognis.mcpStdioMode` | `proxy`, `heavy` | **`proxy`** | Editor-facing stdio process runs as a lightweight model-free proxy forwarding JSON-RPC to a single heavy daemon per canonical repository |
| `cognis.mcpWarmSemanticOnStartup` | `false`, `true` | **`false`** | `false` (`COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP=0`) lazy-loads ONNX on first semantic query; `true` (`=1`) warms at process start |

---

## Manual Configuration by Host

You can generate a host config snippet automatically:

```bash
cognis cli mcp-config --host cursor --repo-root /path/to/repo
```

### Configuration File Locations

| Host | Configuration File |
| --- | --- |
| **Antigravity** | `~/.gemini/config/mcp_config.json` (`%USERPROFILE%\.gemini\config\mcp_config.json` on Windows) |
| **Cursor (Workspace)** | `<repo>/.cursor/mcp.json` |
| **Cursor (Global)** | `~/.cursor/mcp.json` |
| **VS Code (Workspace)** | `<repo>/.vscode/mcp.json` (uses top-level `"servers"` key) |
| **Claude Code / Desktop** | `~/.claude.json` or `%APPDATA%\Claude\claude_desktop_config.json` |
| **Kiro (Workspace)** | `<repo>/.kiro/settings/mcp.json` |

### Antigravity / Cursor / Claude / Kiro JSON (`mcpServers`)

#### Windows

```json
{
  "mcpServers": {
    "cognis": {
      "command": "C:\\Users\\you\\.cognis\\bin\\cognis.exe",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "D:\\repos\\my-app\\.cognis\\uckg.db",
        "COGNIS_AUDIT_LOG": "D:\\repos\\my-app\\.cognis\\audit.log",
        "COGNIS_REPO_ROOT": "D:\\repos\\my-app",
        "COGNIS_ONNX_MODEL_DIR": "C:\\Users\\you\\.cognis\\models\\bge-small-en-v1.5",
        "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0",
        "COGNIS_MCP_SOFT_TIMEOUT_S": "10",
        "COGNIS_MCP_HARD_TIMEOUT_S": "25",
        "COGNIS_MCP_DISCOVER_SEMANTIC_TIMEOUT_S": "12"
      }
    }
  }
}
```

#### macOS / Linux

```json
{
  "mcpServers": {
    "cognis": {
      "command": "/usr/local/bin/cognis",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "/abs/path/to/repo/.cognis/uckg.db",
        "COGNIS_AUDIT_LOG": "/abs/path/to/repo/.cognis/audit.log",
        "COGNIS_REPO_ROOT": "/abs/path/to/repo",
        "COGNIS_MCP_WARM_SEMANTIC_ON_STARTUP": "0"
      }
    }
  }
}
```

### VS Code `.vscode/mcp.json` (`servers`)

```json
{
  "servers": {
    "cognis": {
      "type": "stdio",
      "command": "C:\\Users\\you\\.cognis\\bin\\cognis.exe",
      "args": ["mcpd"],
      "env": {
        "COGNIS_DB_PATH": "D:\\repos\\my-app\\.cognis\\uckg.db",
        "COGNIS_AUDIT_LOG": "D:\\repos\\my-app\\.cognis\\audit.log",
        "COGNIS_REPO_ROOT": "D:\\repos\\my-app"
      }
    }
  }
}
```

---

## All 12 MCP Tools & Live Resource

| Tool / Resource | Purpose |
| --- | --- |
| `analyze_patch_impact(file_path?, new_content?, file_edits?)` | **Flagship Guardrail** — Speculative Shadow AST + CSIA change impact, Big-O deltas, `OPT-001..007` findings, breaking call sites, entrypoints & covering tests |
| `inspect_symbol_complexity(name_or_id, file_path?)` | Intra- and inter-procedural time/space Big-O, loop depth, fan-in/fan-out, and optimization advisories |
| `trace_blast_radius(symbol_id, new_complexity?, signature_changed?, alpha?, eps?, max_results?)` | CSIA loop-amplified reverse-push blast radius, callers, entrypoints, and minimal test set |
| `suggest_optimal_refactor(name_or_id?, file_path?, k?)` | Ranked `OPT-001..007` algorithmic optimizations grounded in existing repo helpers |
| `diffuse_context(query, k?, alpha?, eps?, kind?, file_path?)` | **Flagship Retrieval** — CSAR forward-push spreading-activation recovering full on-path flow |
| `retrieve_context_capsule(task, max_tokens?, include_runtime?)` | Token-budgeted Context Capsule (RRF + CSAR + untrusted markers) |
| `discover_symbols(query, k?, kind?, file_path?)` | Hybrid FTS5 lexical + ONNX semantic symbol discovery (RRF-fused) |
| `symbol_lookup(name_or_id, kind?)` | Exact or fuzzy lookup of a single symbol |
| `symbol_search(query, k?, kind?, file_path?)` | Lexical FTS5 search over names, signatures, and docstrings |
| `semantic_search(query, k?, kind?, file_path?)` | Vector KNN concept search over `bge-small-en-v1.5` embeddings |
| `resolve_symbols(symbol_ids, include_body?)` | Batch-hydrate up to 50 symbol IDs into full source records |
| `dependency_trace(symbol_id, direction?, depth?)` | Caller (`in`), callee (`out`), or bidirectional (`both`) call-graph walk |
| `cognis://live/guardrails` *(MCP Resource)* | Latest real-time complexity & blast-radius guardrail snapshot |

### Recommended Agent Workflow During Implementation

1. **Understand before editing:** Call `diffuse_context` or `retrieve_context_capsule` once instead of chaining manual searches.
2. **Validate while coding:** Call `analyze_patch_impact` with the updated file buffer (`new_content`) before finalizing edits to catch $O(n^2)$ regressions, N+1 loops (`OPT-002`), broken caller signatures, or loop-amplified upstream blast radius.
3. **Optimize hotspots:** Call `suggest_optimal_refactor` or `inspect_symbol_complexity` whenever a guardrail flags a complexity escalation or anti-pattern.

---

## Multi-Host Ownership & Troubleshooting

- **Canonical Lease Deduplication:** Multiple editors (e.g. Antigravity + Cursor + VS Code) attached to the same canonical repository share one heavy daemon via `.cognis/mcpd.lease` and `.cognis/indexd.lease` when thin-proxy mode is active.
- **Safe Config Updates:** Cognis never deletes or overwrites non-Cognis entries in `mcp.json` / `mcp_config.json`.

| Symptom | Resolution |
| --- | --- |
| `INDEX_NOT_READY` | Run `cognis index --full .` or **Cognis: Set Up Workspace** |
| Missing new guardrail tools (`TOOL_DISABLED`) | Run `cognis init` (which additively upgrades `.cognis/config.yaml` `allow_tools` to all 12 tools) and reload the MCP host |
| Semantic search returns 0 vector hits | Verify `COGNIS_ONNX_MODEL_DIR` points to `bge-small-en-v1.5` assets and re-run `cognis index --full .` |
