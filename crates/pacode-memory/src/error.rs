//! Error type of the memory crate.

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Bad caller input (empty text, unknown scope or kind, ...).
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("no memory with id '{0}'")]
    NotFound(String),
    /// Failure reported by an [`crate::Embedder`].
    #[error("embedder: {0}")]
    Embedder(String),
}

impl MemoryError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}
