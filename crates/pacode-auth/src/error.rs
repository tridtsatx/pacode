//! Error types for the `pacode-auth` crate.

use thiserror::Error;

/// Core error type representing any authentication, OAuth, or credential store failure.
#[derive(Debug, Error)]
pub enum AuthError {
    /// Filesystem or OS-level I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP request or transport failure.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Access was denied during authorization (e.g. user cancelled or server rejected).
    #[error("access denied: {0}")]
    Denied(String),

    /// Local callback server error (state mismatch, port bind failure, timeout).
    #[error("callback error: {0}")]
    Callback(String),

    /// Credential store failure (file corrupted, persistence failure).
    #[error("store error: {0}")]
    Store(String),

    /// Token refresh error (terminal failure, concurrent flight abort).
    #[error("refresh error: {0}")]
    Refresh(String),

    /// Provider requested was not recognized in the catalog or store.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    /// Configuration or proxy resolution error.
    #[error("config error: {0}")]
    Config(String),
}

/// A specialized [`Result`](std::result::Result) type for `pacode-auth` operations.
pub type Result<T> = std::result::Result<T, AuthError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
