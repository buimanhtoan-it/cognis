//! cognis-csar — Code Spreading-Activation Retrieval & Code Spreading-Impact Analysis (CSIA).

mod diffuse;
pub mod impact;
mod push;
mod solvers;

pub use cognis_core::{CodeGraph, Hit, ImpactGraph, Result};

pub use diffuse::{build_seed_distribution, diffuse_seed_hits, DEFAULT_ALPHA, DEFAULT_EPS};
pub use impact::{
    evaluate_change_impact, is_entrypoint_symbol, is_test_symbol, SymbolImpactSeed,
    CSIA_BEHAVIORAL_ALPHA, CSIA_COMPLEXITY_ALPHA, CSIA_DEFAULT_EPS,
};
pub use push::{approximate_ppr_push, PushResult};
pub use solvers::{
    personalized_pagerank_exact, personalized_pagerank_power, transition_matrix, DEFAULT_MAX_ITER,
    DEFAULT_TOL,
};
