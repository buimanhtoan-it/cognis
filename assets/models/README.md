# Model Assets — `onnx-local` Embedder

The `cognis-embed` `onnx-local` backend loads its semantic embedding model from this directory tree (or from `COGNIS_ONNX_MODEL_DIR`). Large model binaries are kept out of Git via `.gitignore` and provisioned automatically at release or downloaded on first managed setup by the IDE extension.

## Expected layout

```text
assets/models/bge-small-en-v1.5/
  model.onnx        # exported transformer (inputs: input_ids, attention_mask,
                    #   token_type_ids; output: last_hidden_state)
  tokenizer.json    # fast WordPiece tokenizer (loaded by the `tokenizers` crate)
  pooling.json      # {"pooling": "cls"|"mean", "normalize": true}
```

`cognis_embed::build_embedder` resolves the directory from `embedder.model` in `.cognis/config.yaml` (`BAAI/bge-small-en-v1.5` -> `assets/models/bge-small-en-v1.5/`), unless overridden by `COGNIS_ONNX_MODEL_DIR`.

## Obtaining the assets

- **Managed IDE install:** Clicking **Install engine** in the Cognis VS Code / Cursor panel downloads the checksum-verified `model.onnx`, `tokenizer.json`, and `pooling.json` assets matching the extension version and configures `COGNIS_ONNX_MODEL_DIR`.
- **Manual / CLI setup:** Place the prebuilt `Xenova/bge-small-en-v1.5` ONNX files into `assets/models/bge-small-en-v1.5/` or point `COGNIS_ONNX_MODEL_DIR` at your model directory.

`pooling.json` specifies CLS pooling + L2 normalization (`{"pooling":"cls","normalize":true}`). When absent, the backend defaults to CLS + L2.

## Build features

- `--features onnx` uses ONNX Runtime via `ort`'s dynamic loading strategy.
- `--features onnx-download` statically links prebuilt ONNX Runtime at build time to produce a self-contained single binary.
