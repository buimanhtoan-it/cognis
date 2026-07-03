# native/ — Rust hot-path core for cognis

This directory holds `csar-rs`, the standalone Rust port of cognis's hottest
computational kernel (forward-push Personalized PageRank). It builds as a plain
**C-ABI shared library** (`cdylib`) and exists as the historical parity bridge:
the kernel it proves out was promoted into the in-workspace
[`cognis-csar`](../crates/cognis-csar) crate, which is what the engine actually
uses today. `csar-rs` is excluded from the main Cargo workspace (it sets its own
release profile) and is kept here as a self-contained reference + benchmark.

Rust over C++: same runtime speed class (both LLVM-native), but memory safety,
fearless parallelism, single-static-binary by default, and a user-space
toolchain (no admin / no MSVC). See
[`docs/native-core-rust.md`](../docs/native-core-rust.md) §1.2.

## Current contents

| Path | What |
| --- | --- |
| `csar-rs/Cargo.toml`, `csar-rs/src/lib.rs` | Rust forward-push PPR kernel (C ABI, `cdylib`) |
| `build.ps1` | `cargo build --release` → stages `build/csar_native.dll` |

The production kernel now lives in [`crates/cognis-csar`](../crates/cognis-csar)
(pure-Rust, in-workspace). Its parity is asserted by
`crates/cognis-csar/tests/solver_parity.rs` against a checked-in golden — no
external runtime required.

## Build

cognis's native core is **Rust** (see
[`docs/native-core-rust.md`](../docs/native-core-rust.md) §1.2 for why Rust over
C++). The Rust toolchain installs **user-space, no admin** — on this machine it
is already installed (`x86_64-pc-windows-gnu`, no MSVC needed):

```powershell
# one-time, if not present (user-space, no admin):
Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe -OutFile $env:TEMP\rustup-init.exe
& $env:TEMP\rustup-init.exe --default-host x86_64-pc-windows-gnu --default-toolchain stable --profile minimal -y
```

Build + stage the library:

```powershell
pwsh -File native/build.ps1        # cargo build --release -> native/build/csar_native.dll
```

Verify parity (the kernel lives in the workspace crate now):

```powershell
cargo test -p cognis-csar          # solver parity vs the checked-in golden
```

## Measured result (slice A, Rust kernel, this machine)

Wall-clock, `alpha=0.15 eps=1e-5`, forward-push, seed = 5 nodes. **Solver time
excludes the one-time CSR marshalling** (reported as `csr_ms`); `L1` is the
estimate difference vs the original reference solver.

| graph | n | edges | reference_ms | rust_solver_ms | speedup | csr_ms | L1 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| requests.db (real) | 320 | 2,678 | 259.7 | **2.18** | **119×** | 0.6 | 0.0 |
| scale-free | 1,000 | 7,968 | 247.2 | **2.01** | **123×** | 2.0 | 0.0 |
| scale-free | 10,000 | 79,968 | 88.4 | **2.63** | **34×** | 26.0 | 0.0 |
| scale-free | 100,000 | 799,968 | 38.5 | **2.55** | **15×** | 378.6 | 0.0 |

What this proves (machine-verified on this box):

1. **The Rust solver is 15–123× faster than the original reference push, same
   algorithm, `L1 = 0` (bit-exact parity).** *(empirically supported, n=4 graphs
   incl. 1 real; single machine.)* The carried-over parity gate now lives in
   `crates/cognis-csar/tests/solver_parity.rs` (estimate L1 < 1e-9, approximates
   exact PPR, work bound holds).
2. **Solver time is ~2 ms flat regardless of `n`** (2.0–2.6 ms across 320 →
   100,000 nodes), matching the proven size-independent work bound `1/(α·ε)`
   (Theorem 5c). The reference solver varies 38–260 ms.
3. **Honest caveat — the CSR marshalling is the remaining cost.** `csr_ms` (the
   loop that builds CSR arrays from the graph) grows with edges (379 ms at
   n=100k). In *this slice* it is a one-time, per-index cost; in the **full
   native core it disappears** (the graph is built natively from the index and
   never marshalled). The design (native-core-rust.md §3) keeps the native graph
   resident across queries for exactly this reason.

This cleared slice A's gate: native parity (L1 < 1e-9) **and** a real, large
speedup — and the kernel has since been promoted into the `cognis-csar` crate.
