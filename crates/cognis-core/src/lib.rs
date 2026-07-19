//! cognis-core — models, config, contract constants, error types.
//!
//! The dependency-neutral foundation crate every other crate builds on. Models
//! and config mirror the Python pydantic schemas field-for-field so the Rust
//! engine round-trips the same UCKG rows and `.cognis/config.yaml`
//! (Requirement 2); contract constants keep the extension ↔ backend JSON
//! contract invariant (Requirement 3).

pub mod config;
pub mod contract;
pub mod error;
pub mod graph;
pub mod hit;
pub mod identity;
pub mod lease;
pub mod models;
pub mod warm_policy;

pub use config::Config;
pub use contract::{handshake_payload, CLI_COMMANDS, CONTRACT_VERSION, MCP_TOOLS};
pub use error::{CognisError, Result};
pub use graph::CodeGraph;
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
    fn exactly_eight_tools() {
        assert_eq!(MCP_TOOLS.len(), 8);
        assert!(MCP_TOOLS.contains(&"diffuse_context"));
    }
}
