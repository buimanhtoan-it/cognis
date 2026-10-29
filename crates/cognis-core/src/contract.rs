//! Extension ↔ backend JSON contract.
//!
//! Keep these constants and payload types in lockstep with the TypeScript
//! consumer, committed fixtures, and parity test (Requirement 22.7).

use serde::Serialize;

/// Bump on any breaking change to the extension ↔ backend JSON contract.
pub const CONTRACT_VERSION: u32 = 1;

/// CLI commands the extension relies on (same order as `contract.ts`).
pub const CLI_COMMANDS: [&str; 9] = [
    "init",
    "bootstrap",
    "health",
    "paths",
    "doctor",
    "mcp-config",
    "index",
    "handshake",
    "digest",
];

/// MCP tools the server exposes (same order as `contract.ts`).
/// Includes the 8 retrieval tools plus the 4 flagship real-time complexity,
/// optimization, and change-impact guardrail tools.
pub const MCP_TOOLS: [&str; 12] = [
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
    "analyze_patch_impact",
    "inspect_symbol_complexity",
    "trace_blast_radius",
    "suggest_optimal_refactor",
];

/// Build/runtime capabilities reported by the additive contract surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub semantic_backend: bool,
    pub accelerated_vector_search: bool,
}

impl Capabilities {
    /// Conservative defaults until startup resolves compiled/runtime support.
    pub const fn unavailable() -> Self {
        Self {
            semantic_backend: false,
            accelerated_vector_search: false,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// The `cognis-cli handshake` payload the extension reads at startup.
#[derive(Debug, Clone, Serialize)]
pub struct HandshakePayload {
    pub contract_version: u32,
    pub engine_version: String,
    pub cli_commands: Vec<String>,
    pub mcp_tools: Vec<String>,
    pub capabilities: Capabilities,
}

/// Build a handshake payload with conservative capabilities.
///
/// Binary entry points should call [`handshake_payload_with_capabilities`] after
/// resolving their compiled and runtime capability set.
pub fn handshake_payload() -> HandshakePayload {
    handshake_payload_with_capabilities(Capabilities::default())
}

/// Build the handshake payload for a resolved running-binary capability set.
pub fn handshake_payload_with_capabilities(capabilities: Capabilities) -> HandshakePayload {
    HandshakePayload {
        contract_version: CONTRACT_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        cli_commands: CLI_COMMANDS.iter().map(|s| s.to_string()).collect(),
        mcp_tools: MCP_TOOLS.iter().map(|s| s.to_string()).collect(),
        capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_set_and_order_match_contract() {
        assert_eq!(MCP_TOOLS.len(), 12);
        assert_eq!(MCP_TOOLS[0], "diffuse_context");
        assert_eq!(MCP_TOOLS[1], "symbol_lookup");
        assert_eq!(MCP_TOOLS[7], "retrieve_context_capsule");
        assert_eq!(MCP_TOOLS[8], "analyze_patch_impact");
        assert_eq!(MCP_TOOLS[9], "inspect_symbol_complexity");
        assert_eq!(MCP_TOOLS[10], "trace_blast_radius");
        assert_eq!(MCP_TOOLS[11], "suggest_optimal_refactor");
        assert_eq!(CLI_COMMANDS.len(), 9);
        assert_eq!(CLI_COMMANDS[0], "init");
        assert_eq!(CLI_COMMANDS[7], "handshake");
        assert_eq!(CLI_COMMANDS[8], "digest");
    }

    #[test]
    fn handshake_payload_serializes() {
        let p = handshake_payload();
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["contract_version"], 1);
        assert_eq!(v["mcp_tools"].as_array().unwrap().len(), 12);
        assert_eq!(v["cli_commands"].as_array().unwrap().len(), 9);
        assert_eq!(v["cli_commands"][0], "init");
        assert_eq!(v["cli_commands"][8], "digest");
        assert_eq!(v["capabilities"]["semantic_backend"], false);
        assert_eq!(v["capabilities"]["accelerated_vector_search"], false);
    }
}
