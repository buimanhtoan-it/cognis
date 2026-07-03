# Model assets — `onnx-local` embedder

The `cognis-embed` `onnx-local` backend (spec task 6.2 / Requirement 7.2) loads
its model from this directory tree. The model binaries are **not committed** (see
`.gitignore`); they are produced by a one-time, developer-only export step.

## Expected layout

```
assets/models/bge-small-en-v1.5/
  model.onnx        # exported transformer (inputs: input_ids, attention_mask,
                    #   token_type_ids; output: last_hidden_state)
  tokenizer.json    # HuggingFace fast tokenizer (loaded by the `tokenizers` crate)
  pooling.json      # {"pooling": "cls"|"mean", "normalize": true}
```

The directory is resolved by `cognis_embed::build_embedder` from
`embedder.model` in `.cognis/config.yaml`: it takes the segment after the last
`/` (`BAAI/bge-small-en-v1.5` → `bge-small-en-v1.5`) under `assets/models/`. Set
the `COGNIS_ONNX_MODEL_DIR` environment variable to override the location.

## Obtaining the assets

The three files above are the standard prebuilt `BAAI/bge-small-en-v1.5` ONNX
export. They are large binaries kept out of git (see `.gitignore`); drop the
prebuilt artifacts into `assets/models/bge-small-en-v1.5/` (or point
`COGNIS_ONNX_MODEL_DIR` at wherever you keep them). No Python toolchain is
involved — the engine is pure-Rust and loads these files directly via the `ort`
crate.

`pooling.json` records the model's real pooling decision (bge-small uses **CLS**
pooling + L2 normalize), so the Rust backend pools identically to the reference.
When `pooling.json` is absent the backend defaults to CLS + L2 (bge's actual
mode).

## Runtime note

The `onnx` cargo feature uses ONNX Runtime via `ort`'s `load-dynamic` strategy:
the ONNX Runtime shared library is resolved at runtime. For a fully
self-contained binary that links the runtime statically, build with
`--features onnx-download` instead (downloads a prebuilt ONNX Runtime at build
time).
