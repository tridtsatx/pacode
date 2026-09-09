//! Tool results and errors.

use pacode_types::{DiffStat, TaskId};

/// What a tool returns. `content` goes to the model; `title`/`preview`/`diff` go to
/// the transcript row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolOutput {
    /// Model-visible result. Already capped by the tool (spec §7: head+tail 16K chars).
    pub content: String,
    /// Transcript row title, e.g. `Edit src/tui/sidebar.rs`. Empty = use the default
    /// `<Name> <first arg>` title built by the core.
    pub title: String,
    /// Short output tail for the transcript row (bounded, a few lines).
    pub preview: String,
    pub diff: Option<DiffStat>,
    /// Set when the call handed work to a background task.
    pub task: Option<TaskId>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = preview.into();
        self
    }

    pub fn with_diff(mut self, diff: DiffStat) -> Self {
        self.diff = Some(diff);
        self
    }

    pub fn with_task(mut self, task: TaskId) -> Self {
        self.task = Some(task);
        self
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Bad arguments; the message goes back to the model as an error result.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The user or the mode denied the call.
    #[error("denied: {0}")]
    Denied(String),
    /// Cancelled by interrupt or agent stop.
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Anything else; reported to the model as an error result.
    #[error("{0}")]
    Failed(String),
}

impl ToolError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self::Failed(msg.into())
    }
}
