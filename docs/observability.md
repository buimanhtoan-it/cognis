# Observability & Diagnostics

This document describes the runtime telemetry, logs, and health checks built into `cognis` and `cognis-mcp`.

## Overview of Signals

1. **Extension Diagnostics Trace (`diagnostics.jsonl`)** — structured JSONL trace of extension commands, CLI spawns, MCP wiring, and guardrail invocations.
2. **Startup Capability Handshake (`cognis cli handshake`)** — verifies engine/extension contract lockstep and advertises all 12 MCP tools.
3. **Workspace Health Report (`cognis health`)** — structured check of `.cognis/config.yaml`, SQLite store, vectors, and index freshness.
4. **MCP Metrics & Audit Log (`.cognis/audit.log`)** — in-memory tool latency/error counters and append-only SHA-256 hashed-argument audit trail.

## Extension Diagnostics Trace

- **Location:** `diagnostics.jsonl` inside the editor's global storage directory (size-rotated at 5 MiB) and mirrored to the **Cognis** Output channel.
- **Open in Editor:** Run **Cognis: Show Diagnostics Log** from the Command Palette.
- **Verbosity:** Controlled by `cognis.logLevel` (`debug` | `info` | `warn` | `error`).
- **Privacy:** Records timestamps, scopes, exit codes, durations (`durationMs`), counts, and extension version — never source code, query text, or secrets.

## Version & Contract Handshake

On activation, `cognis-mcp` runs `cognis cli handshake` and compares the engine's `contract_version` and advertised `mcp_tools` (all 12 tools) against `EXPECTED_CONTRACT_VERSION`. Any version or capability mismatch surfaces an actionable repair prompt in the Cognis panel instead of failing silently during agent sessions.

## In-Process MCP Metrics

`cognis mcpd` tracks lightweight counters and histograms per session:

| Metric | Description |
| --- | --- |
| `cognis_tool_calls_total` | Total invocations per MCP tool |
| `cognis_tool_errors_total` | Calls returning a structured error envelope |
| `cognis_tool_duration_seconds` | Execution latency per tool |
| `cognis_cache_hits_total` / `misses_total` | Query & result cache hit rates |
| `cognis_index_size_rows` | Symbol, edge, and vector row counts in `.cognis/uckg.db` |

## Audit Log (`.cognis/audit.log`)

Every MCP tool call appends a JSON line to `.cognis/audit.log` recording timestamp, tool name, SHA-256 argument digest, duration, and status.

## Debugging Checklist

When diagnosing a workspace issue, inspect in order:

1. `cognis health` in the repository root
2. **Cognis: Show Diagnostics Log** in VS Code / Cursor
3. `.cognis/indexd-status.json` (live watcher state)
4. `.cognis/audit.log` (recent MCP tool calls and error codes)
