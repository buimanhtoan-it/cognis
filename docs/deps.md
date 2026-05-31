# Dependency Reference

This document records why each runtime and development dependency exists. Use it
when reviewing dependency additions or deciding whether a dependency still
belongs in the project.

## Core runtime dependencies

| Dependency | Purpose |
| --- | --- |
| `pydantic>=2.7` | Typed configuration loading and schema validation |
| `pyyaml>=6.0` | Reading `.cognis/config.yaml` |
| `click>=8.1` | Command-line interface for `cognis-cli` |

## Optional feature dependencies

| Extra | Dependency | Purpose |
| --- | --- | --- |
| `indexer` | `tree-sitter`, `watchdog` | Parsing source files and watching the file system |
| `indexer` | `tree-sitter-python>=0.21` | Python grammar support |
| `indexer` | `tree-sitter-typescript>=0.21` | TypeScript grammar support |
| `indexer` | `tree-sitter-go>=0.21` | Go grammar support |
| `embed-local` | `sentence-transformers`, `numpy` | Local embedding generation |
| `vector` | `sqlite-vec` | Vector similarity search |
| `mcp` | `fastmcp` | MCP server runtime |
| `tokenizers` | `tiktoken` | Token counting for capsule budgeting |

## Development dependencies

| Dependency | Purpose |
| --- | --- |
| `ruff>=0.6` | Formatting and linting |
| `mypy>=1.11` | Static type checking |
| `pytest>=8.2` | Test runner |
| `pytest-asyncio>=0.23` | Async test support |
| `pytest-benchmark>=4.0` | Benchmark and latency checks |
| `hypothesis>=6.108` | Property-based testing |
| `pre-commit>=3.7` | Local hook runner |
| `invoke>=2.2` | Cross-platform task runner |

## Dependency policy

When adding a dependency:

1. prefer the smallest viable dependency surface
2. document the reason here
3. keep optional features behind extras where possible
4. avoid overlapping tools unless there is a clear operational reason
