//! Error types for plugin execution and host management.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin '{name}' not found")]
    NotFound { name: String },

    #[error("tool '{name}' not found")]
    ToolNotFound { name: String },

    #[error("command '{name}' not found")]
    CommandNotFound { name: String },

    #[error("plugin manifest error: {0}")]
    Manifest(String),

    #[error("plugin execution timed out")]
    Timeout,

    #[error("memory limit exceeded: {0}")]
    MemoryLimit(String),

    #[error("lua error: {0}")]
    Lua(String),

    #[error("wasm error: {0}")]
    Wasm(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
