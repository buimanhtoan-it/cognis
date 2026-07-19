//! cognis-mcp — MCP tool surface (Task 7).
//!
//! This crate lands the read-only MCP server: JSON-RPC 2.0 stdio framing
//! ([`jsonrpc`]), hard caps applied before work ([`caps`]), the hashed-argument
//! audit log ([`audit`]), the stable error envelope ([`errors`]), the read-only
//! [`engine::RetrievalEngine`] seam the 8 tools are composed from, the
//! contract-shaped tool handlers ([`tools`]), and the [`server::McpServer`]
//! dispatch loop that ties them together.
//!
//! The server keeps the extension ↔ backend JSON contract invariant
//! (Requirement 3, Property 4): it advertises exactly the 8 tools in
//! [`MCP_TOOLS`], returns each tool's pinned output shape (`on_path`/`ppr_score`
//! on `diffuse_context`, the capsule schema, the `{error:{code,message,
//! retryable}}` envelope), and reports a [`CONTRACT_VERSION`] kept in lockstep
//! with the extension's `EXPECTED_CONTRACT_VERSION`.

pub mod audit;
pub mod caps;
pub mod engine;
pub mod errors;
pub mod http;
pub mod jsonrpc;
pub mod server;
pub mod store_engine;
pub mod tools;

pub use cognis_core::contract::HandshakePayload;
pub use cognis_core::{handshake_payload, CONTRACT_VERSION, MCP_TOOLS};

pub use audit::AuditLog;
pub use caps::Caps;
pub use engine::RetrievalEngine;
pub use errors::{error_envelope, is_error_envelope, McpError};
pub use http::{
    is_loopback_host, BindOptions, HttpServeConfig, RouteCredential, ALLOW_REMOTE_ENV,
    ROUTE_CREDENTIAL_ENV, ROUTE_CREDENTIAL_HEADER,
};
pub use server::McpServer;
pub use store_engine::StoreEngine;

// Re-export isolation types so daemon entry points can configure attachment
// verification without reaching into cognis-core / cognis-embed directly.
pub use cognis_core::{
    verify_repo_attachment, verify_repo_wire_key, AttachmentDecision, RepoIdentity,
    REPO_IDENTITY_HEADER,
};
pub use cognis_embed::{session_reuse_allowed, ModelFingerprint, MODEL_FINGERPRINT_HEADER};
