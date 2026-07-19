# Documentation

This directory contains user, operator, and maintainer documentation for
`cognis`.

## Getting started

| Document | Use it for |
| --- | --- |
| [getting-started.md](getting-started.md) | Follow a plug-and-play path from fresh machine to working Cursor / VS Code setup |
| [install.md](install.md) | Install the Polar ZIP or build the Rust engine and editor extension from source |
| [quickstart.md](quickstart.md) | Bootstrap a repository, run the MCP server, and make the first successful query |
| [mcp-client-config.md](mcp-client-config.md) | Configure Claude Code, Claude Desktop, Cursor, VS Code, or Cline; workspace scope, migration/rollback, eager/lazy semantic, multi-host lifecycle |
| [troubleshooting-huggingface.md](troubleshooting-huggingface.md) | Resolve common local embedding and model download problems |

## Editor integration

| Document | Use it for |
| --- | --- |
| [../apps/cognis-vscode/README.md](../apps/cognis-vscode/README.md) | Build, install, and use the VS Code / Cursor extension |

## Deployment and operations

| Document | Use it for |
| --- | --- |
| [production-flow.md](production-flow.md) | Choose the shortest path to a working local or self-hosted setup |
| [operations.md](operations.md) | Review the source-built container deployment status and operational design |
| [observability.md](observability.md) | Metrics, logging, and audit trail details |
| [performance.md](performance.md) | Latency budgets, warm policy, process/private-byte labeling, and profiling guidance |
| [security.md](security.md) | Security model, untrusted content, loopback/credential/fingerprint isolation, and disclosure process |
| [e2e-testing.md](e2e-testing.md) | Cross-app E2E, contract parity, and private-byte / process-cardinality measurement |

## Reference and maintenance

| Document | Use it for |
| --- | --- |
| [architecture.md](architecture.md) | Internal structure, data flow, and MCP tool design |
| [development-criteria.md](development-criteria.md) | The four-pillar measurement loop (quality / UX / reliability / scaling, including MCP process/RAM targets) used to develop every release |
| [deps.md](deps.md) | Dependency decisions and rationale |
| [release.md](release.md) | Release steps for maintainers |
| [eval/phase1-baseline.md](eval/phase1-baseline.md) | Eval baseline and acceptance criteria |
| [eval/swe-bench-methodology.md](eval/swe-bench-methodology.md) | SWE-bench Lite evaluation method |

Historical Python-era release notes: [release-notes-v0.3.0.md](release-notes-v0.3.0.md), [release-notes-v0.2.1.md](release-notes-v0.2.1.md), [release-notes-v0.2.0.md](release-notes-v0.2.0.md), and [release-notes-v0.1.17.md](release-notes-v0.1.17.md). Their install commands are archival; use [install.md](install.md) for the current pure-Rust product.
