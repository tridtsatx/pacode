//! Error types for pacode-acp.

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("client error: {0}")]
    Client(#[from] pacode_client::ClientError),

    #[error("protocol error: {0}")]
    Protocol(#[from] agent_client_protocol::Error),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("internal error: {0}")]
    Internal(String),
}
