# Cognis Documentation

User, operator, and contributor reference for **Cognis `v0.1.0`**.

## Getting Started

| Document | Purpose |
| --- | --- |
| [install.md](install.md) | Install via GitHub Release / VSIX or build the Rust engine & extension from source |
| [quickstart.md](quickstart.md) | Index a repository, verify health, start `cognis mcpd`, and run your first guardrail & retrieval queries |
| [mcp-client-config.md](mcp-client-config.md) | Configure Antigravity, Cursor, VS Code, Claude Code/Desktop, and Kiro; workspace scope, thin-proxy topology, and all 12 MCP tools |
| [troubleshooting-huggingface.md](troubleshooting-huggingface.md) | Resolve local ONNX embedding model (`bge-small-en-v1.5`) setup issues |

## Editor Extension

| Document | Purpose |
| --- | --- |
| [../apps/cognis-mcp/README.md](../apps/cognis-mcp/README.md) | Live Big-O CodeLens, Guardrail Diagnostics, settings (`cognis.guardrails.*`), and workspace setup in VS Code & Cursor |

## Architecture & Math

| Document | Purpose |
| --- | --- |
| [architecture.md](architecture.md) | Workspace crate layout, 4 runtime surfaces (`cli`, `mcpd`, `indexd`, `owner`), Shadow AST complexity analyzer, UCKG SQLite store, and 12 MCP tools |
| [csar.md](csar.md) | Mathematical foundations of **CSAR** (forward-push retrieval) and **CSIA** (loop-amplified reverse-push change impact analysis) |
| [deps.md](deps.md) | Rust crate dependency inventory and policy |

## Operations, Quality & Security

| Document | Purpose |
| --- | --- |
| [performance.md](performance.md) | Latency budgets (`< 10 ms` Shadow AST, CSIA/CSAR benchmarks), caching, and profiling |
| [security.md](security.md) | Threat model, untrusted-content markers, secret redaction, MCP concurrency/timeout limits, loopback isolation, and hashed audit log |
| [observability.md](observability.md) | Extension diagnostics trace (`diagnostics.jsonl`), capability handshake, metrics, and `.cognis/audit.log` |
| [e2e-testing.md](e2e-testing.md) | Cross-process E2E tests, JSON contract parity, and full-stack VS Code host testing |
| [release.md](release.md) | Version lockstep, verification gates, tag-driven GitHub Releases, and VSIX packaging |
