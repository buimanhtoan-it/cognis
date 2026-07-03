# cognis-embed test fixtures

## `bge_parity_golden.json` (optional, not committed by default)

Reference embeddings captured from `sentence-transformers` for the `onnx-local`
parity test (`tests/onnx_parity.rs`, spec task 6.2 / Requirement 7.2).

This is a **frozen oracle capture** — it was produced once against the reference
model and is treated as checked-in data. There is no toolchain in this repo to
regenerate it; the engine is pure-Rust and ships the ONNX model assets directly
under `assets/models/`.

The parity test **skips gracefully** when this fixture (or the ONNX model assets)
is absent, so `cargo test` stays green offline. See the module docs in
`tests/onnx_parity.rs` for details.

Shape:

```json
{
  "model": "BAAI/bge-small-en-v1.5",
  "dim": 384,
  "normalize": true,
  "cases": [ { "text": "...", "embedding": [/* 384 floats */] } ]
}
```
