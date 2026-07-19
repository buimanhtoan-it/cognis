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

## Daemon transport isolation (loopback, credentials, fingerprints)

Process topology changes (thin stdio proxy, optional shared HTTP, multi-host
attach) do **not** relax repository isolation. Controls that apply whenever a
daemon, proxy, or broker accepts or routes work:

| Control | Default / rule |
| --- | --- |
| **Bind address** | Loopback only (`127.0.0.1` / `::1`). Non-loopback binds are rejected unless explicitly opted in for advanced deployments. |
| **Scoped credential** | Where the transport supports it, each client route uses an unguessable scoped credential; missing or wrong credentials are rejected. |
| **Repository identity** | Attachment canonicalizes and verifies repository root + `COGNIS_DB_PATH` (and related identity headers). Cross-repository access is rejected. |
| **Model fingerprint** | Derived from immutable model asset checksums (the `.sha256` sidecars verified at download) plus backend, embedding dimension, and config identity. Sessions must **not** be reused when fingerprints differ. |
| **Sharing gate** | Direct shared HTTP and any future model broker stay **disabled by default** (`cognis.mcpSharedHttpEnabled` / `COGNIS_MCP_SHARED_HTTP`). Enabling the flag still requires evidence for semantic parity, eight-tool contracts, cancellation/failure behavior, concurrent load/eviction safety, repository isolation, model-fingerprint isolation, and reproducible process/private-byte improvement. A failed gate keeps the thin-proxy / per-repository stdio path with no data loss. |
| **Thin proxy** | Editor-facing proxies forward JSON-RPC only; they must not load ONNX or retain a repository DB/model as a heavy owner. |

These assumptions matter for multi-root and multi-host setups: two windows on the
same canonical repository may share one heavy daemon, but two different
repositories must never share DB, credentials, leases, or model sessions.

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
- HTTP MCP (when used) is loopback-bound by default with repository-identity and
  model-fingerprint checks; shared HTTP remains behind a reversible gate that
  defaults OFF
- MCP config migration backups and rollbacks must preserve non-Cognis user
  servers; Cognis never force-deletes unrelated MCP configuration

Future versions may add additional controls for outbound policy enforcement,
structured audit export, and richer provenance checks.

## Reporting vulnerabilities


If you discover a security issue, please report it privately through
[GitHub Security Advisories](https://github.com/buimanhtoan-it/cognis/security/advisories/new)
instead of opening a public issue.
