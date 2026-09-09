//! Error types for pacode-import.

/// Error returned when parsing an [`ImportSource`](crate::ImportSource) fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown import source: {0}")]
pub struct ParseImportSourceError(pub String);
