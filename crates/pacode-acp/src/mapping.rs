//! Protocol mapping between pacode and ACP (Agent Client Protocol) v1.

use std::collections::{HashMap, HashSet, VecDeque};

use agent_client_protocol::schema::v1::{
    Content, ContentBlock, ContentChunk, EmbeddedResourceResource, ImageContent, PermissionOption,
    PermissionOptionKind, Plan as AcpPlan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId as AcpSessionId, SessionMode, SessionModeId,
    SessionModeState, SessionUpdate, StopReason as AcpStopReason, TextContent,
    ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallStatus,
    ToolCallUpdate as AcpToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use base64::Engine;
use pacode_types::Attachment;
use pacode_types::ids::SessionId as PacodeSessionId;
use pacode_types::message::ImageSource;
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

/// One-line description of a pacode `Mode`, shared by the legacy mode picker
/// (`mode_to_session_mode`) and the config-option "mode" select so both
/// surfaces always show identical texts.
pub fn mode_description(mode: Mode) -> &'static str {
    match mode {
        Mode::Build => "ask before edits and non-trivial commands",
        Mode::Auto => "auto-accept edits and low-risk commands",
        Mode::Plan => "read-only tools and planning",
        Mode::Bypass => "bypass permissions except catastrophic deny",
    }
}

/// Converts a pacode `Mode` to an ACP `SessionMode`.
pub fn mode_to_session_mode(mode: Mode) -> SessionMode {
    SessionMode::new(mode.as_str(), mode.label()).description(mode_description(mode))
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
pub fn turn_stop_to_stop_reason(stop: &TurnStop) -> Result<AcpStopReason, String> {
    match stop {
        TurnStop::Completed => Ok(AcpStopReason::EndTurn),
        TurnStop::Interrupted => Ok(AcpStopReason::Cancelled),
        TurnStop::Failed { message } => Err(message.clone()),
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

const MAPPING_RETENTION_LIMIT: usize = 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ToolView {
    name: String,
    title: String,
    preview: String,
}

fn tool_view(kind: &TranscriptKind) -> ToolView {
    match kind {
        TranscriptKind::ToolCall {
            name,
            title,
            preview,
            ..
        } => ToolView {
            name: name.clone(),
            title: title.clone(),
            preview: preview.clone(),
        },
        TranscriptKind::User { .. }
        | TranscriptKind::Assistant { .. }
        | TranscriptKind::Reasoning { .. }
        | TranscriptKind::Notice { .. }
        | TranscriptKind::Permission(_)
        | TranscriptKind::Question { .. }
        | TranscriptKind::BashCommand { .. }
        | TranscriptKind::Images { .. }
        | TranscriptKind::AgentMessage { .. } => ToolView::default(),
    }
}

/// Tracks streaming state for a prompt turn to avoid duplicate chunks on item updates.
#[derive(Debug, Default)]
pub struct MappingState {
    pub streamed_items: HashMap<u64, String>,
    completed_items: HashSet<u64>,
    replayed_items: HashSet<u64>,
    tool_statuses: HashMap<u64, ToolStatus>,
    tool_views: HashMap<u64, ToolView>,
    retained_items: HashSet<u64>,
    retention_order: VecDeque<u64>,
    replayed_floor: u64,
}

impl MappingState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.streamed_items.clear();
        self.completed_items.clear();
        self.replayed_items.clear();
        self.tool_statuses.clear();
        self.tool_views.clear();
        self.retained_items.clear();
        self.retention_order.clear();
        self.replayed_floor = 0;
    }

    pub fn finish_turn(&mut self) {
        self.streamed_items
            .retain(|item_seq, _| !self.completed_items.contains(item_seq));
    }

    fn retired(&self, item_seq: u64) -> bool {
        item_seq <= self.replayed_floor
    }

    fn track_item(&mut self, item_seq: u64) {
        if self.retired(item_seq) {
            return;
        }
        if self.retained_items.insert(item_seq) {
            self.retention_order.push_back(item_seq);
        }
        while self.retention_order.len() > MAPPING_RETENTION_LIMIT {
            let Some(oldest) = self.retention_order.pop_front() else {
                break;
            };
            self.retained_items.remove(&oldest);
            self.completed_items.remove(&oldest);
            self.replayed_items.remove(&oldest);
            self.tool_statuses.remove(&oldest);
            self.tool_views.remove(&oldest);
            self.streamed_items.remove(&oldest);
            self.replayed_floor = self.replayed_floor.max(oldest);
        }
    }

    pub fn seed_transcript(&mut self, floor: u64, items: &[(u64, String, bool)]) {
        let seen = items.iter().map(|(seq, _, _)| *seq).collect::<Vec<_>>();
        self.seed_transcript_with_seen(floor, items, &seen);
    }

    pub fn seed_transcript_with_seen(
        &mut self,
        _floor: u64,
        items: &[(u64, String, bool)],
        seen: &[u64],
    ) {
        for (seq, text, complete) in items {
            if self.retired(*seq) {
                continue;
            }
            self.track_item(*seq);
            self.streamed_items.insert(*seq, text.clone());
            if *complete {
                self.completed_items.insert(*seq);
                self.streamed_items.remove(seq);
            } else {
                self.completed_items.remove(seq);
            }
        }
        for seq in seen {
            self.mark_item_replayed(*seq);
        }
    }

    pub fn seed_tool_statuses(&mut self, statuses: &[(u64, ToolStatus)]) {
        for (seq, status) in statuses {
            if self.retired(*seq) {
                continue;
            }
            self.track_item(*seq);
            self.tool_statuses.insert(*seq, *status);
        }
    }

    pub fn replay_transcript(
        &mut self,
        items: &[TranscriptItem],
    ) -> Result<Vec<SessionUpdate>, String> {
        let mut updates = Vec::new();
        for item in items {
            updates.extend(self.replay_transcript_item(item)?);
        }
        Ok(updates)
    }

    pub(crate) fn replay_transcript_item(
        &mut self,
        item: &TranscriptItem,
    ) -> Result<Vec<SessionUpdate>, String> {
        if !item.agent.is_main() || self.retired(item.seq) {
            return Ok(Vec::new());
        }
        let is_new = !self.replayed_items.contains(&item.seq);
        let updates = match &item.kind {
            TranscriptKind::Assistant { text, complete } if *complete => {
                self.validate_authoritative(item.seq, text)?;
                final_item_update(self, item.seq, text, true, false)
            }
            TranscriptKind::Assistant { text, .. }
                if is_new || !self.completed_items.contains(&item.seq) =>
            {
                self.replay_partial_text(item.seq, text, false)?
            }
            TranscriptKind::Reasoning { text, complete } if *complete => {
                self.validate_authoritative(item.seq, text)?;
                final_item_update(self, item.seq, text, true, true)
            }
            TranscriptKind::Reasoning { text, .. }
                if is_new || !self.completed_items.contains(&item.seq) =>
            {
                self.replay_partial_text(item.seq, text, true)?
            }
            TranscriptKind::ToolCall { status, .. } => {
                self.track_item(item.seq);
                let status_changed = self.tool_statuses.insert(item.seq, *status) != Some(*status);
                let view = tool_view(&item.kind);
                let view_changed = self.tool_views.get(&item.seq) != Some(&view);
                self.tool_views.insert(item.seq, view);
                if is_new {
                    transcript_item_to_session_updates(item)
                } else if status_changed || view_changed {
                    tool_call_update(item)
                } else {
                    Vec::new()
                }
            }
            _ if is_new => transcript_item_to_session_updates(item),
            _ => Vec::new(),
        };
        self.mark_item_replayed(item.seq);
        self.remember_item(item);
        Ok(updates)
    }

    fn validate_authoritative(&self, item_seq: u64, authoritative: &str) -> Result<(), String> {
        if self.completed_items.contains(&item_seq) {
            return Ok(());
        }
        if self
            .streamed_items
            .get(&item_seq)
            .is_some_and(|streamed| !authoritative.starts_with(streamed.as_str()))
        {
            return Err(format!(
                "authoritative transcript item {item_seq} diverges from the ACP v1 prefix"
            ));
        }
        Ok(())
    }

    fn replay_partial_text(
        &mut self,
        item_seq: u64,
        authoritative: &str,
        reasoning: bool,
    ) -> Result<Vec<SessionUpdate>, String> {
        if self.retired(item_seq) || self.completed_items.contains(&item_seq) {
            return Ok(Vec::new());
        }
        self.track_item(item_seq);
        self.validate_authoritative(item_seq, authoritative)?;
        let suffix = self
            .streamed_items
            .get(&item_seq)
            .map_or(authoritative, |streamed| &authoritative[streamed.len()..]);
        self.streamed_items
            .insert(item_seq, authoritative.to_owned());
        if suffix.is_empty() {
            return Ok(Vec::new());
        }
        let content = ContentChunk::new(ContentBlock::Text(TextContent::new(suffix.to_owned())));
        Ok(if reasoning {
            vec![SessionUpdate::AgentThoughtChunk(content)]
        } else {
            vec![SessionUpdate::AgentMessageChunk(content)]
        })
    }

    pub fn remember_item(&mut self, item: &TranscriptItem) {
        if !item.agent.is_main() || self.retired(item.seq) {
            return;
        }
        self.remember_tool_call(item);
        self.track_item(item.seq);
        let (text, complete) = match &item.kind {
            TranscriptKind::Assistant { text, complete }
            | TranscriptKind::Reasoning { text, complete } => (text, *complete),
            _ => return,
        };
        if complete {
            self.completed_items.insert(item.seq);
            return;
        }
        self.completed_items.remove(&item.seq);
        self.streamed_items.insert(item.seq, text.clone());
    }

    pub(crate) fn remember_tool_call(&mut self, item: &TranscriptItem) {
        if !item.agent.is_main() || self.retired(item.seq) {
            return;
        }
        let TranscriptKind::ToolCall { status, .. } = &item.kind else {
            return;
        };
        self.track_item(item.seq);
        self.tool_statuses.insert(item.seq, *status);
        self.tool_views.insert(item.seq, tool_view(&item.kind));
    }

    fn mark_item_replayed(&mut self, item_seq: u64) {
        if self.retired(item_seq) {
            return;
        }
        self.track_item(item_seq);
        self.replayed_items.insert(item_seq);
    }

    fn record_delta(&mut self, item_seq: u64, text: &str) -> bool {
        if self.retired(item_seq) || self.completed_items.contains(&item_seq) {
            return false;
        }
        self.mark_item_replayed(item_seq);
        self.streamed_items
            .entry(item_seq)
            .or_default()
            .push_str(text);
        true
    }

    fn take_final_text(&mut self, item_seq: u64, authoritative: &str) -> Option<String> {
        if self.retired(item_seq) || self.completed_items.contains(&item_seq) {
            return None;
        }
        self.track_item(item_seq);
        let (prefix_matches, suffix) = self.streamed_items.get(&item_seq).map_or(
            (
                true,
                (!authoritative.is_empty()).then(|| authoritative.to_owned()),
            ),
            |streamed| {
                let matches = authoritative.starts_with(streamed.as_str());
                let suffix = if matches && authoritative.len() > streamed.len() {
                    Some(authoritative[streamed.len()..].to_owned())
                } else {
                    None
                };
                (matches, suffix)
            },
        );
        self.completed_items.insert(item_seq);
        if !prefix_matches {
            self.streamed_items
                .insert(item_seq, authoritative.to_owned());
            return None;
        }
        self.streamed_items.remove(&item_seq);
        suffix
    }
}

fn tool_call_update(item: &TranscriptItem) -> Vec<SessionUpdate> {
    let TranscriptKind::ToolCall {
        call_id,
        status,
        preview,
        name,
        title,
        ..
    } = &item.kind
    else {
        return Vec::new();
    };
    let tool_status = tool_status_to_acp(*status);
    let mut content = Vec::new();
    if !preview.is_empty() {
        content.push(ToolCallContent::Content(Content::new(ContentBlock::Text(
            TextContent::new(preview.clone()),
        ))));
    }
    let fields = ToolCallUpdateFields::new()
        .kind(tool_kind_from_name(name))
        .title(title.clone())
        .status(tool_status)
        .content(content);
    vec![SessionUpdate::ToolCallUpdate(AcpToolCallUpdate::new(
        ToolCallId::new(call_id.as_str()),
        fields,
    ))]
}

fn final_item_update(
    state: &mut MappingState,
    item_seq: u64,
    text: &str,
    complete: bool,
    reasoning: bool,
) -> Vec<SessionUpdate> {
    if !complete {
        return Vec::new();
    }
    let Some(text) = state.take_final_text(item_seq, text) else {
        return Vec::new();
    };
    let content = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
    if reasoning {
        vec![SessionUpdate::AgentThoughtChunk(content)]
    } else {
        vec![SessionUpdate::AgentMessageChunk(content)]
    }
}

/// Maps an incoming pacode `Event` into zero or more ACP `SessionUpdate`s.
///
/// Unmapped events (toasts, plugins, agent metadata, etc.) return an empty `Vec` cleanly.
pub fn event_to_session_updates(event: &Event, state: &mut MappingState) -> Vec<SessionUpdate> {
    if let Event::TextDelta { agent, .. } | Event::ReasoningDelta { agent, .. } = event
        && !agent.is_main()
    {
        return Vec::new();
    }
    if let Event::ItemAdded(item) | Event::ItemUpdated(item) = event
        && !item.agent.is_main()
    {
        return Vec::new();
    }
    if let Event::ItemAdded(item) | Event::ItemUpdated(item) = event {
        state.mark_item_replayed(item.seq);
        state.remember_tool_call(item);
    }
    match event {
        Event::TextDelta {
            item_seq,
            text,
            agent: _,
        } => {
            if !state.record_delta(*item_seq, text) {
                return Vec::new();
            }
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text.clone())),
            ))]
        }
        Event::ReasoningDelta {
            item_seq,
            text,
            agent: _,
        } => {
            if !state.record_delta(*item_seq, text) {
                return Vec::new();
            }
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
                final_item_update(state, item.seq, text, *complete, false)
            }
            TranscriptKind::Reasoning { text, complete } => {
                final_item_update(state, item.seq, text, *complete, true)
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
            | TranscriptKind::Images { .. }
            | TranscriptKind::Notice { .. }
            | TranscriptKind::Permission(_)
            | TranscriptKind::Question { .. }
            | TranscriptKind::AgentMessage { .. } => vec![],
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
                final_item_update(state, item.seq, text, *complete, false)
            }
            TranscriptKind::Reasoning { text, complete } => {
                final_item_update(state, item.seq, text, *complete, true)
            }
            TranscriptKind::BashCommand { .. }
            | TranscriptKind::User { .. }
            | TranscriptKind::Images { .. }
            | TranscriptKind::Notice { .. }
            | TranscriptKind::Permission(_)
            | TranscriptKind::Question { .. }
            | TranscriptKind::AgentMessage { .. } => vec![],
        },
        Event::PlanUpdated(plan) => {
            vec![SessionUpdate::Plan(plan_to_acp(plan))]
        }
        // ACP has no way to retract transcript it already streamed; the plan
        // is the one piece of state a client keeps from us that a clear empties.
        Event::ContextCleared => {
            vec![SessionUpdate::Plan(plan_to_acp(&Plan::default()))]
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
        Event::TurnEnded { agent, .. } if agent.is_main() => {
            state.finish_turn();
            vec![]
        }
        Event::TurnEnded { .. } => vec![],
        Event::TurnStarted { .. }
        | Event::PermissionRequested(_)
        | Event::PermissionResolved { .. }
        // The explanation is a display hint attached to a pending permission;
        // ACP has no channel for it and the prompt itself is already a
        // `session/request_permission` round-trip.
        | Event::PermissionExplained { .. }
        | Event::AgentAdded(_)
        | Event::AgentUpdated(_)
        | Event::TaskAdded(_)
        | Event::TaskUpdated(_)
        // A running call's output belongs to the tool cell ACP already got as
        // an item update when the call started; there is no ACP channel for
        // "the row is still growing", and sending it as message chunks would
        // put tool output into the assistant's transcript.
        | Event::ToolOutput { .. }
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
        | Event::AuthUpdated(_)
        // Connection-level repair signal for stateful clients; ACP keeps no mirror.
        | Event::Resync { .. }
        // The model catalog is ACP `list_models` request data, not a session update.
        | Event::ModelsUpdated { .. }
        // The slot badge and the notice-lock badge are pacode client concerns;
        // ACP sessions are driven one at a time and have no footer chrome.
        | Event::SessionStatus { .. }
        | Event::NoticeLockChanged { .. } => vec![],
    }
}

/// Converts a static `TranscriptItem` into ACP session updates (used during history replay).
pub fn transcript_item_to_session_updates(item: &TranscriptItem) -> Vec<SessionUpdate> {
    if !item.agent.is_main() {
        return Vec::new();
    }
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
        TranscriptKind::Images { images } => images
            .iter()
            .filter_map(|image| {
                // Bytes live in the shared image store; an evicted image is skipped.
                let bytes = pacode_image::store::load(&image.hash).ok()?;
                Some(SessionUpdate::UserMessageChunk(ContentChunk::new(
                    ContentBlock::Image(ImageContent::new(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                        image.mime.clone(),
                    )),
                )))
            })
            .collect(),
        TranscriptKind::Permission(_)
        | TranscriptKind::Question { .. }
        | TranscriptKind::AgentMessage { .. } => vec![],
    }
}

/// Image blocks of an ACP prompt as daemon attachments, in order. The daemon
/// prepares (downscales, re-encodes) and stores them.
pub fn prompt_content_to_attachments(prompt: &[ContentBlock]) -> Vec<Attachment> {
    prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(img) => Some(Attachment {
                mime: img.mime_type.clone(),
                data: img.data.clone(),
                name: None,
            }),
            _ => None,
        })
        .collect()
}

/// Extracts plain prompt text from ACP prompt content blocks (images are left
/// out; see [`prompt_content_to_parts`]).
pub fn prompt_content_to_text(prompt: &[ContentBlock]) -> String {
    prompt_content_to_parts(prompt).0
}

/// Base64 length cap for one image block, matching the TUI's 20 MiB paste
/// limit (`IMAGE_PASTE_MAX_BYTES`) after decoding — 4/3 ratio plus padding.
/// Checked before decoding so an oversized payload never allocates the
/// decoded buffer.
const MAX_IMAGE_B64_LEN: usize = 20 * 1024 * 1024 * 4 / 3 + 8;

/// Extracts prompt text and attached images from ACP prompt content blocks.
///
/// Text blocks, embedded text resources and resource links fold into one
/// string; `Image` blocks decode into `ImageSource::Bytes`. An image that
/// fails base64 decoding or exceeds the size cap degrades to a text
/// placeholder rather than failing the whole prompt.
pub fn prompt_content_to_parts(prompt: &[ContentBlock]) -> (String, Vec<ImageSource>) {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    for block in prompt {
        match block {
            ContentBlock::Text(t) => {
                text_parts.push(t.text.clone());
            }
            ContentBlock::Image(img) => {
                let mime = &img.mime_type;
                if img.data.len() > MAX_IMAGE_B64_LEN {
                    text_parts.push(format!("[image: {mime}, too large]"));
                } else {
                    match base64::engine::general_purpose::STANDARD.decode(&img.data) {
                        Ok(data) => images.push(ImageSource::Bytes {
                            mime: mime.clone(),
                            data,
                        }),
                        Err(_) => text_parts.push(format!("[image: {mime}, undecodable]")),
                    }
                }
            }
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
    (text_parts.join(""), images)
}

#[cfg(test)]
#[path = "mapping_tests.rs"]
mod mapping_tests;
