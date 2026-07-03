//! Engine error type shared across crates.

/// Top-level engine error. Library crates return `Result<_, CognisError>`;
/// binaries may wrap with `anyhow` at the edge.
#[derive(Debug, thiserror::Error)]
pub enum CognisError {
    #[error("store error: {0}")]
    Store(String),
    #[error("retrieval error: {0}")]
    Retrieval(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("model error: {0}")]
    Model(String),
    #[error("eval error: {0}")]
    Eval(String),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, CognisError>;
