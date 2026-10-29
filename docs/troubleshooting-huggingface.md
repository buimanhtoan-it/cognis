# Local Embedder Troubleshooting

Use this guide if semantic indexing or `semantic_search` returns empty vector results.

> **Note:** Real-time AST complexity (`inspect_symbol_complexity`), `OPT-001..007` optimizations (`suggest_optimal_refactor`), Speculative Shadow AST + CSIA blast radius (`analyze_patch_impact`, `trace_blast_radius`), lexical search (`symbol_search`), and dependency tracing work **100% without ONNX model assets**. Only dense vector similarity (`semantic_search` and semantic legs) requires the local ONNX model.

## How Model Assets Are Loaded

The production embedder backend is `onnx-local` (`BAAI/bge-small-en-v1.5`):

- **Managed IDE install:** Clicking **Install engine** in the VS Code / Cursor Cognis panel downloads checksum-verified `model.onnx`, `tokenizer.json`, and `pooling.json` from the matching GitHub Release and sets `COGNIS_ONNX_MODEL_DIR` automatically.
- **Manual / source builds:** Place `model.onnx`, `tokenizer.json`, and `pooling.json` inside `assets/models/bge-small-en-v1.5/` or set `COGNIS_ONNX_MODEL_DIR` to the directory holding those three files (see [`../assets/models/README.md`](../assets/models/README.md)).

Relevant `.cognis/config.yaml` block:

```yaml
embedder:
  backend: onnx-local
  model: BAAI/bge-small-en-v1.5
```

## Common Symptoms & Fixes

1. **Model files missing:** Verify `COGNIS_ONNX_MODEL_DIR` (or `assets/models/bge-small-en-v1.5/`) contains `model.onnx` and `tokenizer.json`, then run `cognis index --full .`.
2. **Binary built without `--features onnx` / `onnx-download`:** Default `cargo build` compiles the fast offline binary without ONNX Runtime. Rebuild with `cargo build --release -p cognis --bin cognis --features onnx-download` for semantic embeddings.
3. **Workspace indexed with `--skip-embeddings`:** Re-run `cognis index --full .` without `--skip-embeddings` to populate vector rows.

## Fast Offline Setup Without Embeddings

When working completely offline without model weights, run:

```bash
cognis bootstrap . --skip-embeddings
```

All guardrail, complexity, blast-radius, FTS5 lexical, and call-graph tools operate at full speed.
