# Observability

This document describes the current runtime signals that help you monitor and
debug `cognis`.

## What is available today

The current implementation exposes three main observability surfaces:

- in-memory metrics inside the MCP server
- Python logging from the CLI and daemon processes
- the append-only audit log under `.cognis/`

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
