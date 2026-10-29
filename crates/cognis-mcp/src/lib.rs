//! cognis-mcp — MCP guardrail & code intelligence server.
//!
//! Implements the read-only MCP server over JSON-RPC 2.0 stdio and HTTP:
//! framing ([`jsonrpc`]), hard caps applied before work ([`caps`]), hashed-argument
//! audit logging ([`audit`]), stable error envelopes ([`errors`]), the
//! [`engine::RetrievalEngine`] trait composing the 12 guardrail and retrieval
//! tools, contract-shaped tool handlers ([`tools`]), and [`server::McpServer`]
//! dispatch (including the `cognis://live/guardrails` resource).
//!
//! Advertises the 12 tools in [`MCP_TOOLS`], enforces pinned output schemas,
//! and reports [`CONTRACT_VERSION`] in lockstep with the IDE extension.

pub mod audit;
pub mod caps;
pub mod engine;
pub mod errors;
pub mod http;
pub mod jsonrpc;
#[cfg(feature = "reference-scan")]
pub mod reference_scan;
pub mod server;
pub mod store_engine;
pub mod tools;

pub use cognis_core::{
    handshake_payload, handshake_payload_with_capabilities, Capabilities, HandshakePayload,
    CLI_COMMANDS, CONTRACT_VERSION, MCP_TOOLS,
};

pub use audit::AuditLog;
pub use caps::Caps;
pub use engine::RetrievalEngine;
pub use errors::{error_envelope, is_error_envelope, McpError};
pub use http::{
    is_loopback_host, BindOptions, HttpServeConfig, RouteCredential, ALLOW_REMOTE_ENV,
    ROUTE_CREDENTIAL_ENV, ROUTE_CREDENTIAL_HEADER,
};
pub use server::McpServer;
pub use store_engine::{guard, GuardVerdict, StoreEngine};

// Re-export isolation types so daemon entry points can configure attachment
// verification without reaching into cognis-core / cognis-embed directly.
pub use cognis_core::{
    verify_repo_attachment, verify_repo_wire_key, AttachmentDecision, RepoIdentity,
    REPO_IDENTITY_HEADER,
};
pub use cognis_embed::{session_reuse_allowed, ModelFingerprint, MODEL_FINGERPRINT_HEADER};
