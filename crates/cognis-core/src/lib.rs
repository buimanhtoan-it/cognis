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
pub mod models;

pub use config::Config;
pub use contract::{handshake_payload, CLI_COMMANDS, CONTRACT_VERSION, MCP_TOOLS};
pub use error::{CognisError, Result};
pub use graph::CodeGraph;
pub use hit::Hit;
pub use models::{Edge, EdgeKind, FileRecord, ParseStatus, Symbol, SymbolAttribute, SymbolKind};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_eight_tools() {
        assert_eq!(MCP_TOOLS.len(), 8);
        assert!(MCP_TOOLS.contains(&"diffuse_context"));
    }
}
