//! cognis-eval — quality oracle + parity harness.
//!
//! Task 9 lands the golden-set runner, the differential parity harness (same
//! query on Python-built vs Rust-built UCKG — Requirement 10.3), and the
//! reproduced fair-harness benchmark gating Pillar-1 quality (Requirement 6).
//!
//! Task 9.1 (this slice): the [`parity`] module — a differential parity harness
//! that runs the *same* query against a Python-built and a Rust-built UCKG and
//! asserts design **Property 2** (CSAR estimate `L1 < 1e-9`, RRF top-k
//! byte-identical, lexical/semantic hit sets identical on the same DB). See
//! `tests/differential_parity.rs`.
//! Task 9.2 (this slice): the [`bench`] module — the reproduced **fair-harness
//! benchmark** on the objective PR-derived golden key. It computes Recall@k /
//! MRR / Contamination@k for the engine's retrieval surfaces and exposes the
//! [`bench::RegressionGate`] that asserts the Rust engine does not regress
//! versus the captured Python oracle (design **Property 5**, Requirements 6.1 /
//! 6.2 — the gate that blocks removing Python at K8). See `tests/fair_harness.rs`.
pub use cognis_core::Result;

pub mod bench;
pub mod parity;
