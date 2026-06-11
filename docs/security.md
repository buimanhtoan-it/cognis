# Security Model

This document describes the current security assumptions and controls in
`cognis`.

## Core assumption

`cognis` treats indexed text and MCP tool output as potentially untrusted. This
includes comments, docstrings, commit messages, and other content that may be
present in a repository but should not be treated as trusted instructions.

## Threat model

| Threat | Typical source | Current control |
| --- | --- | --- |
| Prompt injection | Comments, PR descriptions, commit messages | Mark untrusted content and preserve the markers in capsules |
| Secret leakage | Hard-coded credentials in code or docs | Redact secret-shaped values before persistence |
| Tool misuse | MCP clients requesting unsafe operations | Keep the MCP tool surface read-only |
| Expensive or abusive queries | Deep traversals or large requests | Enforce hard limits on depth, result size, and wall time |
| Index poisoning | Malicious repository content | Track provenance and content hashes during indexing |

## Untrusted content markers

When `cognis` includes untrusted text in a capsule, it wraps that text so the
client can treat it differently:

```text
<<<UNTRUSTED type="comment" symbol="auth.jwt.validate">>>
... raw content ...
<<<END UNTRUSTED>>>
```

Clients should instruct their model to ignore instructions inside those
sections.

`cognis` also records untrusted sections in capsule metadata so downstream tools
can detect them programmatically.

## Secret redaction

Before text is written to the database or embedded for retrieval, `cognis`
scans for common secret patterns, including:

- cloud provider API keys
- GitHub and OpenAI tokens
- JWTs
- PEM material
- high-entropy values associated with `password=` or `secret=` patterns

When a match is found, the value is replaced with a stable redaction token. The
original secret is not persisted.

## MCP tool limits

The MCP server enforces hard limits to reduce accidental overload and malicious
queries.

| Limit | Current value |
| --- | --- |
| Maximum traversal depth | 8 |
| Maximum result count | 50 |
| Maximum capsule size | 32,000 tokens |
| Soft timeout | 5 seconds |
| Hard timeout | 10 seconds |
| Max concurrent tool calls | 16 (env `COGNIS_MCP_MAX_CONCURRENCY`) |

The global concurrency cap is a process-wide bounded semaphore: every tool call
must acquire one of `COGNIS_MCP_MAX_CONCURRENCY` slots (default 16) before
running. When the server is saturated, a call that cannot be admitted within a
short acquire timeout returns a retryable error envelope instead of piling up
work. In addition, the semantic-retrieval stage is **single-flighted** (one
in-flight semantic query at a time) and enters a short **cooldown** after a
timeout, so a slow embedder cannot stack overlapping work. Set the cap to `0` to
disable it.

## Audit logging

All MCP tool calls are written to `.cognis/audit.log`.

The audit log is:

- append-only
- JSONL
- stored locally with the repository runtime data
- based on hashed arguments rather than raw arguments

This allows basic tracing and incident review without logging sensitive request
contents directly.

## Security boundaries

The current security posture is intentionally conservative:

- MCP tools are read-only
- runtime state is local to `.cognis/`
- external services are not required for the default installation

Future versions may add additional controls for outbound policy enforcement,
structured audit export, and richer provenance checks.

## Reporting vulnerabilities

If you discover a security issue, please report it privately through
[GitHub Security Advisories](https://github.com/buimanhtoan-it/cognis/security/advisories/new)
instead of opening a public issue.
