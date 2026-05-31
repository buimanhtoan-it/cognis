# Security Policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Report privately via [GitHub Security Advisories](https://github.com/buimanhtoan-it/cognis/security/advisories/new).

We aim to acknowledge reports within **5 business days**.

## Security model

See [docs/security.md](docs/security.md) for the threat model, untrusted-content handling, secret redaction, MCP tool limits, and audit logging.

## Hardening checklist for self-hosted deployments

- Run `cognis-mcpd` as a non-root user (Docker image defaults to user `cognis`).
- Mount only the workspace that should be indexed; keep `.cognis/` on a persistent volume.
- Do not commit `.env` files or API keys; `.gitignore` excludes common secret patterns.
- Review `.cognis/audit.log` periodically; entries store hashed arguments only.
- Re-index after upgrading cognis when `cognis-cli health` reports an `index_version` mismatch.
