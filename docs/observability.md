# Observability

This document describes the current runtime signals that help you monitor and
debug `cognis`.

## What is available today

The current implementation exposes these observability surfaces:

- a structured diagnostics trace in the VS Code extension (JSON Lines)
- in-memory metrics inside the MCP server
- Python logging from the CLI and daemon processes
- the append-only audit log under `.cognis/`

## Extension diagnostics trace

The VS Code extension writes a structured, append-only trace so that failures
which only appear in a real install — version skew, contract drift, a backend
that won't start — are reconstructable instead of invisible. This is the signal
that closes the "e2e green, production broken" gap.

- **Where:** `diagnostics.jsonl` under the extension's global storage, size-
  rotated at 5 MiB; also mirrored live into the **Cognis** output channel.
- **Open it:** command **Cognis: Show Diagnostics Log**.
- **Verbosity:** setting `cognis.logLevel` (`debug` | `info` | `warn` | `error`).
  Use `debug` to capture every CLI call and command with timings when filing an
  issue.
- **Each entry:** `ts`, `level`, `scope`, `message`, optional `data`
  (identifiers/counts only — never query text, file contents, or secrets),
  optional `durationMs`, and the `extVersion` so an entry can be correlated with
  a release.
- **What is captured today:** every `cognis-cli` invocation (exit code +
  duration), JSON parse/contract failures at the boundary, the startup
  version/capability handshake result, unknown indexd status phases, and the
  MCP-config write.

## Version/capability handshake

On activation the extension runs `cognis-cli handshake` and compares the
backend's `contract_version` against the version it was built with
(`EXPECTED_CONTRACT_VERSION`). A skew (older/newer backend, or a missing required
command/tool) is recorded to the trace and surfaced as an actionable warning,
rather than failing silently downstream. The contract source of truth is
`packages/core/cognis/contract.py`; the two version constants are kept in
lockstep by an e2e test.

## Metrics

`cognis-mcpd` records lightweight in-memory metrics during MCP tool execution.
These metrics are intended for local inspection and internal debugging.

The implementation lives in:

```text
apps/cognis-mcpd/cognis_mcpd/metrics.py
```

### Current metric families

| Metric | Meaning |
| --- | --- |
| `cognis_tool_calls_total` | Number of MCP tool calls by tool name |
| `cognis_tool_errors_total` | Number of calls that returned an error envelope |
| `cognis_tool_duration_seconds` | Latency distribution for each tool |
| `cognis_cache_hits_total` | Cache hits by cache name |
| `cognis_cache_misses_total` | Cache misses by cache name |
| `cognis_index_size_rows` | Row count by database table |

### Inspecting metrics in code

```python
from cognis_mcpd.metrics import METRICS

snapshot = METRICS.snapshot()
print(snapshot)
```

### Adding instrumentation

```python
from cognis_mcpd.metrics import METRICS

METRICS.tool_calls.inc("my_tool")

with METRICS.tool_latency.time("my_tool"):
    result = do_work()
```

## Logging

`cognis` uses Python logging for operator-visible runtime events.

### Typical log levels

| Level | Typical use |
| --- | --- |
| `DEBUG` | Per-query details and diagnostic information |
| `INFO` | Startup, shutdown, indexing progress, and normal tool activity |
| `WARNING` | Degraded behavior or partial failures |
| `ERROR` | Hard failures that require attention |

### Configure the log level

```bash
export COGNIS_LOG_LEVEL=DEBUG
cognis-mcpd
```

## Audit log

Every MCP tool call is recorded in the audit log:

```text
.cognis/audit.log
```

Each line is a JSON object that includes:

- timestamp
- tool name
- hashed arguments
- success or failure state

The audit log is intended for review and incident tracing. It avoids writing raw
arguments so that sensitive request contents are not stored directly.

## Resource hygiene (RAM / handle leaks)

`cognis-mcpd` is a long-lived process, so a per-call resource leak (an unclosed
sqlite connection, a file handle, an unbounded cache) would climb until the
editor's MCP host is sluggish or the process is OOM-killed.

- **Guard:** `tests/e2e/test_memory.py` spins up the real server over a real
  process boundary, drives hundreds of MCP tool calls through one session, and
  asserts bounded OS-handle and RSS growth (`pytest -m e2e -k memory`). A
  near-linear handle climb is the fingerprint of a per-call connection/file
  leak. Run it on a target machine to reproduce a suspected leak quantitatively.
- **Deterministic cleanup:** semantic tool stages run on a throwaway worker
  thread, and `Database.connect()` caches a sqlite connection *per thread*. The
  worker now closes its connection in `_run_with_deadline`'s `finally`
  (`_close_worker_db_connections`) so it is released immediately rather than
  lingering until GC — this keeps peak handles/RSS low under concurrent bursts
  and avoids the "unclosed database" finalizer warnings.

## Operator workflow

When debugging a deployment or local environment, check these in order:

1. process logs from `cognis-mcpd` and `cognis-indexd`
2. `cognis-cli health`
3. `.cognis/audit.log`
4. the local database and index status

## Planned expansion

Future versions may add:

- Prometheus-compatible export
- structured JSON logs
- HTTP metrics endpoints
- richer tracing for long-running indexing jobs
