# Embedder Troubleshooting

Use this guide when semantic indexing or search is not working as expected.

## The default backend does no network download

The production embedder is `onnx-local`: it loads `bge-small-en-v1.5` from the
checked-in `assets/models/` directory (or `COGNIS_ONNX_MODEL_DIR`) via the `ort`
ONNX Runtime crate. There is **no Hugging Face download at runtime** and no
Python — so the classic 401 / "Repository Not Found" download failures no longer
apply.

Relevant `.cognis/config.yaml`:

```yaml
embedder:
  backend: onnx-local
  model: BAAI/bge-small-en-v1.5
```

The directory is resolved from the segment after the last `/`
(`BAAI/bge-small-en-v1.5` → `assets/models/bge-small-en-v1.5/`). See
[../assets/models/README.md](../assets/models/README.md) for the expected
layout (`model.onnx`, `tokenizer.json`, `pooling.json`).

## Symptom: semantic search returns nothing

1. **Model assets missing.** If `assets/models/bge-small-en-v1.5/` (or
   `COGNIS_ONNX_MODEL_DIR`) does not contain `model.onnx` + `tokenizer.json`, the
   `onnx-local` backend cannot load. Drop the prebuilt ONNX export into that
   directory, then re-index.
2. **Index built without embeddings.** If you indexed with `--skip-embeddings`,
   the vector table is empty. Re-index without the flag:
   ```bash
   cognis index --full .
   ```
3. **Offline / stub backend.** The `stub` backend returns zero vectors (fully
   offline); lexical and structural retrieval still work, but semantic search is
   degraded until you switch to `onnx-local` with assets present.

## Temporary workaround: skip embeddings

To get indexing working immediately without the model, defer embeddings:

```bash
cognis bootstrap . --skip-embeddings
```

Lexical and structural retrieval will still work. Re-index without
`--skip-embeddings` once the ONNX assets are in place to enable semantic search.
