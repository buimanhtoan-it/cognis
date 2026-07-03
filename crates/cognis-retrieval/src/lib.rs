//! cognis-retrieval — retrieval mesh + RRF + capsule.
//!
//! Task 5 lands `Hit`, the `RetrievalLayer` trait, lexical/semantic layers,
//! rank-based `rrf_fuse` (byte-identical to `fusion.py`, Requirement 4.1), and
//! the capsule composer (CSAR on-path add, monotone — Requirement 4.4).
//!
//! Task 5.2: [`rrf_fuse`] — rank-based, scale-invariant Reciprocal Rank Fusion
//! whose fused top-k is byte-identical to the Python oracle (`fusion.py`) on the
//! same seed set. See `tests/fusion_parity.rs`.
//!
//! Task 5.3 (this slice): [`compose_capsule`] — the capsule composer. Fuses the
//! confident lexical/semantic layers with [`rrf_fuse`], then *additively* unions
//! CSAR on-path context (dedup per-symbol, RRF order preserved as a prefix). The
//! union is monotone: it never drops a confident hit (`recall ≥ direct prefix`,
//! Requirement 4.4 / Property 3). See `tests/capsule_monotonicity.rs`.
pub use cognis_core::{Hit, Result};

pub mod capsule;
pub mod fusion;
pub mod layer;
pub use capsule::{compose_capsule, compose_capsule_ids};
pub use fusion::{rrf_fuse, rrf_fuse_ids, DEFAULT_RRF_K};
pub use layer::{LexicalLayer, RetrievalLayer, SemanticLayer};
