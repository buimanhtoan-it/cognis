# Security Model

This document describes the security assumptions, guardrails, and isolation controls enforced by `cognis`.

## Core Assumption

`cognis` treats indexed repository text and MCP tool output as potentially untrusted data — never executable instructions. Comments, docstrings, Markdown files, and string literals inside a repository may contain adversarial text.

## Threat Model

| Threat | Typical Vector | Control in Cognis |
| --- | --- | --- |
| Prompt injection | Adversarial instructions embedded in comments or docs | Wrap untrusted text in explicit `<<<UNTRUSTED ...>>>` markers inside Context Capsules |
| Secret leakage | Hard-coded API keys, tokens, JWTs, or PEM material | Automatically redact secret-shaped strings prior to SQLite persistence and embedding |
| Destructive tool misuse | Malicious or hallucinated MCP write calls | Strictly **read-only** 12-tool MCP surface; no file mutation or shell execution tools |
| Resource exhaustion / DoS | Deep graph traversals or concurrent query floods | Bounded concurrency semaphore, single-flight semantic embedding, depth/result caps, and soft/hard wall-time timeouts |
| Cross-repo data bleed | Multi-window / multi-host daemon sharing | Canonical repository + DB path verification, loopback-only HTTP binds, scoped credentials, and SHA-256 model-fingerprint isolation |

## Untrusted Content Markers

When `retrieve_context_capsule` includes comments or docstrings, it wraps them explicitly:

```text
<<<UNTRUSTED type="comment" symbol="auth.jwt.validate">>>
... raw content ...
<<<END UNTRUSTED>>>
```

Capsule metadata also records untrusted spans so MCP hosts and agents can treat them strictly as data.

## Automatic Secret Redaction

Before symbol bodies or excerpts are written to `.cognis/uckg.db` or passed to the embedder, `cognis-indexer` scans for common credential shapes:

- Cloud provider keys (AWS, GCP, Azure)
- GitHub, OpenAI, Anthropic, and Slack tokens
- JSON Web Tokens (JWTs) and PEM private keys
- High-entropy values paired with `password=`, `secret=`, `token=`, or `api_key=`

Matched values are replaced with deterministic redaction placeholders; raw secrets are never persisted.

## MCP Resource Caps & Concurrency Controls

| Control | Default Limit |
| --- | --- |
| Maximum dependency traversal depth | `8` |
| Maximum results per search / hydrate call | `50` |
| Maximum capsule token budget | `32,000` tokens |
| Soft tool timeout | `5 s` (configurable via `COGNIS_MCP_SOFT_TIMEOUT_S`) |
| Hard tool timeout | `10 s` (configurable via `COGNIS_MCP_HARD_TIMEOUT_S`) |
| Max concurrent tool calls | `16` (`COGNIS_MCP_MAX_CONCURRENCY`) |

Semantic embedding queries are additionally **single-flighted** with a short post-timeout cooldown so slow cold-start model loads cannot pile up overlapping sessions.

## Daemon & Transport Isolation

| Control | Rule |
| --- | --- |
| **Loopback-only HTTP** | Any local HTTP transport binds strictly to `127.0.0.1` / `::1` unless explicitly overridden |
| **Scoped credentials** | Loopback HTTP routes require unguessable per-session credentials |
| **Canonical repository identity** | Daemons verify canonicalized repo root + `COGNIS_DB_PATH` before serving |
| **Model fingerprint verification** | Derived from `.sha256` sidecars, backend, and embedding dimension; mismatched fingerprints are never shared |
| **Shared HTTP gate** | Disabled by default (`cognis.mcpSharedHttpEnabled = false`); twelve-tool contract parity and isolation checks govern any opt-in |
| **Non-destructive MCP config edits** | Cognis preserves all non-Cognis MCP servers across setup, migration, repair, and uninstall |

## Hashed-Argument Audit Log

Every MCP tool invocation appends a JSONL entry to `.cognis/audit.log` containing:

- ISO-8601 timestamp
- tool name
- SHA-256 hash of arguments (never raw source code or query text)
- execution duration and ok/error status

## Reporting Vulnerabilities

Please report security issues privately via [GitHub Security Advisories](https://github.com/buimanhtoan-it/cognis/security/advisories/new) rather than public issues.
