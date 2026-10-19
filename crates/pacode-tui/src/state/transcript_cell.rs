//! What one transcript cell is, and what it weighs: the resident-bytes
//! estimate the transcript's byte cap counts.

use std::mem::size_of;

use pacode_types::{ExplainLevel, Question, TranscriptKind};

/// One transcript row group. `id` is the item seq from the daemon for daemon
/// rows, and a client-allocated id above `CLIENT_ID_TAG` for rows the client
/// invents itself (notices, gaps, finished background jobs, a permission row
/// that arrived as an event rather than as an item).
#[derive(Clone, Debug)]
pub struct Cell {
    pub id: u64,
    pub kind: CellKind,
    /// Bumped whenever the content changes (invalidates the cache).
    pub version: u32,
    pub ts_ms: u64,
    pub stats: Option<String>,
    /// `e`-key explanation state — `Some` only while the cell still carries a
    /// pending `TranscriptKind::Permission` request; a verdict's swap to a
    /// notice drops it with the prompt.
    pub perm_explain: Option<PermExplain>,
}

/// Display state of the detached permission explanation. `e` on a pending
/// permission sends `Request::ExplainPermission`; the aux model's answer
/// arrives later as `Event::PermissionExplained`. It lives on the cell rather
/// than inside `TranscriptKind::Permission` — that is the wire item and has
/// no room for client-side state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermExplain {
    /// `e` was pressed; the daemon has not answered yet.
    Requested,
    /// The explanation landed. `level` is the model's danger rating —
    /// `None` when the aux call failed and `text` holds the honest reason.
    Done {
        text: String,
        level: Option<ExplainLevel>,
    },
}

/// The word shown for an explanation's danger rating — the same spellings
/// the aux model's `LEVEL:` line uses (`ExplainLevel::parse` accepts them).
pub fn explain_level_name(level: ExplainLevel) -> &'static str {
    match level {
        ExplainLevel::None => "none",
        ExplainLevel::Low => "low",
        ExplainLevel::Medium => "medium",
        ExplainLevel::High => "high",
        ExplainLevel::Catastrophic => "catastrophic",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderInfo {
    pub version: String,
    /// Days since the Unix epoch; picks the phrase of the day (stable for the whole day).
    pub day: u64,
    pub mascot: crate::ui::mascot::MascotKind,
    /// Whether the terminal takes RGB, so the mascot can use the arcade palette
    /// instead of folding it down to the nearest ANSI colour.
    pub truecolor: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CellKind {
    /// Wraps the daemon item; `Assistant`/`Reasoning` text is the *revealed* text.
    Item(TranscriptKind),
    /// Divider between turns (drawn as a blank line).
    Gap,
    /// A background task or subagent that ended. Client-side only: the daemon
    /// reports the task, the client decides whether it was long enough to be
    /// worth a line and renders it like a tool call rather than as prose.
    BackgroundResult(BackgroundResult),
}

/// What a finished background job is shown as.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundResult {
    /// `task` for a command, `agent` for a subagent.
    pub kind: BackgroundKind,
    /// Command line or agent name.
    pub label: String,
    pub outcome: BackgroundOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    Task,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundOutcome {
    Completed,
    Failed,
    Killed,
}

fn sum_bytes(values: impl IntoIterator<Item = usize>) -> usize {
    values.into_iter().fold(0usize, usize::saturating_add)
}

fn string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn question_bytes(question: &Question) -> usize {
    let options = question.options.iter().map(|option| {
        sum_bytes([
            size_of::<pacode_types::QuestionOption>(),
            option.label.capacity(),
            option.description.capacity(),
        ])
    });
    sum_bytes(
        [
            size_of::<Question>(),
            question.header.capacity(),
            question.question.capacity(),
        ]
        .into_iter()
        .chain(options),
    )
}

fn item_bytes(kind: &TranscriptKind) -> usize {
    match kind {
        TranscriptKind::Images { images } => std::mem::size_of_val(images.as_slice()),
        TranscriptKind::User { text }
        | TranscriptKind::Assistant { text, .. }
        | TranscriptKind::Reasoning { text, .. } => text.capacity(),
        TranscriptKind::ToolCall {
            call_id,
            name,
            title,
            intent,
            preview,
            task,
            ..
        } => sum_bytes([
            call_id.as_str().len(),
            name.capacity(),
            title.capacity(),
            string_bytes(intent),
            preview.capacity(),
            task.as_ref().map_or(0, |id| id.as_str().len()),
        ]),
        TranscriptKind::Notice { text, .. } => text.capacity(),
        TranscriptKind::Permission(request) => sum_bytes([
            request.id.as_str().len(),
            request.agent.as_str().len(),
            request.agent_name.capacity(),
            request.call_id.as_str().len(),
            request.tool.capacity(),
            request.title.capacity(),
            request.detail.capacity(),
        ]),
        TranscriptKind::Question { question, .. } => question_bytes(question),
        TranscriptKind::BashCommand {
            command, output, ..
        } => sum_bytes([command.capacity(), output.capacity()]),
        TranscriptKind::AgentMessage {
            subagent,
            subagent_name,
            text,
            reply,
            ..
        } => sum_bytes([
            subagent.as_str().len(),
            subagent_name.capacity(),
            text.capacity(),
            match reply {
                Some(pacode_types::AgentReply::Answered { text }) => text.capacity(),
                Some(
                    pacode_types::AgentReply::Unanswered
                    | pacode_types::AgentReply::Failed
                    | pacode_types::AgentReply::Cancelled,
                )
                | None => 0,
            },
        ]),
    }
}

/// Resident cost of one cell, estimated the way the daemon estimates its tail:
/// string capacities, not lengths, because capacity is what the process holds.
pub(super) fn cell_bytes(cell: &Cell) -> usize {
    let kind = match &cell.kind {
        CellKind::Gap => 0,
        CellKind::Item(kind) => item_bytes(kind),
        CellKind::BackgroundResult(result) => {
            sum_bytes([size_of::<BackgroundResult>(), result.label.capacity()])
        }
    };
    sum_bytes([
        size_of::<Cell>(),
        kind,
        string_bytes(&cell.stats),
        match cell.perm_explain.as_ref() {
            Some(PermExplain::Requested) => 0,
            Some(PermExplain::Done { text, .. }) => text.capacity(),
            None => 0,
        },
    ])
}
