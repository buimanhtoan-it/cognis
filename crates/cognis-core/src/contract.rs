//! Extension ↔ backend contract — Rust mirror of `packages/core/cognis/contract.py`.
//!
//! Single source of truth for the cross-process JSON contract. Order and values
//! are kept identical to the Python contract so the existing TypeScript
//! extension keeps working unchanged (Requirements 3.1, 3.3).

use serde::Serialize;

/// Bump on any breaking change to the extension ↔ backend JSON contract.
pub const CONTRACT_VERSION: u32 = 1;

/// CLI commands the extension relies on (same order as `contract.py`).
pub const CLI_COMMANDS: [&str; 8] = [
    "init",
    "bootstrap",
    "health",
    "paths",
    "doctor",
    "mcp-config",
    "index",
    "handshake",
];

/// MCP tools the server exposes (same order as `contract.py`).
pub const MCP_TOOLS: [&str; 8] = [
    "diffuse_context",
    "symbol_lookup",
    "symbol_search",
    "discover_symbols",
    "semantic_search",
    "resolve_symbols",
    "dependency_trace",
    "retrieve_context_capsule",
];

/// The `cognis-cli handshake` payload the extension reads at startup.
#[derive(Debug, Clone, Serialize)]
pub struct HandshakePayload {
    pub contract_version: u32,
    pub engine_version: String,
    pub cli_commands: Vec<String>,
    pub mcp_tools: Vec<String>,
}

/// Build the handshake payload (mirrors `handshake_payload()`).
pub fn handshake_payload() -> HandshakePayload {
    HandshakePayload {
        contract_version: CONTRACT_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        cli_commands: CLI_COMMANDS.iter().map(|s| s.to_string()).collect(),
        mcp_tools: MCP_TOOLS.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_set_and_order_match_contract() {
        assert_eq!(MCP_TOOLS.len(), 8);
        assert_eq!(MCP_TOOLS[0], "diffuse_context");
        assert_eq!(MCP_TOOLS[1], "symbol_lookup");
        assert_eq!(MCP_TOOLS[7], "retrieve_context_capsule");
        assert_eq!(CLI_COMMANDS[0], "init");
        assert_eq!(CLI_COMMANDS[7], "handshake");
    }

    #[test]
    fn handshake_payload_serializes() {
        let p = handshake_payload();
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["contract_version"], 1);
        assert_eq!(v["mcp_tools"].as_array().unwrap().len(), 8);
        assert_eq!(v["cli_commands"][0], "init");
    }
}
