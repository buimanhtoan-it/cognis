//! cognis-csar — Code Spreading-Activation Retrieval: code graph + PPR solvers.
//!
//! This crate promotes the proven forward-push Personalized PageRank kernel
//! (originally the C-ABI cdylib in `native/csar-rs`, measured 15–123× over the
//! Python reference with **L1 = 0** bit-exact parity, 6/6 parity tests) into a
//! normal Rust crate the retrieval layer can depend on directly — no FFI, no
//! per-query marshalling. It mirrors `cognis_retrieval.csar` (the Python oracle)
//! operation-for-operation so results stay within the algorithm's L1 tolerance.
//!
//! It exposes three PPR solvers over the resident CSR [`CodeGraph`]:
//!
//! * [`approximate_ppr_push`] — Andersen-Chung-Lang forward push (the
//!   size-independent solver; work bound `Σ d_u ≤ 1/(α·ε)`, T5c). This is the
//!   carried-over proven kernel.
//! * [`personalized_pagerank_exact`] — the closed-form linear solve
//!   `r = α·(I − (1−α)P)⁻¹·s`.
//! * [`personalized_pagerank_power`] — power iteration `r ← α·s + (1−α)P·r`
//!   (geometric convergence at rate `1 − α`, T2).
//!
//! and the shared [`diffuse_seed_hits`] entry point that builds a seed
//! distribution from per-layer [`Hit`]s, runs forward push, and returns on-path
//! context hits tagged with `on_path`/`ppr_score` evidence (the MCP
//! `diffuse_context` contract shape — Requirement 4.4: the structural layer must
//! never drop a confident seed hit).
//!
//! ## Where [`CodeGraph`] lives, and why
//!
//! The CSR [`CodeGraph`] is defined in **`cognis-core`**, not here. `cognis-store`
//! *produces* one (`SymbolStore::build_code_graph`, Task 3.5) and this crate
//! *consumes* one (Task 4.2); defining it in the dependency-neutral foundation
//! lets `store` and `csar` share a single type with **no dependency cycle**
//! (`cognis-csar → cognis-core`, never the reverse). This crate re-exports it so
//! the design's stated home (`cognis-csar`) still surfaces it.
//!
//! ## Note on signatures
//!
//! The design sketches `approximate_ppr_push(...) -> PushResult` and
//! `diffuse_seed_hits(...) -> Vec<Hit>`. We instead return [`Result`] from the
//! fallible solvers (invalid `alpha`/`eps`), matching the Python oracle's
//! `ValueError` semantics and the crate-wide error-handling rule ("library code
//! returns typed `Result`, never panics on data input" — design § Error
//! Handling). Callers use `?`; degenerate-but-valid input (empty graph, no seed
//! mass) still yields `Ok(empty)`.

mod diffuse;
mod push;
mod solvers;

pub use cognis_core::{CodeGraph, Hit, Result};

pub use diffuse::{build_seed_distribution, diffuse_seed_hits, DEFAULT_ALPHA, DEFAULT_EPS};
pub use push::{approximate_ppr_push, PushResult};
pub use solvers::{
    personalized_pagerank_exact, personalized_pagerank_power, transition_matrix, DEFAULT_MAX_ITER,
    DEFAULT_TOL,
};
