//! cognis-core — models, config, contract constants, error types, complexity
//! algebra, and impact graph primitives.

pub mod complexity;
pub mod config;
pub mod contract;
pub mod error;
pub mod graph;
pub mod hit;
pub mod identity;
pub mod lease;
pub mod models;
pub mod warm_policy;

pub use complexity::{
    ArgReduction, AstCallSite, AstComplexitySummary, BottleneckStep, ChangeImpactReport,
    ComplexityAmplificationPath, ComplexityClass, ContractBreakageSite, GuardrailCapsule,
    ImpactDimensions, IntrinsicPattern, IoEffectKind, LoopBound, OptimizationAdvisory,
    SymbolComplexityReport,
};
pub use config::Config;
pub use contract::{
    handshake_payload, handshake_payload_with_capabilities, Capabilities, HandshakePayload,
    CLI_COMMANDS, CONTRACT_VERSION, MCP_TOOLS,
};
pub use error::{CognisError, Result};
pub use graph::{CodeGraph, ImpactGraph};
pub use hit::Hit;
pub use identity::{
    canonicalize_path, verify_repo_attachment, verify_repo_wire_key, AttachmentDecision,
    RepoIdentity, DB_PATH_ENV, REPO_IDENTITY_HEADER, REPO_ROOT_ENV,
};
pub use lease::{
    acquire_or_attach, lease_path, resolve_repo_root_from_env, AcquireOutcome, LeaseGuard,
    LeaseRecord, LeaseRole, DEFAULT_LEASE_TTL,
};
pub use models::{Edge, EdgeKind, FileRecord, ParseStatus, Symbol, SymbolAttribute, SymbolKind};
pub use warm_policy::{SemanticWarmPolicy, WARM_SEMANTIC_ENV};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_contract_tools() {
        assert_eq!(MCP_TOOLS.len(), 12);
        assert!(MCP_TOOLS.contains(&"diffuse_context"));
        assert!(MCP_TOOLS.contains(&"analyze_patch_impact"));
        assert!(MCP_TOOLS.contains(&"inspect_symbol_complexity"));
        assert!(MCP_TOOLS.contains(&"trace_blast_radius"));
        assert!(MCP_TOOLS.contains(&"suggest_optimal_refactor"));
    }
}
