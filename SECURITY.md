# Security Policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Report privately via [GitHub Security Advisories](https://github.com/buimanhtoan-it/cognis/security/advisories/new). We aim to acknowledge reports within **5 business days**.

## Security model

See [docs/security.md](docs/security.md) for the threat model, untrusted-content markers, secret redaction, MCP concurrency/timeout limits, loopback transport isolation, and hashed-argument audit logging.

## Operational checklist

- Keep MCP tool access local (`stdio` or loopback-only `127.0.0.1` / `::1`).
- Index only trusted workspace directories; keep `.cognis/` out of version control.
- Do not commit `.env` files, API keys, or private certificates.
- Review `.cognis/audit.log` periodically; entries store SHA-256 hashed arguments rather than raw code payloads.
- Re-index (`cognis index --full .`) whenever `cognis health` reports a schema or index version mismatch.
