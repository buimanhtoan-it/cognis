# Dependency Reference

This document records why each dependency exists. Use it when reviewing
dependency additions or deciding whether a dependency still belongs in the
project. The engine is a pure-Rust Cargo workspace; shared dependencies are
declared once in the root `Cargo.toml` `[workspace.dependencies]` and each crate
opts in as needed.

## Core dependencies

| Dependency | Purpose |
| --- | --- |
| `serde` (+ derive) | Typed (de)serialization for config, contracts, and store models |
| `serde_json` | JSON for MCP/CLI contracts, golden fixtures, status files |
| `serde_yaml` | Reading/writing `.cognis/config.yaml` |
| `thiserror` | Library error types (`CognisError`) |
| `anyhow` | Application-level error context in bins |
| `clap` (+ derive) | Command-line parsing for the `cognis` binary and its surfaces |
| `rusqlite` (`bundled`) | SQLite (with FTS5) compiled from source into the binary — no system SQLite |
| `sha2` | SHA-256 for node-id hashing and the hashed-argument audit log |

## Indexer dependencies

| Dependency | Purpose |
| --- | --- |
| `tree-sitter` | Parser core for the indexer AST pass |
| `tree-sitter-python` | Python grammar |
| `tree-sitter-typescript` | TypeScript / JavaScript grammar |
| `tree-sitter-go` | Go grammar |
| `tree-sitter-java` | Java grammar |
| `tree-sitter-c-sharp` | C# grammar |
| `regex` | AST-text normalization in the parser stage |

## Daemon dependencies

| Dependency | Purpose |
| --- | --- |
| `notify` | File-system watch loop for `cognis indexd` incremental indexing |
| `ctrlc` | Cross-platform Ctrl-C / SIGTERM handling for clean daemon shutdown |

## Embedding dependencies (feature-gated)

| Dependency | Purpose |
| --- | --- |
| `ort` (ONNX Runtime) | Native `bge-small-en-v1.5` inference for the `onnx-local` embedder — no Python/PyTorch at runtime (behind the `onnx` feature) |
| `tokenizers` | The model's BERT WordPiece tokenizer (`tokenizer.json`) |

## Development dependencies

| Dependency | Purpose |
| --- | --- |
| `proptest` | Property-based testing (CSAR theorems, round-trips) |
| `criterion` | Benchmark / latency harness (`cargo bench`) |
| `tempfile` | Temp DB copies in parity/integration tests |

## Tooling

Formatting, linting, type/borrow checking, and testing are all provided by the
Rust toolchain itself — no separate tools to install:

| Command | Purpose |
| --- | --- |
| `cargo fmt --all` | Formatting (`--check` in CI) |
| `cargo clippy --workspace --all-targets` | Linting |
| `cargo test --workspace` | Unit + property + parity tests |
| `cargo llvm-cov --workspace` | Coverage |
| `cargo xtask dist` | Build + stage the single-binary distribution |

## Dependency policy

When adding a dependency:

1. prefer the smallest viable dependency surface
2. document the reason here
3. declare shared deps in `[workspace.dependencies]`; gate optional ones behind
   cargo features where possible
4. avoid overlapping crates unless there is a clear operational reason
