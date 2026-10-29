//! Contract-version lockstep + handshake (Property 4 — P-CON-VER).
//!
//! Port of the Python `test_contract_version_is_in_lockstep_across_languages`
//! and `test_handshake_command_emits_the_contract`. These assert the
//! cross-language version invariant *without* a live server:
//!
//! * **Lockstep** — the backend `CONTRACT_VERSION` equals the extension's
//!   `EXPECTED_CONTRACT_VERSION` (parsed from `apps/cognis-mcp/src/
//!   contract.ts`). Bumping one without the other — a breaking shape change —
//!   fails the build the moment they drift (Requirement 3.3).
//! * **Handshake** — `cognis_core::handshake_payload()` (the payload
//!   `cognis-cli handshake` emits, Task 7.3) carries the keys the extension
//!   reads and advertises exactly the 12 `MCP_TOOLS` (Requirement 3.1).

use std::path::PathBuf;

use cognis_mcp::{handshake_payload, CLI_COMMANDS, CONTRACT_VERSION, MCP_TOOLS};

/// Locate `apps/cognis-mcp/src/contract.ts` from the crate manifest dir.
fn contract_ts_path() -> PathBuf {
    // crates/cognis-mcp/ -> repo root is two levels up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root");
    repo_root
        .join("apps")
        .join("cognis-mcp")
        .join("src")
        .join("contract.ts")
}

/// Parse the integer literal from `export const CONTRACT_VERSION = N;`.
fn parse_expected_contract_version(text: &str) -> Option<u32> {
    let anchor = "export const CONTRACT_VERSION";
    let idx = text.find(anchor)?;
    let after = &text[idx + anchor.len()..];
    let eq = after.find('=')?;
    let digits: String = after[eq + 1..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[test]
fn contract_version_is_in_lockstep_across_languages() {
    let path = contract_ts_path();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let ts_version = parse_expected_contract_version(&text)
        .expect("could not find EXPECTED_CONTRACT_VERSION in contract.ts");
    assert_eq!(
        ts_version, CONTRACT_VERSION,
        "contract version skew: backend CONTRACT_VERSION={CONTRACT_VERSION} but the \
         extension's EXPECTED_CONTRACT_VERSION={ts_version}. Bump both together when the \
         cross-process JSON contract changes."
    );
}

#[test]
fn handshake_payload_emits_the_contract() {
    let payload = handshake_payload();
    let value = serde_json::to_value(&payload).expect("serialize handshake");

    for key in [
        "contract_version",
        "engine_version",
        "cli_commands",
        "mcp_tools",
        "capabilities",
    ] {
        assert!(
            value.get(key).is_some(),
            "handshake payload missing key '{key}'"
        );
    }
    assert_eq!(value["contract_version"], CONTRACT_VERSION);
    assert_eq!(value["cli_commands"], serde_json::json!(CLI_COMMANDS));
    assert_eq!(value["mcp_tools"], serde_json::json!(MCP_TOOLS));
    assert_eq!(value["capabilities"]["semantic_backend"], false);
    assert_eq!(value["capabilities"]["accelerated_vector_search"], false);

    let advertised: Vec<&str> = value["mcp_tools"]
        .as_array()
        .expect("mcp_tools array")
        .iter()
        .map(|v| v.as_str().expect("tool name"))
        .collect();
    for tool in MCP_TOOLS {
        assert!(
            advertised.contains(&tool),
            "handshake mcp_tools missing {tool}"
        );
    }
}
