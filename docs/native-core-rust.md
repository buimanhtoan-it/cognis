# Native hot-path core — archived migration design (Rust)

> **Historical document.** The migration is complete: the shipped engine is now
> pure Rust. References below to Python, ctypes, wheels, or staged migration
> surfaces describe the old transition and are not current install guidance.
> Use [install.md](install.md) and [distribution.md](distribution.md).
>
> **Original status:** decided — **Rust** is the native core language (§1.2). Slice A
> landed and is **measured**: Rust CSAR kernel + ctypes bridge + parity test
> (6/6 pass) + benchmark showing **15–123× solver speedup at L1 = 0** vs the
> Python push (§12). **Evidence discipline applies** (per
> `docs/development-criteria.md`): every claim is tagged **proven**,
> **empirically supported (n=…)**, or **conjectured**.

## 1. Why this document exists

The request that started this: *"rewrite cognis in C++ to be the fastest, and
beat codebase-memory-mcp (CBM) by 10×."* This document is the honest engineering
answer — what is achievable, what is not, and the staged plan that gets the real
win without throwing away cognis's moat.

### 1.1 The premise correction (read this first)

**CONJECTURED-FALSE claim:** "C++ will make cognis 10× faster than CBM."

CBM is already **pure C**, single static binary, RAM-first, indexing 28M LOC in
~3 min with <1 ms structural queries. C++ vs C — both compile to native, both
use SQLite, both use tree-sitter. A language rewrite lets cognis **reach CBM's
speed class**; it will not make it *10× faster than well-written C*. Anyone
promising that is selling a number that physics does not support. Raw
index/query throughput is **CBM's home turf** (158 languages, single binary,
SLSA-3). Competing there to "win" is the wrong battle.

### 1.2 Where a defensible 10× actually lives

cognis's edge is **on-path retrieval quality per token**, where CBM is
structurally weak (per-node scoring; `trace_path` needs the exact symbol name up
front; **no diffusion**). The redefined, defensible targets:

| Target (the real "10×") | Metric | Instrument |
| --- | --- | --- |
| On-path flow recovered in one round trip | answer quality / tool-calls per task | Pillar 1 harness + a new CBM-vs-cognis task suite |
| Diffusion latency in the interactive regime | `diffuse_context` p50 | Pillar 2 (`make e2e-report`) |
| Distribution parity with CBM | single-artifact install, no Python required at runtime | new packaging gate |
| Cold-index throughput at scale | symbols/sec, sub-linear wall time | Pillar 4 |

> **Honest framing, unchanged from the engine's current stance:** cognis is "a
> local, mathematically-grounded retrieval engine, RRF-ranked, with structure as
> proven low-contamination on-path context." The C++ core makes that engine
> **fast and trivially distributable**; it does not change the Pillar-1 quality
> claim, which is governed independently by the objective PR-derived benchmark.

## 2. Goals and non-goals

**Goals**
- G1. Move the hot computational kernels to native code: graph build, CSAR
  forward-push, RRF/BM25 fusion, top-k selection, vector scan glue.
- G2. Ship a **single distributable artifact** (one native extension, and a
  PyInstaller/static path to a one-file binary) to close the distribution gap
  with CBM.
- G3. Bring `diffuse_context` and `discover_symbols` into the sub-/low-ms
  interactive regime on real repos.
- G4. **Preserve the moat**: the Python eval/property-test/benchmark harness
  keeps running and verifies the native core (theorems T1–T5, golden sets).

**Non-goals**
- N1. "Beat C by 10× on raw speed." Rejected as unphysical (§1.1).
- N2. Rewriting the embedder / model layer in C++. The embedder is now a native
  Rust `Embedder` trait with an `onnx-local` backend (`bge-small-en-v1.5` via the
  `ort` crate) — swappable by design, a *differentiator* to keep.
- N3. ~~Rewriting eval, benchmark mining, PBT, or the VS Code extension.~~
  *(Superseded: the full rust-engine-migration ported eval/benchmark/PBT to Rust
  — `cognis-eval` + `proptest` — and removed Python entirely. Only the VS Code /
  Cursor extension remains TypeScript.)*
- N4. A from-scratch rewrite in one shot. We use a strangler-fig migration with
  a Python oracle at every step (§6).

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  Python surface (UNCHANGED public behavior)                        │
│  cognis-cli · cognis-indexd · cognis-mcpd · eval · benchmark · PBT │
│  embedder/reranker registry (PyTorch)   ← stays Python (N2)        │
└───────────────▲───────────────────────────────┬───────────────────┘
                │ ctypes (C ABI)                 │ same protocols/dataclasses
┌───────────────┴───────────────────────────────▼───────────────────┐
│  cognis_core (Rust)  →  one shared lib (cdylib: .dll/.so/.dylib)   │
│  ├─ graph    CSR adjacency, symmetrized, cache-friendly            │
│  ├─ solver   forward-push PPR (CSAR)  ← slice A, DONE + measured   │
│  ├─ fusion   RRF, BM25 top-k, min-heap selection                   │
│  ├─ store    SQLite C API (read path), sqlite-vec glue             │
│  └─ index    tree-sitter C API (vendored grammars)  [later phase]  │
└────────────────────────────────────────────────────────────────────┘
```

### 3.1 What stays in Python and why
- **Embedding/reranking** (N2): model swap is a product differentiator; PyTorch
  is the right home.
- **Eval, benchmark mining, PBT** (N3, G4): the evidence harness *is* the moat.
  It must remain the independent oracle that grades the native core.
- **Orchestration** (CLI/daemon/MCP server loop): thin; the cost is in the
  kernels, not the glue. Port the loop only in the final single-binary phase.

### 3.2 Binding strategy: C-ABI `cdylib` + ctypes now, PyO3 later
Slice A uses a **plain C ABI** (Rust `#[no_mangle] pub extern "C"`, crate-type
`cdylib`) loaded via `ctypes` (`native/csar-rs/src/lib.rs` ↔
`cognis_retrieval/_native.py`). Rationale:
- **Decoupled from the CPython ABI** — works on Python 3.14 (current interpreter
  here) with no binding-library version dependency. **(empirically relevant:
  this box runs 3.14.5; PyO3/pybind11 wheels lag new CPython.)**
- Zero build-time Python headers; the `.dll` is a normal shared library.
- Trivial parity testing: pass numpy buffers as raw pointers.

`PyO3` + `maturin` becomes worthwhile once we expose richer types (resident
graph objects, batched calls) ergonomically — planned for Phase 3, behind the
same registry seam so call sites do not change.

## 4. Data model & memory layout

The native graph is **CSR** (compressed sparse row), built once per index epoch
and reused across queries:

```
indptr  : int32[n+1]      row pointers
indices : int32[nnz]      neighbor node ids (sorted per row → matches Python push order)
weights : float64[nnz]    edge confidences (symmetrized, parallel edges summed)
degree  : float64[n]      weighted column sums (isolated node → self-loop, degree 1)
```

This mirrors `build_code_graph` exactly (symmetrized, `dst_missing` excluded,
self-loops on isolated nodes) so results stay within tolerance of the reference.
Node ids (`<lang>:<path>:<qname>@<hash>`) stay Python-side; the native layer is
**index-based only** (int32), which is cache-friendly and ABI-trivial.

**Memory target (Pillar 4):** CSR for the Linux-kernel-scale graph (4.8M nodes,
7.7M edges in CBM's numbers) is ~`4·(n+1) + 4·2·E + 8·2·E + 8·n` bytes ≈ **a few
hundred MB**, released after each query batch. CONJECTURED until measured on a
large index.

## 5. Component plan (kernel by kernel)

| # | Component | Native? | Replaces | Parity oracle | Status |
| --- | --- | --- | --- | --- | --- |
| K1 | CSAR forward-push | ✅ **done + measured** | `approximate_ppr_push` | `test_csar.py` T5 + `test_csar_native.py` (6/6 pass) | **slice A: 15–123× @ L1=0** |
| K2 | CSR graph build (native, resident) | next | `build_code_graph` + marshalling | `TestBuildCodeGraph` | removes csr_ms cost |
| K3 | RRF fusion + top-k | planned | `fusion.py` | golden ordering snapshot | Phase 2 |
| K4 | BM25 / FTS5 read | planned (SQLite C API) | `lexical.py` | lexical hit parity | Phase 2 |
| K5 | sqlite-vec scan glue | planned | `semantic.py` scan | semantic hit parity | Phase 2 |
| K6 | tree-sitter index pass | planned | indexer parse/resolve | symbol/edge count parity | Phase 3 |
| K7 | MCP/CLI loop (single binary) | planned | daemon glue | MCP contract suite | Phase 4 |

Each kernel ships behind a feature flag and a pure-Python fallback (as K1 does),
so the engine is never left broken and a kernel can be reverted by config alone.

> **K4 embedding-runtime decision (rust-engine-migration Task 6.3, 2026-06-15).**
> The full-rewrite spec supersedes N2 (embedder stays Python) for the *shipped*
> product: the production embedder is native (no PyTorch at runtime). The runtime
> is **`ort` (ONNX Runtime)**, not `candle` — decided on parity fidelity (running
> the exact exported ONNX graph) over `candle`'s cleaner static link, with `ort`
> bundling via `onnx-download` to still hit the single-binary goal. Rationale,
> evidence tiers, and re-evaluation triggers:
> `docs/decisions/ADR-0001-embedding-runtime.md`. Speed axis is **conjectured**
> (model assets not downloadable offline); the bench harness
> (`crates/cognis-embed/benches/embed_latency.rs`) is the methodology to confirm.

## 6. Migration plan (strangler-fig, Python as oracle)

1. **Land the kernel in `native/` with a C ABI + ctypes bridge** (K1 done).
2. **Parity gate**: native vs Python within the algorithm's L1 tolerance, on the
   same property-test graphs. The Python implementation is the **oracle** —
   theorems T1–T5 stay verified against it (Pillar 3, G4).
3. **Benchmark gate**: `python` vs `native` apples-to-apples speedup on the real
   `requests` graph + synthetic scale-free sizes (`.benchmarks/native/`).
4. **Wire behind a flag** in `diffuse_seed_hits` only after parity + speedup
   both pass. Default stays Python until a kernel is proven.
5. **Promote** the flag default once a kernel passes parity on the full PBT
   suite and shows a real speedup, recorded with evidence tier in the bench log.
6. Repeat per kernel. Only Phase 4 (single binary) changes the distribution
   story; everything before it is a drop-in accelerator.

> This is the same discipline the engine already uses for retrieval methods:
> *every change leaves correct/accurate/efficient measurably better, or produces
> a sound negative result.* A kernel that does not beat Python after porting is
> reverted and recorded — not shipped.

## 7. Verification & parity (the moat stays intact)

- **Theorems (proven):** T1–T5 remain verified by `tests/unit/test_csar.py` and
  `tests/pbt/` against the Python oracle. The native kernel must reproduce the
  same L1-bounded estimates (`tests/unit/test_csar_native.py`).
- **Ranking parity:** golden ordering snapshots for fusion/top-k (K3) so native
  reordering is byte-identical or explained.
- **MCP contract:** the 8-tool output contract (`cognis.contract.MCP_TOOLS`,
  `diffuse_context` `on_path`/`ppr_score`) is unchanged — the native core is an
  implementation detail below the contract.
- **Pillar 1 untouched:** retrieval *quality* is still graded by the objective
  PR-derived benchmark. The C++ core changes latency/footprint, not what is
  retrieved, so no Pillar-1 claim moves because of it.

## 8. Build & distribution

- **Dev build:** `pwsh -File native/build.ps1` (`cargo build --release`) → one
  `cdylib` staged at `native/build/csar_native.dll`.
- **Wheel:** ship the prebuilt lib inside the wheel per-platform (maturin or
  cibuildwheel matrix: win-amd64, linux-{amd64,arm64}, darwin-{amd64,arm64}); `_native.py`
  finds it next to the package as well as in `native/build/`.
- **Single binary (Phase 4, G2):** PyInstaller one-file for the daemon/MCP, or a
  fully native CLI for the structural subset, to match CBM's "download → run"
  story. This is the concrete answer to the distribution gap, which is cognis's
  **largest real disadvantage vs CBM** — bigger than any speed gap.

## 9. Risk register

| Risk | Severity | Mitigation |
| --- | --- | --- |
| No compiler on dev machine | resolved | Rust via rustup `x86_64-pc-windows-gnu` — user-space, no admin, no MSVC; installed and building |
| Python 3.14 toolchain immaturity | medium | C-ABI `cdylib` + ctypes avoids PyO3/pybind11 ABI coupling |
| Float-order divergence native vs Python | low | identical op order + LIFO worklist + sorted CSR; graded by L1 tolerance, not bit-equality |
| Rewrite scope creep → moat erosion | **high** | strangler-fig + Python oracle (§6); N2/N3 hard non-goals |
| "10× faster than C" expectation | high (managerial) | §1.1 correction; success redefined in §1.2 measurable terms |
| sqlite-vec / tree-sitter C-API integration cost | medium | phase-gated (K5/K6); fall back to Python path per flag |

## 10. Phased roadmap with gates

| Phase | Scope | Gate to advance |
| --- | --- | --- |
| **A (done in source)** | CSAR kernel + bridge + parity test + bench | code compiles; parity test passes; bench prints `python` vs `native` |
| **1** | Measure K1 speedup (**done**); wire K1 behind a flag | ✅ native parity L1=0 on PBT; ✅ speedup measured (15–123×, tier-tagged §12); flag-wire + MCP contract green = remaining |
| **2** | K3/K4/K5 (fusion, BM25, vec glue) | per-kernel parity + speedup; Pillar-2 `diffuse_context` p50 improves, no Pillar-1 change |
| **3** | K6 native index pass; PyO3 for rich/resident types | symbol/edge count parity on `requests`; cold-index throughput ≥ Python (Pillar 4) |
| **4** | Single-artifact distribution (G2) | one-file install on 5 platforms; packaging e2e gate green |

## 11. Next action

Slice A is **done and measured** (§12). Reproduce any time with the Rust
workspace (no Python toolchain — the engine is pure-Rust):

```powershell
cargo test -p cognis-csar          # CSAR theorems T1–T5 + solver parity
cargo bench -p cognis-eval --bench diffuse_latency   # kernel / diffuse latency
```

The next kernel is **K2 (native, resident graph build)** — it removes the
one-time Python→CSR marshalling (`csr_ms`, the only place native loses at large
`n` today), so the solver win holds end-to-end. After K2, wire K1+K2 behind a
config flag in `diffuse_seed_hits` (§6 step 4) and confirm the MCP contract +
Pillar-1 quality are unchanged before promoting the default.

## 12. Slice-A measured result (Rust kernel, this machine)

`alpha=0.15 eps=1e-5`, forward-push, seed = 5 nodes. **Solver time excludes the
one-time CSR marshalling** (`csr_ms`); `L1` = estimate diff vs the Python
reference. Full table in `native/README.md`.

| graph | n | edges | python_ms | rust_solver_ms | speedup | csr_ms | L1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| requests.db (real) | 320 | 2,678 | 259.7 | 2.18 | **119×** | 0.6 | 0.0 |
| scale-free | 1,000 | 7,968 | 247.2 | 2.01 | **123×** | 2.0 | 0.0 |
| scale-free | 10,000 | 79,968 | 88.4 | 2.63 | **34×** | 26.0 | 0.0 |
| scale-free | 100,000 | 799,968 | 38.5 | 2.55 | **15×** | 378.6 | 0.0 |

- **The Rust solver is 15–123× faster than the Python push, same algorithm,
  `L1 = 0` (bit-exact parity).** *(empirically supported, n=4 graphs incl. 1
  real; single machine.)* 6/6 parity tests pass.
- **Solver ~2 ms flat regardless of `n`** — matches the proven size-independent
  work bound `1/(α·ε)` (Theorem 5c). *(empirically supported; consistent with proven T5c.)*
- **Honest caveat:** `csr_ms` (pure-Python CSR marshalling) grows with edges
  (379 ms at n=100k). It is eliminated by K2 (native resident graph); until then
  a naive per-query rebuild would erase the win at large `n`. The design keeps
  the native graph resident across queries precisely for this reason (§3).
- The theorems (T1–T5) remain **proven** independently; this is a statement
  about kernel latency, not about retrieval quality (Pillar 1 unchanged).

### 12.1 Criterion latency harness (rust-engine-migration Task 9.3)

The §12 numbers above are the recorded slice-A baseline. The runnable Rust-side
methodology that keeps them honest is
`crates/cognis-eval/benches/diffuse_latency.rs` (Criterion, `harness = false`).
It measures the real Rust paths on the current machine — it never fabricates a
timing — across three groups: `csar_kernel/forward_push` (the §12
`rust_solver_ms` quantity), `diffuse_context_resident` (seed-build + push +
ranking over a resident graph), and `diffuse_context_end_to_end`
(`build_code_graph` (native K2) + diffuse, the **Requirement 11.2** p50 path that
folds in the cost the Python reference paid per query as `csr_ms`). It prints the
recorded Python/Pillar-2 baseline as context so the measured Rust p50 is read
against it.

Graphs are a deterministic Barabási–Albert synthetic (sizes lined up with the
§12 rows; offline-safe), or a **real** `.cognis/uckg.db` via `COGNIS_DIFFUSE_DB`
for an apples-to-apples p50 on the same DB. Because the synthetic topology is not
bit-identical to the numpy reference, the cross-language comparison is **by size
class** — label any result **empirically-supported (n=…)** / **conjectured**,
never **proven**.

```powershell
cargo bench -p cognis-eval --bench diffuse_latency
$env:COGNIS_DIFFUSE_DB=".cognis/uckg.db"; cargo bench -p cognis-eval --bench diffuse_latency
```

A first run on this machine (short-sample, single box) measured the
forward-push kernel and `diffuse_context` at low-single-digit milliseconds
(p50 ≈ 2.7 ms at n=320, ≈ 4 ms at n=1000), versus the recorded Python push of
259.7 / 247.2 ms at the same sizes — consistent with Requirement 11.2 (Rust p50
not worse than the Python Pillar-2 baseline; the native graph build keeps the
win end-to-end). *(empirically-supported, single machine; topology by size class
vs the §12 reference — not proven.)*
