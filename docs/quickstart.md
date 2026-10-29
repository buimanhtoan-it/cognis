# Quickstart

Get a repository indexed and connected to real-time complexity/impact guardrails and code retrieval in under two minutes.

## Before You Start

Install or build the `cognis` binary (or install the VS Code / Cursor extension) following [install.md](install.md).

---

## Fastest CLI Path

From the root of the repository you want to analyze:

```bash
cognis bootstrap .
```

`bootstrap` executes three steps in order:

1. `cognis init` — creates `.cognis/config.yaml`, cache directories, and `.cognis/audit.log`
2. `cognis index --full .` — parses source files, extracts symbols/edges, computes static Big-O complexity + `OPT-001..007` metadata, and builds embeddings
3. `cognis health` — verifies configuration, SQLite store, and capability readiness

For an instant cold run without semantic embeddings:

```bash
cognis bootstrap . --skip-embeddings
```

---

## Connect Your Editor or AI Agent

### Using the VS Code / Cursor Extension

1. Open the repository in VS Code or Cursor.
2. Open the **Cognis** sidebar and click **Set up workspace** (or run **Cognis: Set Up Workspace**).
3. Open any supported source file:
   - **Big-O CodeLens** appears above functions and methods (`Cognis: O(n) · space O(1)`).
   - Saving a file triggers **real-time guardrail analysis** and surfaces `OPT-001..007` or `CSIA` blast-radius warnings in the **Problems** panel.

### Connecting Any MCP Host (Antigravity, Cursor, VS Code, Claude Code, Kiro)

Generate a ready-to-paste MCP block:

```bash
cognis cli mcp-config --host cursor --repo-root /path/to/repo
```

Or see [mcp-client-config.md](mcp-client-config.md) for host-specific config locations.

---

## First Agent Tool Calls

Once your MCP client is connected to `cognis mcpd`:

### 1. Real-Time Patch Guardrail (Before Saving or Committing)

```text
cognis: analyze_patch_impact(
  file_path="src/orders/reconcile.ts",
  new_content="<updated file contents>"
)
```

Returns Big-O deltas (`O(n) -> O(n^2)`), `OPT-001..007` optimization prescriptions, CSIA upstream blast radius, signature-breaking call sites, affected entrypoints, and minimal tests to run.

### 2. Inspect Complexity or Suggest Optimizations

```text
cognis: inspect_symbol_complexity(name_or_id="reconcileOrders")
cognis: suggest_optimal_refactor(name_or_id="reconcileOrders", k=5)
cognis: trace_blast_radius(symbol_id="reconcileOrders", new_complexity="O(n^2)")
```

### 3. Understand Call Flow & Build Context in One Round Trip

```text
cognis: diffuse_context(query="order retry and idempotency flow", k=10)
cognis: retrieve_context_capsule(task="Fix duplicate retry charge under timeout", max_tokens=6000)
```

---

## Tool Choice Cheat Sheet

| Goal | Best MCP Tool |
| --- | --- |
| Validate an edit/patch for Big-O regressions, anti-patterns & blast radius | `analyze_patch_impact` |
| Check a function's intra-/inter-procedural time & space complexity | `inspect_symbol_complexity` |
| See upstream callers, hot-loop amplification, entrypoints & tests affected | `trace_blast_radius` |
| Get ranked `OPT-001..007` refactors grounded in repo utilities | `suggest_optimal_refactor` |
| Trace end-to-end call flow around a feature or concept | `diffuse_context` |
| Assemble a token-budgeted context package for a task | `retrieve_context_capsule` |
| Hybrid keyword + meaning symbol discovery | `discover_symbols` |
| Fast exact/keyword symbol search | `symbol_search` / `symbol_lookup` |
| Batch-fetch full bodies for up to 50 symbol IDs | `resolve_symbols` |
| Walk direct/multi-hop callers or callees | `dependency_trace` |

---

## Keep the Index Live Outside the Extension

If you use a standalone terminal agent (e.g., Claude Code) without the VS Code / Cursor extension open, run the incremental file watcher in the background:

```bash
cognis indexd --repo-root /path/to/repo
```
