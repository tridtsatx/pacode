//! Protocol mapping between pacode and ACP (Agent Client Protocol) v1.

use std::collections::HashSet;

use agent_client_protocol::schema::v1::{
    Content, ContentBlock, ContentChunk, EmbeddedResourceResource, PermissionOption,
    PermissionOptionKind, Plan as AcpPlan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId as AcpSessionId, SessionMode, SessionModeId,
    SessionModeState, SessionUpdate, StopReason as AcpStopReason, TextContent,
    ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallStatus,
    ToolCallUpdate as AcpToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use pacode_types::ids::SessionId as PacodeSessionId;
use pacode_types::protocol::{Event, TurnStop};
use pacode_types::state::{Mode, PermissionDecision, PermissionRequest, Plan, PlanStatus};
use pacode_types::transcript::{ToolStatus, TranscriptItem, TranscriptKind};

/// Maps a pacode `SessionId` to an ACP `SessionId`.
pub fn session_id_to_acp(id: &PacodeSessionId) -> AcpSessionId {
    AcpSessionId::new(id.as_str())
}

/// Maps an ACP `SessionId` to a pacode `SessionId`.
pub fn session_id_from_acp(id: &AcpSessionId) -> PacodeSessionId {
    PacodeSessionId::new(&*id.0)
}

/// Infers the ACP `ToolKind` from a tool name.
pub fn tool_kind_from_name(name: &str) -> ToolKind {
    match name {
        "read" | "read_file" | "cat" => ToolKind::Read,
        "edit" | "edit_file" | "write" | "write_file" | "patch" => ToolKind::Edit,
        "delete" | "delete_file" | "rm" => ToolKind::Delete,
        "move" | "move_file" | "mv" => ToolKind::Move,
        "search" | "grep" | "glob" | "find" => ToolKind::Search,
        "execute" | "bash" | "run_bash" | "command" | "exec" => ToolKind::Execute,
        "think" | "reasoning" => ToolKind::Think,
        "fetch" | "web" | "http" => ToolKind::Fetch,
        "switch_mode" => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

/// Maps pacode's `ToolStatus` to ACP's `ToolCallStatus`.
///
/// Exhaustive match over pacode's `ToolStatus`.
pub fn tool_status_to_acp(status: ToolStatus) -> ToolCallStatus {
    match status {
        ToolStatus::Running => ToolCallStatus::InProgress,
        ToolStatus::Ok => ToolCallStatus::Completed,
        ToolStatus::Error => ToolCallStatus::Failed,
        ToolStatus::Backgrounded => ToolCallStatus::InProgress,
        ToolStatus::Denied => ToolCallStatus::Failed,
    }
}

/// Maps pacode's `PlanStatus` to ACP's `PlanEntryStatus`.
///
/// Returns `None` for cancelled items, matching pacode's internal `Plan::counted()` behavior.
pub fn plan_status_to_acp(status: PlanStatus) -> Option<PlanEntryStatus> {
    match status {
        PlanStatus::Pending => Some(PlanEntryStatus::Pending),
        PlanStatus::Active => Some(PlanEntryStatus::InProgress),
        PlanStatus::Done => Some(PlanEntryStatus::Completed),
        PlanStatus::Cancelled => None,
    }
}

/// Converts a pacode `Plan` to an ACP `Plan`.
pub fn plan_to_acp(plan: &Plan) -> AcpPlan {
    let entries = plan
        .items
        .iter()
        .filter_map(|item| {
            let status = plan_status_to_acp(item.status)?;
            Some(PlanEntry::new(
                item.content.clone(),
                PlanEntryPriority::Medium,
                status,
            ))
        })
        .collect();
    AcpPlan::new(entries)
}

/// Converts a pacode `Mode` to an ACP `SessionMode`.
pub fn mode_to_session_mode(mode: Mode) -> SessionMode {
    match mode {
        Mode::Build => SessionMode::new("build", "Build")
            .description("ask before edits and non-trivial commands"),
        Mode::Auto => {
            SessionMode::new("auto", "Auto").description("auto-accept edits and low-risk commands")
        }
        Mode::Plan => SessionMode::new("plan", "Plan").description("read-only tools and planning"),
        Mode::Bypass => SessionMode::new("bypass", "Bypass")
            .description("bypass permissions except catastrophic deny"),
    }
}

/// Builds the full `SessionModeState` for an ACP session given the current pacode mode.
pub fn modes_state(current_mode: Mode) -> SessionModeState {
    let available_modes = vec![
        mode_to_session_mode(Mode::Build),
        mode_to_session_mode(Mode::Auto),
        mode_to_session_mode(Mode::Plan),
        mode_to_session_mode(Mode::Bypass),
    ];
    SessionModeState::new(SessionModeId::new(current_mode.as_str()), available_modes)
}

/// Maps pacode `TurnStop` to ACP `StopReason`.
pub fn turn_stop_to_stop_reason(stop: &TurnStop) -> AcpStopReason {
    match stop {
        TurnStop::Completed => AcpStopReason::EndTurn,
        TurnStop::Interrupted => AcpStopReason::Cancelled,
        TurnStop::Failed { .. } => AcpStopReason::EndTurn,
    }
}

/// Maps a pacode `PermissionRequest` to an ACP `RequestPermissionRequest`.
pub fn permission_request_to_acp(
    session_id: &AcpSessionId,
    perm: &PermissionRequest,
) -> RequestPermissionRequest {
    let tool_call_id = ToolCallId::new(perm.call_id.as_str());
    let tool_kind = tool_kind_from_name(&perm.tool);
    let mut fields = ToolCallUpdateFields::new()
        .title(perm.title.clone())
        .kind(tool_kind)
        .status(ToolCallStatus::Pending);

    if !perm.detail.is_empty() {
        fields = fields.content(vec![ToolCallContent::Content(Content::new(
            ContentBlock::Text(TextContent::new(perm.detail.clone())),
        ))]);
    }

    let tool_call = AcpToolCallUpdate::new(tool_call_id, fields);

    let options = vec![
        PermissionOption::new("allow_once", "Allow Once", PermissionOptionKind::AllowOnce),
        PermissionOption::new(
            "allow_session",
            "Allow for Session",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new("deny", "Deny", PermissionOptionKind::RejectOnce),
    ];

    RequestPermissionRequest::new(session_id.clone(), tool_call, options)
}

/// Maps an ACP `RequestPermissionResponse` back to a pacode `PermissionDecision`.
pub fn permission_response_to_decision(response: &RequestPermissionResponse) -> PermissionDecision {
    match &response.outcome {
        RequestPermissionOutcome::Cancelled => PermissionDecision::Deny,
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome { option_id, .. }) => {
            match option_id.0.as_ref() {
                "allow_once" => PermissionDecision::AllowOnce,
                "allow_session" => PermissionDecision::AllowSession,
                "deny" => PermissionDecision::Deny,
                _ => PermissionDecision::Deny,
            }
        }
        _ => PermissionDecision::Deny,
    }
}

/// Tracks streaming state for a prompt turn to avoid duplicate chunks on item updates.
#[derive(Debug, Default)]
pub struct MappingState {
    /// Item sequence numbers that have been streamed via deltas.
    pub streamed_items: HashSet<u64>,
}

impl MappingState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.streamed_items.clear();
    }
}

/// Maps an incoming pacode `Event` into zero or more ACP `SessionUpdate`s.
///
/// Unmapped events (toasts, plugins, agent metadata, etc.) return an empty `Vec` cleanly.
pub fn event_to_session_updates(event: &Event, state: &mut MappingState) -> Vec<SessionUpdate> {
    match event {
        Event::TextDelta {
            item_seq,
            text,
            agent: _,
        } => {
            state.streamed_items.insert(*item_seq);
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text.clone())),
            ))]
        }
        Event::ReasoningDelta {
            item_seq,
            text,
            agent: _,
        } => {
            state.streamed_items.insert(*item_seq);
            vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text.clone())),
            ))]
        }
        Event::ItemAdded(item) => match &item.kind {
            TranscriptKind::ToolCall {
                call_id,
                name,
                title,
                status,
                preview,
                ..
            } => {
                let kind = tool_kind_from_name(name);
                let tool_status = tool_status_to_acp(*status);
                let mut content = Vec::new();
                if !preview.is_empty() {
                    content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
                        TextContent::new(preview.clone()),
                    ))));
                }
                vec![SessionUpdate::ToolCall(
                    AcpToolCall::new(ToolCallId::new(call_id.as_str()), title.clone())
                        .kind(kind)
                        .status(tool_status)
                        .content(content),
                )]
            }
            TranscriptKind::Assistant { text, complete } => {
                if *complete && !text.is_empty() && !state.streamed_items.contains(&item.seq) {
                    vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(text.clone())),
                    ))]
                } else {
                    vec![]
                }
            }
            TranscriptKind::Reasoning { text, complete } => {
                if *complete && !text.is_empty() && !state.streamed_items.contains(&item.seq) {
                    vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(text.clone())),
                    ))]
                } else {
                    vec![]
                }
            }
            TranscriptKind::BashCommand {
                command,
                output,
                exit_code: _,
                truncated: _,
            } => {
                let seq = item.seq;
                let tool_call_id = ToolCallId::new(format!("cmd_{seq}"));
                let mut content = Vec::new();
                if !output.is_empty() {
                    content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
                        TextContent::new(output.clone()),
                    ))));
                }
                vec![SessionUpdate::ToolCall(
                    AcpToolCall::new(tool_call_id, format!("Bash: {command}"))
                        .kind(ToolKind::Execute)
                        .status(ToolCallStatus::Completed)
                        .content(content),
                )]
            }
            TranscriptKind::User { .. }
            | TranscriptKind::Notice { .. }
            | TranscriptKind::Permission(_)
            | TranscriptKind::Question { .. } => vec![],
        },
        Event::ItemUpdated(item) => match &item.kind {
            TranscriptKind::ToolCall {
                call_id,
                status,
                preview,
                name,
                title,
                ..
            } => {
                let tool_status = tool_status_to_acp(*status);
                let mut content = Vec::new();
                if !preview.is_empty() {
                    content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
                        TextContent::new(preview.clone()),
                    ))));
                }
                let kind = tool_kind_from_name(name);
                let fields = ToolCallUpdateFields::new()
                    .kind(kind)
                    .title(title.clone())
                    .status(tool_status)
                    .content(content);
                vec![SessionUpdate::ToolCallUpdate(AcpToolCallUpdate::new(
                    ToolCallId::new(call_id.as_str()),
                    fields,
                ))]
            }
            TranscriptKind::Assistant { text, complete } => {
                if *complete && !text.is_empty() && !state.streamed_items.contains(&item.seq) {
                    vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(text.clone())),
                    ))]
                } else {
                    vec![]
                }
            }
            TranscriptKind::Reasoning { text, complete } => {
                if *complete && !text.is_empty() && !state.streamed_items.contains(&item.seq) {
                    vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(text.clone())),
                    ))]
                } else {
                    vec![]
                }
            }
            TranscriptKind::BashCommand { .. }
            | TranscriptKind::User { .. }
            | TranscriptKind::Notice { .. }
            | TranscriptKind::Permission(_)
            | TranscriptKind::Question { .. } => vec![],
        },
        Event::PlanUpdated(plan) => {
            vec![SessionUpdate::Plan(plan_to_acp(plan))]
        }
        Event::SessionUpdated(meta) => {
            vec![SessionUpdate::CurrentModeUpdate(
                agent_client_protocol::schema::v1::CurrentModeUpdate::new(SessionModeId::new(
                    meta.mode.as_str(),
                )),
            )]
        }
        Event::UsageUpdated(usage) => {
            if let Some(window) = usage.context_window {
                if window > 0 {
                    vec![SessionUpdate::UsageUpdate(
                        agent_client_protocol::schema::v1::UsageUpdate::new(
                            usage.context_tokens as u64,
                            window as u64,
                        ),
                    )]
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        }
        Event::TurnEnded { .. } => {
            state.clear();
            vec![]
        }
        Event::TurnStarted { .. }
        | Event::PermissionRequested(_)
        | Event::PermissionResolved { .. }
        | Event::AgentAdded(_)
        | Event::AgentUpdated(_)
        | Event::TaskAdded(_)
        | Event::TaskUpdated(_)
        | Event::Toast { .. }
        | Event::DaemonShuttingDown
        | Event::PluginToast { .. }
        | Event::PluginStatus { .. }
        // Scheduling has no ACP counterpart: a cron job or a monitor is a pacode
        // session concern, and the prompt a fired job sends arrives as a normal turn.
        // A question is answered in pacode's own picker; ACP has no equivalent.
        | Event::QuestionAsked(_)
        | Event::QuestionResolved { .. }
        | Event::CronUpdated(_)
        | Event::CronRemoved(_)
        | Event::MonitorUpdated(_)
        // Logging in is a pacode-side concern: an ACP client authenticates the agent
        // through its own `authenticate` call, not through session updates.
        | Event::LoginProgress { .. }
        | Event::AuthUpdated(_) => vec![],
    }
}

/// Converts a static `TranscriptItem` into ACP session updates (used during history replay).
pub fn transcript_item_to_session_updates(item: &TranscriptItem) -> Vec<SessionUpdate> {
    match &item.kind {
        TranscriptKind::User { text } => {
            vec![SessionUpdate::UserMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text.clone())),
            ))]
        }
        TranscriptKind::Assistant { text, .. } => {
            if text.is_empty() {
                vec![]
            } else {
                vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.clone())),
                ))]
            }
        }
        TranscriptKind::Reasoning { text, .. } => {
            if text.is_empty() {
                vec![]
            } else {
                vec![SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.clone())),
                ))]
            }
        }
        TranscriptKind::ToolCall {
            call_id,
            name,
            title,
            status,
            preview,
            ..
        } => {
            let kind = tool_kind_from_name(name);
            let tool_status = tool_status_to_acp(*status);
            let mut content = Vec::new();
            if !preview.is_empty() {
                content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
                    TextContent::new(preview.clone()),
                ))));
            }
            vec![SessionUpdate::ToolCall(
                AcpToolCall::new(ToolCallId::new(call_id.as_str()), title.clone())
                    .kind(kind)
                    .status(tool_status)
                    .content(content),
            )]
        }
        TranscriptKind::BashCommand {
            command, output, ..
        } => {
            let seq = item.seq;
            let tool_call_id = ToolCallId::new(format!("cmd_{seq}"));
            let mut content = Vec::new();
            if !output.is_empty() {
                content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
                    TextContent::new(output.clone()),
                ))));
            }
            vec![SessionUpdate::ToolCall(
                AcpToolCall::new(tool_call_id, format!("Bash: {command}"))
                    .kind(ToolKind::Execute)
                    .status(ToolCallStatus::Completed)
                    .content(content),
            )]
        }
        TranscriptKind::Notice { level, text } => {
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(format!("[{level:?}] {text}\n"))),
            ))]
        }
        TranscriptKind::Permission(_) | TranscriptKind::Question { .. } => vec![],
    }
}

/// Extracts plain prompt text from ACP prompt content blocks.
pub fn prompt_content_to_text(prompt: &[ContentBlock]) -> String {
    let mut text_parts = Vec::new();
    for block in prompt {
        match block {
            ContentBlock::Text(t) => {
                text_parts.push(t.text.clone());
            }
            ContentBlock::Image(_) => {}
            ContentBlock::Audio(_) => {}
            ContentBlock::Resource(r) => match &r.resource {
                EmbeddedResourceResource::TextResourceContents(t) => {
                    let uri = &t.uri;
                    let text = &t.text;
                    text_parts.push(format!("\n--- Resource: {uri} ---\n{text}\n"));
                }
                EmbeddedResourceResource::BlobResourceContents(b) => {
                    let uri = &b.uri;
                    text_parts.push(format!("\n--- Binary Resource: {uri} ---\n"));
                }
                _ => {}
            },
            ContentBlock::ResourceLink(l) => {
                let uri = &l.uri;
                text_parts.push(format!(" {uri}"));
            }
            _ => {}
        }
    }
    text_parts.join("")
}

#[cfg(test)]
#[path = "mapping_tests.rs"]
mod mapping_tests;
