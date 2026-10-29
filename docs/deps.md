# Dependency Reference

This document records the purpose of each shared dependency in the root `Cargo.toml` `[workspace.dependencies]` table.

## Core & Storage Dependencies

| Crate | Purpose |
| --- | --- |
| `serde` (+ `derive`) | Typed serialization/deserialization for models, configs, and guardrail contracts |
| `serde_json` | JSON framing for MCP JSON-RPC 2.0, CLI payloads, status files, and contract fixtures |
| `serde_yaml` | Reading and writing `.cognis/config.yaml` |
| `thiserror` | Structured library error types (`CognisError`) |
| `anyhow` | Ergonomic error context in CLI and `xtask` binaries |
| `clap` (+ `derive`) | Command-line interface parsing for the multi-call `cognis` binary |
| `rusqlite` (`bundled`) | SQLite + FTS5 compiled from C source directly into the binary (zero system SQLite dependency) |
| `sha2` | SHA-256 hashing for symbol content hashes, release checksums, and hashed-argument audit logs |

## Tree-sitter Language Grammars (10 Code Languages)

| Crate | Language Coverage |
| --- | --- |
| `tree-sitter` | Incremental AST parser core |
| `tree-sitter-typescript` | TypeScript, TSX, JavaScript, JSX |
| `tree-sitter-python` | Python |
| `tree-sitter-go` | Go |
| `tree-sitter-c-sharp` | C# |
| `tree-sitter-java` | Java |
| `tree-sitter-rust` | Rust |
| `tree-sitter-c` | C |
| `tree-sitter-cpp` | C++ |
| `tree-sitter-ruby` | Ruby |
| `tree-sitter-php` | PHP |
| `regex` | AST normalization, artifact extraction, and secret-redaction patterns |

## Daemon & Process Ownership Dependencies

| Crate | Purpose |
| --- | --- |
| `notify` | Cross-platform file-system event watcher for `cognis indexd` |
| `ctrlc` | Graceful `SIGINT` / `SIGTERM` shutdown handling |

## Embedding Dependencies (Feature-Gated behind `onnx` / `onnx-download`)

| Crate | Purpose |
| --- | --- |
| `ort` | Native ONNX Runtime execution for local `bge-small-en-v1.5` embeddings |
| `tokenizers` | Fast BERT WordPiece tokenization (`tokenizer.json`) |

## Test & Benchmark Dependencies

| Crate | Purpose |
| --- | --- |
| `proptest` | Property-based testing (Complexity Semiring laws, CSAR Theorems 1–5, CSIA monotonicity, store round-trips) |
| `criterion` | Micro- and end-to-end latency benchmarks (`cargo bench`) |
| `tempfile` | Isolated temporary directories and SQLite databases in tests |

## Dependency Policy

1. Prefer minimal, audited, pure-Rust or self-contained bundled dependencies so `cognis` remains a single portable binary.
2. Declare shared versions once in `[workspace.dependencies]`.
3. Gate optional heavy runtimes (such as ONNX Runtime) behind Cargo features so offline builds remain fast and deterministic.
