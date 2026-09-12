//! Error type for the VCS crate.

/// Failure modes of git queries. `NotARepo` and `GitMissing` are expected
/// outcomes on machines without a checkout or without git; the UI maps them to
/// an empty state rather than an error surface.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VcsError {
    /// `fatal: not a git repository` — cwd is not inside a work tree.
    #[error("not a git repository")]
    NotARepo,
    /// The `git` binary is not installed or not on `PATH`.
    #[error("git binary not found")]
    GitMissing,
    /// The repo exists but `HEAD` resolves to nothing (no commits yet).
    #[error("repository has no commits")]
    EmptyRepo,
    /// The process ran past its timeout and was killed.
    #[error("git timed out after {0}s")]
    Timeout(u64),
    /// git exited non-zero with a message we do not classify further.
    #[error("git failed: {0}")]
    Failed(String),
    /// The hash the client sent is not a commit hash (refused before spawn).
    #[error("invalid commit hash: {0}")]
    InvalidHash(String),
    /// Process spawn / IO failure other than a missing binary.
    #[error("io: {0}")]
    Io(String),
}
