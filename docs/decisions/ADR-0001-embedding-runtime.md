# ADR-0001 — Embedding runtime: `ort` (ONNX Runtime) vs `candle`

- **Status:** Accepted
- **Date:** 2026-06-15
- **Context:** `rust-engine-migration` spec, **Open Question 1**, Task 6.3 (K4 —
  `cognis-embed`).
- **Decision drivers:** Requirement 7.2 (production embedder runs
  `bge-small-en-v1.5` native, no Python/PyTorch), Requirement 8 (single static
  binary, no system deps), the **HIGH**-severity *embedding-parity* risk in
  design.md, and `docs/development-criteria.md` evidence discipline.

> **Evidence discipline.** Every claim below is tagged **proven** /
> **proven-by-construction** / **empirically-supported (n=…)** / **conjectured**.
> The model assets for `bge-small-en-v1.5` cannot be downloaded in this offline
> environment, so **no real latency or parity numbers were produced for this
> ADR**. The speed axis is therefore **conjectured**; the parity and
> distribution axes are decided on architectural grounds (proven-by-construction)
> plus the runnable methodology shipped alongside (the parity test and the
> `embed_latency` bench). This ADR does not fabricate numbers.

## Decision

**Adopt `ort` (the [`ort`](https://crates.io/crates/ort) crate over Microsoft's
ONNX Runtime) as the production embedding runtime for `cognis-embed`.** This is
the backend already implemented in Task 6.2 (`onnx-local`, `crates/cognis-embed/
src/onnx.rs`): the exported ONNX graph + the `tokenizers` crate + CLS pooling +
L2 normalise.

`candle` is **not** adopted now but is kept as the documented fallback with
explicit re-evaluation triggers (below).

## Options considered

### A. `ort` — ONNX Runtime via the `ort` crate (CHOSEN)

Runs the exact `model.onnx` that `optimum`/`sentence-transformers` export for the
model.

- **Parity fidelity — decisive advantage.** `ort` executes the *same computation
  graph* the reference (`sentence-transformers`) produces; the only divergence
  sources are floating-point op order and the tokenizer, both already controlled
  (the model's own `tokenizer.json`; pooling read from the asset, not
  hard-coded). This directly minimises the HIGH-severity embedding-parity risk.
  *(proven-by-construction: same graph ⇒ same math up to fp order; the cosine ≈
  1.0 gate in `tests/onnx_parity.rs` confirms it empirically once assets exist.)*
- **Maturity / operator coverage.** ONNX Runtime is a production-grade engine
  with full coverage of the BERT operator set bge-small uses. *(empirically
  well-established in the ecosystem; not benchmarked here.)*
- **Speed.** ONNX Runtime is generally fast on CPU transformer inference (graph
  optimisation, threading). *(conjectured for this model/machine — `benches/
  embed_latency.rs` is the harness to confirm; no number is claimed.)*
- **Cost — native dependency.** `ort` needs the ONNX Runtime shared library.
  Two mitigations already wired in `Cargo.toml`:
  - `--features onnx` → `ort/load-dynamic`: builds offline (no build-time
    download); resolves the shared lib at runtime. Good for dev.
  - `--features onnx-download` → `ort/download-binaries`: bundles a prebuilt
    ONNX Runtime so the shipped artifact is **self-contained** (the Requirement 8
    path). Cost: larger binary and more friction cross-compiling via `cross`.
    *(conjectured feasibility per target platform until Task 10 measures it.)*

### B. `candle` — pure-Rust (NOT chosen now)

- **Distribution — its one clean win.** Pure Rust, static link, no native
  dependency → the cleanest answer to Requirement 8 / G2. *(proven-by-construction.)*
- **Parity risk — the reason to defer.** `candle` does not run the export graph
  directly. Either route adds risk:
  - `candle-onnx` ONNX import has **partial operator coverage**; the bge graph
    is not guaranteed to import cleanly. *(conjectured — would need a real import
    attempt to confirm.)*
  - a hand-ported `candle` BERT loading the HF weights is a **second
    implementation** that must be validated against `sentence-transformers`
    independently — exactly the parity surface design.md flags as HIGH risk.
- **Speed.** Competitive but typically somewhat slower than ONNX Runtime for
  CPU BERT inference. *(conjectured; would be measured by the same bench.)*
- **Maturity.** Newer than ONNX Runtime; actively developed.

## Rationale

The deciding axis is **parity fidelity**, not raw speed. Requirement 7.2 makes
the production runtime gate on reproducing the `sentence-transformers` vectors
(cosine ≈ 1.0), and design.md ranks embedding parity as a HIGH risk. `ort` runs
the *same ONNX graph* as the reference, so parity is a property of the export,
not of a re-implementation — the lowest-risk path to the gate. `candle`'s
distribution advantage is real but is the *second* priority here, and `ort` can
still satisfy Requirement 8 by bundling ONNX Runtime via the existing
`onnx-download` feature (heavier, but self-contained). Choosing `candle` would
trade the highest-severity risk (parity) for a lower-severity convenience
(static link), which inverts the spec's risk ordering.

## Re-evaluation triggers (when to revisit `candle`)

Revisit and likely switch to `candle` if **any** of these hold at Task 10
(single-binary distribution) or later:

1. Bundling ONNX Runtime via `onnx-download` proves infeasible or unacceptably
   painful for one of the five target platforms (macOS arm64/amd64, Linux
   arm64/amd64, Windows amd64) — e.g. cross-compile via `cross` cannot link it.
2. Final binary **size** becomes a distribution blocker versus the CBM
   "download → run" bar (`native-core-rust.md` §1.2).
3. `candle-onnx` reaches full bge operator coverage (so the *same graph* can run
   on pure Rust) **or** a `candle` BERT backend passes the same cosine ≈ 1.0
   parity gate as `ort` — at which point `candle` dominates (parity *and* clean
   static link).

The runtime is chosen once, behind the `build_embedder` factory and the
`Embedder` trait (Requirement 7.1), so a future `candle-local` backend is an
additive registry entry — no retrieval/indexer call site changes.

## Consequences

- The `onnx-local` (`ort`) backend stays the production default; the `stub`
  zero-vector backend remains the offline/degradation target.
- Distribution work (Task 10) targets `--features onnx-download` and must
  validate the static-bundle path per platform; failure there is an explicit
  trigger to reconsider `candle` (above), not a silent fallback.
- A `candle` comparison, if pursued, plugs into `benches/embed_latency.rs` and
  the parity fixture with identical inputs for an apples-to-apples result.

## Verification status

- **Verified (this task):** default workspace build/test/clippy/fmt green; the
  `embed_latency` bench compiles under `--features onnx` and is excluded from the
  default build (`required-features`); the bench and parity test both
  graceful-skip with no assets (no fabricated numbers).
- **Deferred (needs model assets / network):** real cosine-parity numbers
  (`tests/onnx_parity.rs`) and real `ort` (and any future `candle`) latency
  numbers (`benches/embed_latency.rs`). Until then the speed axis stays
  **conjectured** and the parity axis rests on the same-graph argument
  (proven-by-construction) pending the empirical run.
