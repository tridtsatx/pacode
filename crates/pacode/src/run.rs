//! `pacode run`: headless prompt execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pacode_client::{
    Client, ClientError, ClientEvent, ClientEventContext, ClientOptions, EventReceiver,
};
use pacode_config::Paths;
use pacode_types::ids::{ClientId, PermissionId};
use pacode_types::{
    AgentId, AgentInfo, AgentStatus, Attach, CallId, Effort, Event, Mode, ModelRoute,
    PermissionDecision, PermissionRequest, Reply, Request, SessionId, SessionSnapshot, ToastLevel,
    ToolStatus, TranscriptItem, TranscriptKind, TurnId, TurnStop,
};

const MAX_RETAINED_TRANSCRIPT_ITEMS: usize = 256;
const MAX_RETAINED_TOOL_STATUSES: usize = 256;
const MAX_RETAINED_PERMISSION_IDS: usize = 256;
const HEADLESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const INTERRUPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct RunOptions {
    pub prompt: String,
    pub json: bool,
    pub model: Option<ModelRoute>,
    pub effort: Option<Effort>,
    pub mode: Option<Mode>,
    pub cwd: PathBuf,
    pub socket: Option<PathBuf>,
    pub paths: Paths,
}

pub fn run(opts: RunOptions) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| anyhow::anyhow!("failed to create tokio runtime: {err}"))?;

    rt.block_on(run_headless(opts))
}

#[derive(Default)]
struct HeadlessItem {
    text: String,
    complete: bool,
}

#[derive(Default)]
struct SubagentFence {
    turn: Option<TurnId>,
}

struct PendingPermission {
    request: PermissionRequest,
    reply_sent: bool,
}

struct RunState {
    session_id: SessionId,
    context: ClientEventContext,
    item_floor: Option<u64>,
    event_floor: u64,
    snapshot_floor: u64,
    snapshot_ready: bool,
    global_event_floor: Option<u64>,
    items: HashMap<u64, HeadlessItem>,
    tool_statuses: HashMap<CallId, ToolStatus>,
    tool_status_order: VecDeque<CallId>,
    seen_permissions: HashSet<PermissionId>,
    seen_permission_order: VecDeque<PermissionId>,
    pending_permissions: HashMap<PermissionId, PendingPermission>,
    admission_turn: TurnId,
    active_turn: Option<TurnId>,
    terminal_turn: Option<TurnId>,
    terminal_stop: Option<TurnStop>,
    main_turn_active: bool,
    subagent_blocked_terminal: bool,
    redrive_pending: bool,
    redrive_expected: bool,
    subagents: HashMap<AgentId, SubagentFence>,
    prompt_submitted: bool,
}

impl RunState {
    fn new(
        snapshot: &SessionSnapshot,
        context: ClientEventContext,
        admission_turn: TurnId,
    ) -> Self {
        let item_floor = snapshot
            .transcript
            .iter()
            .filter(|item| {
                !matches!(
                    &item.kind,
                    TranscriptKind::Assistant {
                        complete: false,
                        ..
                    } | TranscriptKind::Reasoning {
                        complete: false,
                        ..
                    }
                )
            })
            .map(|item| item.seq)
            .max();
        let items: HashMap<u64, HeadlessItem> = snapshot
            .transcript
            .iter()
            .filter_map(|item| {
                if !item.agent.is_main() {
                    return None;
                }
                match &item.kind {
                    TranscriptKind::Assistant {
                        text,
                        complete: false,
                        ..
                    } => Some((
                        item.seq,
                        HeadlessItem {
                            text: text.clone(),
                            complete: false,
                        },
                    )),
                    _ => None,
                }
            })
            .collect();
        let mut state = Self {
            session_id: snapshot.meta.id.clone(),
            context,
            item_floor,
            event_floor: snapshot.seq,
            snapshot_floor: snapshot.seq,
            snapshot_ready: true,
            global_event_floor: None,
            items,
            tool_statuses: HashMap::new(),
            tool_status_order: VecDeque::new(),
            seen_permissions: HashSet::new(),
            seen_permission_order: VecDeque::new(),
            pending_permissions: HashMap::new(),
            admission_turn,
            active_turn: snapshot.active_turn.clone(),
            terminal_turn: None,
            terminal_stop: None,
            main_turn_active: snapshot.turn_active,
            subagent_blocked_terminal: false,
            redrive_pending: false,
            redrive_expected: false,
            subagents: HashMap::new(),
            prompt_submitted: false,
        };
        state.prune_items();
        for request in &snapshot.pending_permissions {
            state.observe_permission(request);
        }
        state.observe_snapshot_agents(&snapshot.agents);
        state
    }

    fn accepts_context(&self, context: &ClientEventContext) -> bool {
        context == &self.context && context.session.as_ref() == Some(&self.session_id)
    }

    fn observe_connected(&mut self, context: &ClientEventContext, client: &Client) -> bool {
        if self.context.session.as_ref() != Some(&self.session_id)
            || context.session.as_ref() != Some(&self.session_id)
            || context != &client.event_context()
            || context.connection_epoch < self.context.connection_epoch
            || context.session_epoch < self.context.session_epoch
        {
            return false;
        }
        let reconnected = context.connection_epoch > self.context.connection_epoch;
        self.context = context.clone();
        self.event_floor = self.event_floor.max(client.session_event_floor());
        if reconnected {
            self.snapshot_ready = false;
            self.event_floor = 0;
            self.snapshot_floor = 0;
            self.global_event_floor = None;
            self.reset_lifecycle_after_disconnect();
        }
        true
    }

    fn mark_disconnected(&mut self, context: &ClientEventContext) -> bool {
        if context.session.as_ref() != Some(&self.session_id)
            || context.connection_epoch < self.context.connection_epoch
            || context.session_epoch < self.context.session_epoch
        {
            return false;
        }
        self.snapshot_ready = false;
        self.reset_lifecycle_after_disconnect();
        true
    }

    fn mark_resync(&mut self, context: &ClientEventContext) -> bool {
        if !self.accepts_context(context) {
            return false;
        }
        self.snapshot_ready = false;
        true
    }

    fn reset_lifecycle_after_disconnect(&mut self) {
        for pending in self.pending_permissions.values_mut() {
            pending.reply_sent = false;
        }
        for subagent in self.subagents.values_mut() {
            subagent.turn = None;
        }
    }

    fn accept_event_for(
        &mut self,
        context: &ClientEventContext,
        current_context: &ClientEventContext,
        seq: u64,
        event: &Event,
    ) -> bool {
        if !self.accepts_context(context) || context != current_context {
            return false;
        }
        if event.is_global() {
            if self.global_event_floor.is_some_and(|floor| seq <= floor) {
                return false;
            }
            self.global_event_floor = Some(seq);
            return true;
        }
        if !self.snapshot_ready || seq <= self.event_floor {
            return false;
        }
        self.event_floor = seq;
        true
    }

    fn accept_event(
        &mut self,
        context: &ClientEventContext,
        seq: u64,
        event: &Event,
        client: &Client,
    ) -> bool {
        self.accept_event_for(context, &client.event_context(), seq, event)
    }

    fn accept_snapshot(
        &mut self,
        context: &ClientEventContext,
        snapshot: &SessionSnapshot,
        client: &Client,
    ) -> bool {
        if !self.accepts_context(context)
            || context != &client.event_context()
            || snapshot.meta.id != self.session_id
            || snapshot.seq < self.snapshot_floor
            || (snapshot.seq == self.snapshot_floor && self.snapshot_ready)
        {
            return false;
        }
        self.snapshot_floor = snapshot.seq;
        self.event_floor = self.event_floor.max(snapshot.seq);
        self.snapshot_ready = true;
        true
    }

    fn prune_items(&mut self) {
        while self.items.len() > MAX_RETAINED_TRANSCRIPT_ITEMS {
            let Some(seq) = self.items.keys().min().copied() else {
                break;
            };
            self.items.remove(&seq);
            self.item_floor = Some(self.item_floor.map_or(seq, |floor| floor.max(seq)));
        }
    }

    fn apply_delta(&mut self, agent: &AgentId, item_seq: u64, text: &str) -> Option<String> {
        if !agent.is_main()
            || text.is_empty()
            || self.item_floor.is_some_and(|floor| item_seq <= floor)
        {
            return None;
        }
        let output = {
            let item = self.items.entry(item_seq).or_default();
            if item.complete {
                return None;
            }
            item.text.push_str(text);
            text.to_owned()
        };
        self.prune_items();
        Some(output)
    }

    fn apply_item(&mut self, item: &TranscriptItem) -> Option<String> {
        let (text, complete) = match &item.kind {
            TranscriptKind::Assistant { text, complete } => (text, *complete),
            _ => return None,
        };
        if !item.agent.is_main() || self.item_floor.is_some_and(|floor| item.seq <= floor) {
            return None;
        }
        let output = self.items.entry(item.seq).or_default();
        if output.complete {
            return None;
        }
        if !complete {
            if !output.text.is_empty() && !text.starts_with(&output.text) {
                self.prune_items();
                return None;
            }
            output.text = text.clone();
            self.prune_items();
            return None;
        }
        let addition = authoritative_delta(&output.text, text);
        output.text = text.clone();
        output.complete = true;
        self.prune_items();
        (!addition.is_empty()).then_some(addition.to_owned())
    }

    fn apply_event(&mut self, event: &Event) -> Vec<String> {
        match event {
            Event::TextDelta {
                agent,
                item_seq,
                text,
            } => self
                .apply_delta(agent, *item_seq, text)
                .into_iter()
                .collect(),
            Event::ItemAdded(item) | Event::ItemUpdated(item) => {
                self.apply_item(item).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    fn should_replay_item(&self, item: &TranscriptItem) -> bool {
        item.agent.is_main()
            && (self.item_floor.is_none_or(|floor| item.seq > floor)
                || self.items.contains_key(&item.seq))
    }

    fn replay_snapshot(&mut self, snapshot: &SessionSnapshot) -> Vec<String> {
        let mut output = Vec::new();
        for item in &snapshot.transcript {
            if self.should_replay_item(item)
                && let Some(text) = self.apply_item(item)
            {
                output.push(text);
            }
        }
        output
    }

    fn observe_tool(&mut self, item: &TranscriptItem) -> Option<(String, ToolStatus)> {
        let TranscriptKind::ToolCall {
            call_id,
            title,
            status,
            ..
        } = &item.kind
        else {
            return None;
        };
        if self.tool_statuses.get(call_id) == Some(status) {
            return None;
        }
        if self
            .tool_statuses
            .insert(call_id.clone(), *status)
            .is_none()
        {
            self.tool_status_order.push_back(call_id.clone());
        }
        while self.tool_statuses.len() > MAX_RETAINED_TOOL_STATUSES {
            if let Some(evicted) = self.tool_status_order.pop_front() {
                self.tool_statuses.remove(&evicted);
            }
        }
        Some((title.clone(), *status))
    }

    fn observe_agent(&mut self, info: &AgentInfo) {
        if info.id.is_main() {
            return;
        }
        if info.status.is_live() {
            self.subagents.entry(info.id.clone()).or_default();
        } else if info.status == AgentStatus::Failed {
            self.subagents.remove(&info.id);
            self.redrive_expected = true;
            self.refresh_redrive();
        }
    }

    fn observe_snapshot_agents(&mut self, agents: &[AgentInfo]) {
        let live = agents
            .iter()
            .filter(|info| !info.id.is_main() && info.status.is_live())
            .map(|info| info.id.clone())
            .collect::<HashSet<_>>();
        for info in agents {
            if !info.id.is_main()
                && matches!(info.status, AgentStatus::Finished | AgentStatus::Failed)
            {
                self.redrive_expected = true;
            }
        }
        self.subagents.retain(|id, _| live.contains(id));
        for id in live {
            self.subagents.entry(id).or_default();
        }
        self.refresh_redrive();
    }

    fn prune_seen_permissions(&mut self) {
        while self.seen_permission_order.len() > MAX_RETAINED_PERMISSION_IDS {
            if let Some(evicted) = self.seen_permission_order.pop_front() {
                self.seen_permissions.remove(&evicted);
            }
        }
    }

    fn observe_permission(&mut self, request: &PermissionRequest) -> bool {
        if !self.seen_permissions.insert(request.id.clone()) {
            return false;
        }
        self.seen_permission_order.push_back(request.id.clone());
        self.pending_permissions.insert(
            request.id.clone(),
            PendingPermission {
                request: request.clone(),
                reply_sent: false,
            },
        );
        self.prune_seen_permissions();
        true
    }

    fn next_permission(&self) -> Option<(PermissionId, PermissionRequest)> {
        self.pending_permissions.iter().find_map(|(id, pending)| {
            (!pending.reply_sent).then(|| (id.clone(), pending.request.clone()))
        })
    }

    fn mark_permission_replied(&mut self, id: &PermissionId) {
        if let Some(pending) = self.pending_permissions.get_mut(id) {
            pending.reply_sent = true;
        }
    }

    fn reconcile_permissions(&mut self, snapshot: &SessionSnapshot) {
        let pending = snapshot
            .pending_permissions
            .iter()
            .map(|request| request.id.clone())
            .collect::<HashSet<_>>();
        for request in &snapshot.pending_permissions {
            self.observe_permission(request);
        }
        self.pending_permissions
            .retain(|id, _| pending.contains(id));
        for pending in self.pending_permissions.values_mut() {
            pending.reply_sent = false;
        }
    }

    fn finish_permission(&mut self, id: &PermissionId) {
        self.pending_permissions.remove(id);
        self.prune_seen_permissions();
    }

    fn observe_turn_started(&mut self, agent: &AgentId, turn: &TurnId) -> bool {
        if !agent.is_main() {
            self.subagents.insert(
                agent.clone(),
                SubagentFence {
                    turn: Some(turn.clone()),
                },
            );
            return true;
        }
        let relevant = turn == &self.admission_turn
            || self.active_turn.as_ref() == Some(turn)
            || self.terminal_turn.is_some()
            || self.redrive_pending;
        if !relevant {
            return false;
        }
        self.active_turn = Some(turn.clone());
        self.terminal_turn = None;
        self.terminal_stop = None;
        self.main_turn_active = true;
        self.subagent_blocked_terminal = false;
        self.redrive_pending = false;
        self.redrive_expected = false;
        true
    }

    fn observe_turn_ended(&mut self, agent: &AgentId, turn: &TurnId, stop: &TurnStop) -> bool {
        if agent.is_main() {
            if self.active_turn.as_ref() != Some(turn) {
                return false;
            }
            self.main_turn_active = false;
            self.terminal_turn = Some(turn.clone());
            self.terminal_stop = Some(stop.clone());
            if !self.subagents.is_empty() {
                self.subagent_blocked_terminal = true;
                self.redrive_pending = true;
            }
            return true;
        }
        if self
            .subagents
            .get(agent)
            .and_then(|subagent| subagent.turn.as_ref())
            .is_some_and(|current| current != turn)
        {
            return false;
        }
        self.subagents.remove(agent);
        if matches!(stop, TurnStop::Completed | TurnStop::Failed { .. }) {
            self.redrive_expected = true;
        }
        self.refresh_redrive();
        true
    }

    fn observe_snapshot_turn(&mut self, snapshot: &SessionSnapshot) {
        if snapshot.turn_active {
            if let Some(active) = &snapshot.active_turn
                && (active == &self.admission_turn
                    || self.active_turn.as_ref() == Some(active)
                    || self.terminal_turn.is_some()
                    || self.redrive_pending)
            {
                self.active_turn = Some(active.clone());
                self.terminal_turn = None;
                self.terminal_stop = None;
                self.main_turn_active = true;
                self.subagent_blocked_terminal = false;
                self.redrive_pending = false;
                self.redrive_expected = false;
            } else {
                self.main_turn_active = true;
            }
            return;
        }
        self.main_turn_active = false;
        if let Some(terminal) = &snapshot.terminal_turn
            && (terminal == &self.admission_turn || self.active_turn.as_ref() == Some(terminal))
        {
            self.active_turn = Some(terminal.clone());
            self.terminal_turn = Some(terminal.clone());
        }
    }

    fn refresh_redrive(&mut self) {
        if self.main_turn_active {
            self.redrive_pending = false;
        } else if self.terminal_turn.is_some() {
            self.redrive_pending = if self.subagents.is_empty() {
                self.subagent_blocked_terminal && self.redrive_expected
            } else {
                true
            };
        }
    }

    fn current_stop(&self) -> Option<TurnStop> {
        if !self.prompt_submitted || self.main_turn_active || self.terminal_turn.is_none() {
            return None;
        }
        let stop = self.terminal_stop.as_ref()?;
        if !matches!(stop, TurnStop::Completed) {
            return Some(stop.clone());
        }
        (!self.redrive_pending && self.subagents.is_empty() && self.pending_permissions.is_empty())
            .then(|| stop.clone())
    }

    fn mark_prompt_submitted(&mut self) {
        self.prompt_submitted = true;
    }

    fn snapshot_stop(&mut self, snapshot: &SessionSnapshot) -> Option<TurnStop> {
        if !self.prompt_submitted {
            return None;
        }
        self.observe_snapshot_turn(snapshot);
        let main = snapshot.agents.iter().find(|agent| agent.id.is_main());
        if self.main_turn_active || main.is_some_and(|agent| agent.status.is_active()) {
            return None;
        }
        if self.terminal_turn.is_none()
            || snapshot
                .terminal_turn
                .as_ref()
                .is_some_and(|terminal| self.terminal_turn.as_ref() != Some(terminal))
        {
            return None;
        }
        self.terminal_stop = Some(
            if snapshot.terminal_error.is_some()
                || main.is_some_and(|agent| {
                    agent.status == AgentStatus::Failed
                        || agent.error.is_some()
                        || agent.activity.as_deref() == Some("failed")
                })
            {
                TurnStop::Failed {
                    message: snapshot
                        .terminal_error
                        .clone()
                        .or_else(|| {
                            main.and_then(|agent| {
                                agent.error.clone().or_else(|| {
                                    (agent.activity.as_deref() == Some("failed"))
                                        .then(|| "main agent failed".to_string())
                                })
                            })
                        })
                        .unwrap_or_else(|| "main agent failed".to_string()),
                }
            } else if main.is_some_and(|agent| agent.status == AgentStatus::Stopped) {
                TurnStop::Interrupted
            } else if let Some(message) = snapshot_failure(snapshot) {
                TurnStop::Failed { message }
            } else {
                self.terminal_stop.clone().unwrap_or(TurnStop::Completed)
            },
        );
        self.refresh_redrive();
        self.current_stop()
    }
}

fn snapshot_failure(snapshot: &SessionSnapshot) -> Option<String> {
    snapshot.transcript.iter().find_map(|item| {
        if !item.agent.is_main() {
            return None;
        }
        let TranscriptKind::Notice { level, text } = &item.kind else {
            return None;
        };
        (*level == ToastLevel::Error && text.starts_with("turn failed:")).then(|| {
            text.strip_prefix("turn failed: ")
                .unwrap_or(text)
                .to_string()
        })
    })
}

fn authoritative_delta<'a>(existing: &str, canonical: &'a str) -> &'a str {
    if canonical.is_empty() {
        return "";
    }
    if let Some(remainder) = canonical.strip_prefix(existing) {
        return remainder;
    }
    if existing.starts_with(canonical) {
        return "";
    }
    let common = canonical
        .char_indices()
        .zip(existing.char_indices())
        .take_while(|((_, left), (_, right))| left == right)
        .last()
        .map(|((index, character), _)| index + character.len_utf8())
        .unwrap_or(0);
    if common == 0 {
        canonical
    } else {
        &canonical[common..]
    }
}

fn print_text(text: &str) {
    if text.is_empty() {
        return;
    }
    print!("{text}");
    let _ = std::io::stdout().flush();
}

fn print_stdout_line(line: &str) -> anyhow::Result<()> {
    println!("{line}");
    let _ = std::io::stdout().flush();
    Ok(())
}

fn json_event_line(
    state: &mut RunState,
    context: &ClientEventContext,
    current_context: &ClientEventContext,
    seq: u64,
    event: &Event,
) -> anyhow::Result<Option<String>> {
    if !state.accept_event_for(context, current_context, seq, event) {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&serde_json::json!({
        "seq": seq,
        "event": event,
    }))?))
}

fn snapshot_json_line(
    recovery_reason: Option<&str>,
    snapshot: &SessionSnapshot,
) -> anyhow::Result<String> {
    let value = match recovery_reason {
        Some(reason) => serde_json::json!({
            "type": "recovery",
            "reason": reason,
            "snapshot": snapshot,
        }),
        None => serde_json::json!({
            "type": "snapshot",
            "snapshot": snapshot,
        }),
    };
    Ok(serde_json::to_string(&value)?)
}

enum PromptAttempt {
    Submitted,
    Retry,
}

enum SnapshotOutcome {
    Ignored,
    Continue,
    Complete(TurnStop),
}

async fn submit_prompt(
    client: &Client,
    state: &mut RunState,
    prompt: &str,
    message_id: &str,
) -> anyhow::Result<PromptAttempt> {
    if state.prompt_submitted {
        return Ok(PromptAttempt::Submitted);
    }
    if !state.snapshot_ready
        || !client.is_connected()
        || !state.accepts_context(&client.event_context())
    {
        state.snapshot_ready = false;
        return Ok(PromptAttempt::Retry);
    }
    let reply = client
        .request_session_for(
            state.context.clone(),
            Request::UserMessage {
                text: prompt.to_string(),
                images: Vec::new(),
                message_id: Some(message_id.to_string()),
            },
        )
        .await;
    match reply {
        Ok(Reply::Ok) => {
            state.mark_prompt_submitted();
            Ok(PromptAttempt::Submitted)
        }
        Ok(Reply::Error { message }) => Err(anyhow::anyhow!("message send error: {message}")),
        Ok(other) => Err(anyhow::anyhow!(
            "message send error: unexpected reply: {other:?}"
        )),
        Err(ClientError::Disconnected | ClientError::StaleSession) => {
            state.snapshot_ready = false;
            Ok(PromptAttempt::Retry)
        }
        Err(error) => Err(anyhow::anyhow!("message send error: {error}")),
    }
}

async fn drain_pending_permissions(client: &Client, state: &mut RunState) -> anyhow::Result<()> {
    loop {
        let Some((id, request)) = state.next_permission() else {
            return Ok(());
        };
        eprintln!(
            "auto-denying permission request: {} ({})",
            request.title, request.tool
        );
        let _ = std::io::stderr().flush();
        let reply = client
            .request_session_for(
                state.context.clone(),
                Request::PermissionReply {
                    permission: id.clone(),
                    decision: PermissionDecision::Deny,
                    reason: Some(
                        "permission prompts are auto-denied in headless `pacode run`".to_string(),
                    ),
                },
            )
            .await;
        match reply {
            Ok(Reply::Ok) => state.mark_permission_replied(&id),
            Ok(Reply::Error { message }) => {
                state.finish_permission(&id);
                if message != "unknown permission request" {
                    return Err(anyhow::anyhow!("permission reply failed: {message}"));
                }
            }
            Ok(other) => {
                return Err(anyhow::anyhow!(
                    "permission reply failed: unexpected reply: {other:?}"
                ));
            }
            Err(ClientError::Disconnected | ClientError::StaleSession) => {
                state.snapshot_ready = false;
                state.reset_lifecycle_after_disconnect();
                return Ok(());
            }
            Err(error) => {
                return Err(anyhow::anyhow!("permission reply failed: {error}"));
            }
        }
    }
}

async fn fetch_recovery_snapshot(
    client: &Client,
    context: ClientEventContext,
    state: &RunState,
) -> Result<Option<SessionSnapshot>, ClientError> {
    if !state.accepts_context(&context) || !client.is_connected() {
        return Ok(None);
    }
    match client
        .request_session_for(context, Request::GetSnapshot)
        .await
    {
        Ok(Reply::Snapshot(snapshot)) => Ok(Some(snapshot)),
        Ok(Reply::Error { message }) => Err(ClientError::Daemon(message)),
        Ok(other) => Err(ClientError::UnexpectedReply {
            request: "get_snapshot".to_string(),
            reply: format!("{other:?}"),
        }),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
async fn recover_resync(
    client: &Client,
    context: ClientEventContext,
    state: &mut RunState,
) -> Vec<String> {
    match fetch_recovery_snapshot(client, context.clone(), state).await {
        Ok(Some(snapshot)) if state.accept_snapshot(&context, &snapshot, client) => {
            state.replay_snapshot(&snapshot)
        }
        Ok(_) => Vec::new(),
        Err(ClientError::Disconnected | ClientError::StaleSession) => Vec::new(),
        Err(error) => {
            eprintln!("snapshot recovery error: {error}");
            Vec::new()
        }
    }
}

struct SnapshotInput<'a> {
    context: &'a ClientEventContext,
    snapshot: &'a SessionSnapshot,
    json: bool,
    recovery_reason: Option<&'a str>,
    prompt: &'a str,
    message_id: &'a str,
}

async fn apply_snapshot(
    client: &Client,
    state: &mut RunState,
    input: SnapshotInput<'_>,
    prompt_retry: &mut bool,
) -> anyhow::Result<SnapshotOutcome> {
    let SnapshotInput {
        context,
        snapshot,
        json,
        recovery_reason,
        prompt,
        message_id,
    } = input;
    if !state.accept_snapshot(context, snapshot, client) {
        return Ok(SnapshotOutcome::Ignored);
    }
    state.reconcile_permissions(snapshot);
    state.observe_snapshot_agents(&snapshot.agents);
    state.observe_snapshot_turn(snapshot);
    let recovered = state.replay_snapshot(snapshot);
    if json {
        print_stdout_line(&snapshot_json_line(recovery_reason, snapshot)?)?;
    } else {
        for text in recovered {
            print_text(&text);
        }
    }
    for question in &snapshot.pending_questions {
        client
            .request_session_for(
                state.context.clone(),
                Request::AnswerQuestion {
                    question: question.id.clone(),
                    answer: pacode_types::QuestionAnswer::cancelled(),
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("question cancellation failed: {error}"))?;
    }
    drain_pending_permissions(client, state).await?;
    if *prompt_retry {
        match submit_prompt(client, state, prompt, message_id).await? {
            PromptAttempt::Submitted => *prompt_retry = false,
            PromptAttempt::Retry => {}
        }
    }
    if *prompt_retry {
        return Ok(SnapshotOutcome::Continue);
    }
    Ok(match state.snapshot_stop(snapshot) {
        Some(stop) => SnapshotOutcome::Complete(stop),
        None => SnapshotOutcome::Continue,
    })
}

fn finish_turn(stop: TurnStop, json: bool) -> anyhow::Result<()> {
    if !json {
        println!();
        let _ = std::io::stdout().flush();
    }
    match stop {
        TurnStop::Completed => Ok(()),
        TurnStop::Interrupted => Err(anyhow::anyhow!("turn interrupted")),
        TurnStop::Failed { message } => Err(anyhow::anyhow!("turn failed: {message}")),
    }
}

async fn wait_for_attached_snapshot(
    client: &Client,
    events: &mut EventReceiver,
) -> anyhow::Result<SessionSnapshot> {
    loop {
        let event = events
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("event stream closed before session attach"))?;
        match event {
            ClientEvent::Snapshot { context, snapshot }
                if context == client.event_context()
                    && client.session().as_ref() == Some(&snapshot.meta.id) =>
            {
                return Ok(snapshot);
            }
            ClientEvent::AttachFailed { attach, reason, .. } => {
                return Err(anyhow::anyhow!(
                    "session attach failed for {attach:?}: {reason}"
                ));
            }
            _ => {}
        }
    }
}

fn tool_status_str(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "running",
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
        ToolStatus::Backgrounded => "backgrounded",
        ToolStatus::Denied => "denied",
    }
}

async fn interrupt_all(client_slot: &Mutex<Option<Arc<Client>>>) {
    let client = {
        let guard = client_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    };
    let Some(client) = client else {
        return;
    };
    let deadline = tokio::time::Instant::now() + INTERRUPT_TIMEOUT;
    loop {
        let context = client.event_context();
        if context.session.is_some() && client.is_connected() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(
                remaining,
                client.request_session_for(context, Request::InterruptAll),
            )
            .await
            {
                Ok(Ok(_)) => return,
                Ok(Err(
                    ClientError::Disconnected | ClientError::StaleSession | ClientError::Timeout(_),
                )) => {}
                Ok(Err(_)) | Err(_) => return,
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(std::cmp::min(
            remaining,
            std::time::Duration::from_millis(25),
        ))
        .await;
    }
}

async fn run_headless(opts: RunOptions) -> anyhow::Result<()> {
    run_headless_with_timeout(opts, HEADLESS_TIMEOUT).await
}

async fn run_headless_with_timeout(
    opts: RunOptions,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let client_slot = Arc::new(Mutex::new(None));
    match tokio::time::timeout(timeout, run_headless_inner(opts, Arc::clone(&client_slot))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            interrupt_all(&client_slot).await;
            Err(error)
        }
        Err(_) => {
            interrupt_all(&client_slot).await;
            Err(anyhow::anyhow!(
                "timed out waiting for turn completion ({}s limit)",
                timeout.as_secs()
            ))
        }
    }
}

async fn run_headless_inner(
    opts: RunOptions,
    client_slot: Arc<Mutex<Option<Arc<Client>>>>,
) -> anyhow::Result<()> {
    let RunOptions {
        prompt,
        json,
        model,
        effort,
        mode,
        cwd,
        socket,
        paths,
    } = opts;
    let mut client_opts = ClientOptions::new(paths, pacode_config::APP_VERSION);
    client_opts.socket = socket;
    client_opts.spawn_daemon = true;

    let (client, mut events_rx) = Client::connect(client_opts)
        .await
        .map_err(|err| anyhow::anyhow!("connection error: {err}"))?;
    let client = Arc::new(client);
    *client_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&client));

    let snapshot = match client
        .attach(Attach::New {
            cwd,
            model,
            effort,
            mode,
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(ClientError::Disconnected | ClientError::StaleSession) => {
            wait_for_attached_snapshot(&client, &mut events_rx).await?
        }
        Err(error) => return Err(anyhow::anyhow!("attach error: {error}")),
    };
    let context = client.event_context();
    let message_id = format!("headless-{}", ClientId::generate());
    let admission_turn = TurnId::new(format!("trn_{message_id}"));
    let mut state = RunState::new(&snapshot, context, admission_turn);

    drain_pending_permissions(&client, &mut state).await?;
    let mut prompt_retry = match submit_prompt(&client, &mut state, &prompt, &message_id).await? {
        PromptAttempt::Submitted => false,
        PromptAttempt::Retry => true,
    };
    if let Some(stop) = state.current_stop() {
        return finish_turn(stop, json);
    }
    let mut recovery_reason: Option<&'static str> = None;
    let mut saw_disconnect = false;

    loop {
        if prompt_retry && state.snapshot_ready {
            match submit_prompt(&client, &mut state, &prompt, &message_id).await? {
                PromptAttempt::Submitted => prompt_retry = false,
                PromptAttempt::Retry => {}
            }
            if !prompt_retry && let Some(stop) = state.current_stop() {
                return finish_turn(stop, json);
            }
        }
        let client_event = events_rx.recv().await.ok_or_else(|| {
            let detail = if saw_disconnect {
                " after disconnect"
            } else {
                ""
            };
            anyhow::anyhow!("event stream closed before turn completion{detail}")
        })?;

        match client_event {
            ClientEvent::Connected { context, .. } => {
                let previous_epoch = state.context.connection_epoch;
                if state.observe_connected(&context, &client)
                    && context.connection_epoch > previous_epoch
                {
                    recovery_reason = Some("reconnect");
                }
            }
            ClientEvent::Snapshot { context, snapshot } => {
                match apply_snapshot(
                    &client,
                    &mut state,
                    SnapshotInput {
                        context: &context,
                        snapshot: &snapshot,
                        json,
                        recovery_reason,
                        prompt: &prompt,
                        message_id: &message_id,
                    },
                    &mut prompt_retry,
                )
                .await?
                {
                    SnapshotOutcome::Ignored => {}
                    SnapshotOutcome::Continue => recovery_reason = None,
                    SnapshotOutcome::Complete(stop) => {
                        return finish_turn(stop, json);
                    }
                }
            }
            ClientEvent::ResyncNeeded { context } => {
                if !state.mark_resync(&context) {
                    continue;
                }
                recovery_reason = Some("resync");
                match fetch_recovery_snapshot(&client, context.clone(), &state).await {
                    Ok(Some(snapshot)) => {
                        match apply_snapshot(
                            &client,
                            &mut state,
                            SnapshotInput {
                                context: &context,
                                snapshot: &snapshot,
                                json,
                                recovery_reason,
                                prompt: &prompt,
                                message_id: &message_id,
                            },
                            &mut prompt_retry,
                        )
                        .await?
                        {
                            SnapshotOutcome::Ignored => {}
                            SnapshotOutcome::Continue => recovery_reason = None,
                            SnapshotOutcome::Complete(stop) => {
                                return finish_turn(stop, json);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(ClientError::Disconnected | ClientError::StaleSession) => {
                        state.snapshot_ready = false;
                    }
                    Err(error) => {
                        return Err(anyhow::anyhow!("snapshot recovery error: {error}"));
                    }
                }
            }
            ClientEvent::Disconnected { context, .. } => {
                if state.mark_disconnected(&context) {
                    saw_disconnect = true;
                    recovery_reason = Some("disconnect");
                }
            }
            ClientEvent::Reconnecting { context, .. } => {
                if state.mark_disconnected(&context) {
                    saw_disconnect = true;
                    recovery_reason = Some("reconnect");
                }
            }
            ClientEvent::AttachFailed {
                context,
                attach,
                reason,
            } => {
                let targets_session = match &attach {
                    Attach::Resume { session } => session == &state.session_id,
                    Attach::New { .. } => context.session.as_ref() == Some(&state.session_id),
                    Attach::Latest { .. } => false,
                };
                if targets_session && context.connection_epoch >= state.context.connection_epoch {
                    return Err(anyhow::anyhow!(
                        "session attach failed for {attach:?}: {reason}"
                    ));
                }
            }
            ClientEvent::Event {
                context,
                seq,
                event,
            } => {
                let accepted = if json {
                    let current_context = client.event_context();
                    match json_event_line(&mut state, &context, &current_context, seq, &event)? {
                        Some(line) => {
                            print_stdout_line(&line)?;
                            true
                        }
                        None => false,
                    }
                } else {
                    state.accept_event(&context, seq, &event, &client)
                };
                if !accepted {
                    continue;
                }
                let recovered = state.apply_event(&event);
                if !json {
                    for text in recovered {
                        print_text(&text);
                    }
                    if let Event::ItemAdded(item) | Event::ItemUpdated(item) = &event
                        && let Some((title, status)) = state.observe_tool(item)
                    {
                        eprintln!("[tool] {title} … {}", tool_status_str(status));
                        let _ = std::io::stderr().flush();
                    }
                }
                match &event {
                    Event::TurnStarted { agent, turn } => {
                        state.observe_turn_started(agent, turn);
                    }
                    Event::TurnEnded {
                        agent, turn, stop, ..
                    } => {
                        state.observe_turn_ended(agent, turn, stop);
                    }
                    Event::AgentAdded(info) | Event::AgentUpdated(info) => {
                        state.observe_agent(info);
                    }
                    Event::PermissionRequested(request) => {
                        state.observe_permission(request);
                    }
                    Event::PermissionResolved { permission, .. } => {
                        state.finish_permission(permission);
                    }
                    _ => {}
                }
                if let Event::QuestionAsked(question) = &event {
                    client
                        .request_session_for(
                            state.context.clone(),
                            Request::AnswerQuestion {
                                question: question.id.clone(),
                                answer: pacode_types::QuestionAnswer::cancelled(),
                            },
                        )
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("question cancellation failed: {error}")
                        })?;
                }
                drain_pending_permissions(&client, &mut state).await?;
                if let Some(stop) = state.current_stop() {
                    return finish_turn(stop, json);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacode_types::ids::{AgentId, TurnId};
    use pacode_types::model::{Effort, ModelRoute};
    use pacode_types::state::{AgentKind, AgentStatus, Plan, SessionMeta, UsageTotals};
    use pacode_types::{Envelope, PROTOCOL_VERSION, ServerMessage, SessionStatusKind};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::net::unix::OwnedWriteHalf;
    use tokio::sync::mpsc;

    fn assistant_item(seq: u64, text: &str, complete: bool) -> TranscriptItem {
        TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::Assistant {
                text: text.to_string(),
                complete,
            },
        }
    }

    fn main_agent(status: AgentStatus, error: Option<&str>) -> AgentInfo {
        AgentInfo {
            id: AgentId::main(),
            name: "main".to_string(),
            kind: AgentKind::Main,
            status,
            activity: None,
            started_at_ms: 1,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("provider", "model"),
            effort: Effort::Low,
            parent: None,
            summary: None,
            error: error.map(str::to_string),
        }
    }

    fn sub_agent(id: &str, status: AgentStatus) -> AgentInfo {
        AgentInfo {
            id: AgentId::new(id),
            name: id.to_string(),
            kind: AgentKind::Sub,
            status,
            activity: None,
            started_at_ms: 1,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("provider", "model"),
            effort: Effort::Low,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        }
    }

    fn snapshot(seq: u64, transcript: Vec<TranscriptItem>) -> SessionSnapshot {
        SessionSnapshot {
            meta: SessionMeta {
                id: SessionId::new("ses_headless"),
                name: None,
                cwd: PathBuf::from("/tmp"),
                git_branch: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                model: ModelRoute::new("provider", "model"),
                effort: Effort::Low,
                mode: Mode::Build,
                first_prompt: None,
                zdr: false,
            },
            agents: Vec::new(),
            plan: Plan::default(),
            tasks: Vec::new(),
            usage: UsageTotals::default(),
            transcript,
            has_more_history: false,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
            turn_active: true,
            active_turn: None,
            terminal_turn: None,
            terminal_error: None,
            cron_jobs: Vec::new(),
            monitors: Vec::new(),
            seq,
        }
    }

    fn permission(id: &str) -> PermissionRequest {
        PermissionRequest {
            id: PermissionId::new(id),
            agent: AgentId::main(),
            agent_name: "main".to_string(),
            call_id: CallId::new("call_1"),
            tool: "bash".to_string(),
            title: "Bash: cargo test".to_string(),
            detail: String::new(),
            risk: None,
            created_at_ms: 1,
        }
    }

    fn admitted_turn(message_id: &str) -> TurnId {
        TurnId::new(format!("trn_{message_id}"))
    }

    async fn send_server_message(write_half: &mut OwnedWriteHalf, message: ServerMessage) {
        let mut encoded = serde_json::to_string(&message).unwrap();
        encoded.push('\n');
        write_half.write_all(encoded.as_bytes()).await.unwrap();
        write_half.flush().await.unwrap();
    }

    async fn run_permission_headless_daemon(
        listener: UnixListener,
        permissions: mpsc::UnboundedSender<(PermissionId, PermissionDecision)>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let Envelope { id, req } = serde_json::from_str(line.trim()).unwrap();
            match req {
                Request::Hello(_) => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Hello {
                                daemon_version: "t".to_string(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        },
                    )
                    .await;
                }
                Request::Attach(_) => {
                    let mut attached = snapshot(0, Vec::new());
                    attached.pending_permissions = vec![permission("perm_snapshot")];
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Attached(attached),
                        },
                    )
                    .await;
                }
                Request::PermissionReply {
                    permission,
                    decision,
                    ..
                } => {
                    permissions.send((permission.clone(), decision)).unwrap();
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Event {
                            seq: 1,
                            event: Event::PermissionResolved {
                                permission,
                                decision,
                                reason: Some("auto-denied".to_string()),
                            },
                        },
                    )
                    .await;
                }
                Request::UserMessage { message_id, .. } => {
                    let turn = admitted_turn(message_id.as_deref().unwrap());
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Event {
                            seq: 2,
                            event: Event::TurnStarted {
                                agent: AgentId::main(),
                                turn: turn.clone(),
                            },
                        },
                    )
                    .await;
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Event {
                            seq: 3,
                            event: Event::TurnEnded {
                                agent: AgentId::main(),
                                turn,
                                usage: None,
                                stop: TurnStop::Completed,
                            },
                        },
                    )
                    .await;
                    return;
                }
                _ => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                }
            }
        }
    }

    async fn run_reconnect_headless_daemon(
        listener: UnixListener,
        message_ids: mpsc::UnboundedSender<Option<String>>,
    ) {
        let mut connection = 0;
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let Envelope { id, req } = serde_json::from_str(line.trim()).unwrap();
                match req {
                    Request::Hello(_) => {
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id,
                                reply: Reply::Hello {
                                    daemon_version: "t".to_string(),
                                    protocol: PROTOCOL_VERSION,
                                    pid: 1,
                                },
                            },
                        )
                        .await;
                    }
                    Request::Attach(_) => {
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id,
                                reply: Reply::Attached(snapshot(0, Vec::new())),
                            },
                        )
                        .await;
                    }
                    Request::UserMessage { message_id, .. } => {
                        message_ids.send(message_id.clone()).unwrap();
                        if connection == 0 {
                            break;
                        }
                        let turn = admitted_turn(message_id.as_deref().unwrap());
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id,
                                reply: Reply::Ok,
                            },
                        )
                        .await;
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 1,
                                event: Event::TurnStarted {
                                    agent: AgentId::main(),
                                    turn: turn.clone(),
                                },
                            },
                        )
                        .await;
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Event {
                                seq: 2,
                                event: Event::TurnEnded {
                                    agent: AgentId::main(),
                                    turn,
                                    usage: None,
                                    stop: TurnStop::Completed,
                                },
                            },
                        )
                        .await;
                        return;
                    }
                    _ => {
                        send_server_message(
                            &mut write_half,
                            ServerMessage::Reply {
                                id,
                                reply: Reply::Ok,
                            },
                        )
                        .await;
                    }
                }
            }
            connection += 1;
        }
    }

    async fn run_error_headless_daemon(
        listener: UnixListener,
        interrupted: mpsc::UnboundedSender<bool>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let Envelope { id, req } = serde_json::from_str(line.trim()).unwrap();
            match req {
                Request::Hello(_) => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Hello {
                                daemon_version: "t".to_string(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        },
                    )
                    .await;
                }
                Request::Attach(_) => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Attached(snapshot(0, Vec::new())),
                        },
                    )
                    .await;
                }
                Request::UserMessage { .. } => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Error {
                                message: "rejected".to_string(),
                            },
                        },
                    )
                    .await;
                }
                Request::InterruptAll => {
                    interrupted.send(true).unwrap();
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                    return;
                }
                _ => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                }
            }
        }
    }

    async fn run_timeout_headless_daemon(
        listener: UnixListener,
        interrupted: mpsc::UnboundedSender<bool>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let Envelope { id, req } = serde_json::from_str(line.trim()).unwrap();
            match req {
                Request::Hello(_) => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Hello {
                                daemon_version: "t".to_string(),
                                protocol: PROTOCOL_VERSION,
                                pid: 1,
                            },
                        },
                    )
                    .await;
                }
                Request::Attach(_) => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Attached(snapshot(0, Vec::new())),
                        },
                    )
                    .await;
                }
                Request::UserMessage { .. } => {}
                Request::InterruptAll => {
                    interrupted.send(true).unwrap();
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                    return;
                }
                _ => {
                    send_server_message(
                        &mut write_half,
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Ok,
                        },
                    )
                    .await;
                }
            }
        }
    }

    fn context(connection_epoch: u64) -> ClientEventContext {
        ClientEventContext {
            connection_epoch,
            session_epoch: 1,
            session: Some(SessionId::new("ses_headless")),
        }
    }

    fn run_state(snapshot: &SessionSnapshot, connection_epoch: u64) -> RunState {
        RunState::new(
            snapshot,
            context(connection_epoch),
            TurnId::new("trn_headless"),
        )
    }

    #[test]
    fn dropped_middle_delta_prints_authoritative_replacement_once() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        assert_eq!(
            state.apply_event(&Event::TextDelta {
                agent: AgentId::main(),
                item_seq: 3,
                text: "hello ".into(),
            }),
            ["hello "]
        );
        assert_eq!(
            state.apply_event(&Event::TextDelta {
                agent: AgentId::main(),
                item_seq: 3,
                text: "world".into(),
            }),
            ["world"]
        );
        assert_eq!(
            state.apply_event(&Event::ItemUpdated(assistant_item(
                3,
                "hello brave world",
                true,
            ))),
            ["brave world"]
        );
        assert!(
            state
                .apply_event(&Event::ItemUpdated(assistant_item(
                    3,
                    "hello brave world",
                    true,
                )))
                .is_empty()
        );
    }

    #[test]
    fn headless_resync_marker_replays_final_without_double_output() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        state.apply_event(&Event::TextDelta {
            agent: AgentId::main(),
            item_seq: 4,
            text: "partial".into(),
        });
        let recovered = snapshot(
            5,
            vec![assistant_item(4, "partial authoritative text", true)],
        );
        assert_eq!(state.replay_snapshot(&recovered), [" authoritative text"]);
        assert!(state.replay_snapshot(&recovered).is_empty());
    }

    #[test]
    fn resync_replays_a_partial_authoritative_item_without_duplicate_prefix() {
        let initial = snapshot(1, vec![assistant_item(4, "partial", false)]);
        let mut state = run_state(&initial, 1);
        assert!(state.replay_snapshot(&initial).is_empty());
        let recovered = snapshot(2, vec![assistant_item(4, "partial final", true)]);
        assert_eq!(state.replay_snapshot(&recovered), [" final"]);
        assert!(state.replay_snapshot(&recovered).is_empty());
    }

    #[test]
    fn json_snapshot_and_recovery_lines_have_stable_envelopes() {
        let snapshot = snapshot(3, Vec::new());
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_json_line(None, &snapshot).unwrap()).unwrap();
        assert_eq!(value["type"], "snapshot");
        assert!(value["snapshot"].is_object());
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_json_line(Some("resync"), &snapshot).unwrap()).unwrap();
        assert_eq!(value["type"], "recovery");
        assert_eq!(value["reason"], "resync");
        assert!(value["snapshot"].is_object());
    }

    #[test]
    fn json_event_line_drops_stale_context_and_duplicate_sequence() {
        let mut state = run_state(&snapshot(0, Vec::new()), 1);
        let event = Event::TextDelta {
            agent: AgentId::main(),
            item_seq: 2,
            text: "answer".into(),
        };
        assert!(
            json_event_line(&mut state, &context(0), &context(1), 2, &event,)
                .unwrap()
                .is_none()
        );
        assert!(state.event_floor == 0);
        assert!(
            json_event_line(&mut state, &context(1), &context(1), 2, &event)
                .unwrap()
                .is_some()
        );
        assert!(
            json_event_line(&mut state, &context(1), &context(1), 2, &event)
                .unwrap()
                .is_none()
        );
        let line = json_event_line(&mut state, &context(1), &context(1), 3, &event)
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["seq"], 3);
        assert!(value.get("type").is_none());
        let global = Event::SessionStatus {
            session: SessionId::new("ses_headless"),
            status: SessionStatusKind::Working,
        };
        assert!(
            json_event_line(&mut state, &context(1), &context(1), 4, &global)
                .unwrap()
                .is_some()
        );
        assert!(
            json_event_line(&mut state, &context(1), &context(1), 4, &global)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn idle_snapshot_completes_after_submitted_prompt() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        state.mark_prompt_submitted();
        let mut foreign = snapshot(2, Vec::new());
        foreign.turn_active = false;
        foreign.terminal_turn = Some(TurnId::new("trn_foreign"));
        foreign.terminal_error = Some("foreign failure".to_string());
        foreign.agents = vec![main_agent(AgentStatus::Idle, None)];
        assert!(state.snapshot_stop(&foreign).is_none());
        let mut idle = snapshot(2, Vec::new());
        idle.turn_active = false;
        idle.terminal_turn = Some(TurnId::new("trn_headless"));
        idle.agents = vec![main_agent(AgentStatus::Idle, None)];
        assert_eq!(state.snapshot_stop(&idle), Some(TurnStop::Completed));
        idle.agents[0].status = AgentStatus::Failed;
        idle.agents[0].error = Some("provider exploded".to_string());
        assert!(matches!(
            state.snapshot_stop(&idle),
            Some(TurnStop::Failed { message }) if message == "provider exploded"
        ));
        let mut failed_notice = snapshot(3, Vec::new());
        failed_notice.turn_active = false;
        failed_notice.terminal_turn = Some(TurnId::new("trn_headless"));
        failed_notice.agents = vec![main_agent(AgentStatus::Idle, None)];
        failed_notice.transcript = vec![TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::Notice {
                level: ToastLevel::Error,
                text: "turn failed: snapshot failure".to_string(),
            },
        }];
        assert!(matches!(
            state.snapshot_stop(&failed_notice),
            Some(TurnStop::Failed { message }) if message == "snapshot failure"
        ));
    }

    #[test]
    fn turn_completion_uses_matching_terminal_fence() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        let current = TurnId::new("trn_headless");
        let stale = TurnId::new("trn_stale");
        state.mark_prompt_submitted();

        assert!(!state.observe_turn_started(&AgentId::main(), &stale));
        assert!(!state.observe_turn_ended(&AgentId::main(), &stale, &TurnStop::Completed,));
        assert!(state.current_stop().is_none());
        assert!(state.observe_turn_started(&AgentId::main(), &current));
        assert!(!state.observe_turn_ended(&AgentId::main(), &stale, &TurnStop::Completed,));
        assert!(state.current_stop().is_none());
        assert!(state.observe_turn_ended(&AgentId::main(), &current, &TurnStop::Completed,));
        assert_eq!(state.current_stop(), Some(TurnStop::Completed));
    }

    #[test]
    fn completed_turn_waits_for_permission_resolution() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        let turn = TurnId::new("trn_headless");
        let request = permission("perm_lifecycle");
        state.mark_prompt_submitted();
        assert!(state.observe_permission(&request));
        let (id, _) = state.next_permission().unwrap();
        state.mark_permission_replied(&id);
        assert!(state.next_permission().is_none());
        assert!(state.observe_turn_started(&AgentId::main(), &turn));
        assert!(state.observe_turn_ended(&AgentId::main(), &turn, &TurnStop::Completed,));
        assert!(state.current_stop().is_none());

        state.finish_permission(&id);
        assert_eq!(state.current_stop(), Some(TurnStop::Completed));
    }

    #[test]
    fn subagent_completion_requires_the_followup_main_turn() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        let main_turn = TurnId::new("trn_headless");
        let subagent = AgentId::new("agt_sub");
        let subagent_turn = TurnId::new("trn_sub");
        let followup = TurnId::new("trn_followup");
        state.mark_prompt_submitted();
        state.observe_agent(&sub_agent("agt_sub", AgentStatus::RunningTool));
        assert!(state.observe_turn_started(&AgentId::main(), &main_turn));
        assert!(state.observe_turn_ended(&AgentId::main(), &main_turn, &TurnStop::Completed,));
        assert!(state.current_stop().is_none());
        assert!(state.observe_turn_started(&subagent, &subagent_turn));
        state.observe_agent(&sub_agent("agt_sub", AgentStatus::Finished));
        assert!(state.current_stop().is_none());
        assert!(state.observe_turn_ended(&subagent, &subagent_turn, &TurnStop::Completed,));
        assert!(state.current_stop().is_none());
        assert!(state.observe_turn_started(&AgentId::main(), &followup));
        assert!(state.observe_turn_ended(&AgentId::main(), &followup, &TurnStop::Completed,));
        assert_eq!(state.current_stop(), Some(TurnStop::Completed));
    }

    #[test]
    fn retained_transcript_and_tool_state_are_bounded() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        let extra = 32;
        for seq in 1..=MAX_RETAINED_TRANSCRIPT_ITEMS + extra {
            state.apply_event(&Event::ItemUpdated(assistant_item(
                seq as u64,
                &format!("item-{seq}"),
                true,
            )));
        }
        assert_eq!(state.items.len(), MAX_RETAINED_TRANSCRIPT_ITEMS);
        assert_eq!(state.item_floor, Some(extra as u64));
        assert!(
            state
                .apply_event(&Event::ItemUpdated(assistant_item(1, "evicted", true)))
                .is_empty()
        );

        for index in 0..MAX_RETAINED_TOOL_STATUSES + extra {
            let call_id = CallId::new(format!("call_{index}"));
            let item = TranscriptItem {
                seq: index as u64 + 1,
                agent: AgentId::main(),
                ts_ms: 1,
                kind: TranscriptKind::ToolCall {
                    call_id: call_id.clone(),
                    name: "bash".to_string(),
                    title: "bash".to_string(),
                    intent: None,
                    status: ToolStatus::Running,
                    preview: String::new(),
                    diff: None,
                    duration_ms: None,
                    task: None,
                },
            };
            assert!(state.observe_tool(&item).is_some());
        }
        assert_eq!(state.tool_statuses.len(), MAX_RETAINED_TOOL_STATUSES);
        assert_eq!(state.tool_status_order.len(), MAX_RETAINED_TOOL_STATUSES);
    }

    #[test]
    fn initial_snapshot_permissions_are_queued_for_denial() {
        let request = permission("perm_snapshot");
        let mut initial = snapshot(1, Vec::new());
        initial.pending_permissions = vec![request.clone()];
        let state = run_state(&initial, 1);
        assert_eq!(state.next_permission().unwrap().0, request.id);
    }

    #[test]
    fn permission_ids_are_deduplicated_until_resolution() {
        let mut state = run_state(&snapshot(1, Vec::new()), 1);
        let request = permission("perm_1");
        assert!(state.observe_permission(&request));
        assert!(!state.observe_permission(&request));
        let (id, pending) = state.next_permission().unwrap();
        assert_eq!(id, request.id);
        assert_eq!(pending.id, request.id);
        state.finish_permission(&id);
        assert!(!state.observe_permission(&request));
        assert!(state.next_permission().is_none());
    }

    #[tokio::test]
    async fn run_headless_denies_snapshot_permissions_once() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run-permission.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (permission_tx, mut permission_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(run_permission_headless_daemon(listener, permission_tx));

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_headless(RunOptions {
                prompt: "use a tool".to_string(),
                json: false,
                model: None,
                effort: None,
                mode: None,
                cwd: PathBuf::from("/tmp"),
                socket: Some(socket),
                paths: Paths::under(directory.path()),
            }),
        )
        .await
        .unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            permission_rx.recv().await.unwrap(),
            (PermissionId::new("perm_snapshot"), PermissionDecision::Deny)
        );
        assert!(permission_rx.try_recv().is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_headless_reconnects_and_retries_submitted_message_id() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run-reconnect.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (message_ids_tx, mut message_ids_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(run_reconnect_headless_daemon(listener, message_ids_tx));

        let result = tokio::time::timeout(
            Duration::from_secs(8),
            run_headless(RunOptions {
                prompt: "finish this".to_string(),
                json: true,
                model: None,
                effort: None,
                mode: None,
                cwd: PathBuf::from("/tmp"),
                socket: Some(socket),
                paths: Paths::under(directory.path()),
            }),
        )
        .await
        .unwrap();
        assert!(result.is_ok(), "{result:?}");
        let first = message_ids_rx.recv().await.unwrap();
        let second = message_ids_rx.recv().await.unwrap();
        assert_eq!(first, second);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_headless_interrupts_all_after_errors() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run-error.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (interrupted_tx, mut interrupted_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(run_error_headless_daemon(listener, interrupted_tx));

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_headless(RunOptions {
                prompt: "fail".to_string(),
                json: false,
                model: None,
                effort: None,
                mode: None,
                cwd: PathBuf::from("/tmp"),
                socket: Some(socket),
                paths: Paths::under(directory.path()),
            }),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(interrupted_rx.recv().await.is_some());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_headless_interrupts_all_after_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run-timeout.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (interrupted_tx, mut interrupted_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(run_timeout_headless_daemon(listener, interrupted_tx));

        let result = tokio::time::timeout(
            Duration::from_secs(6),
            run_headless_with_timeout(
                RunOptions {
                    prompt: "hang".to_string(),
                    json: false,
                    model: None,
                    effort: None,
                    mode: None,
                    cwd: PathBuf::from("/tmp"),
                    socket: Some(socket),
                    paths: Paths::under(directory.path()),
                },
                Duration::from_millis(100),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(interrupted_rx.recv().await.is_some());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resync_marker_requests_snapshot_with_current_context() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("run-resync.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (snapshot_request_tx, mut snapshot_request_rx) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let Envelope { id, req } = serde_json::from_str(line.trim()).unwrap();
                let messages = match req {
                    Request::Hello(_) => vec![ServerMessage::Reply {
                        id,
                        reply: Reply::Hello {
                            daemon_version: "t".to_string(),
                            protocol: PROTOCOL_VERSION,
                            pid: 1,
                        },
                    }],
                    Request::Attach(_) => vec![
                        ServerMessage::Reply {
                            id,
                            reply: Reply::Attached(snapshot(0, Vec::new())),
                        },
                        ServerMessage::Resync { current_seq: None },
                    ],
                    Request::GetSnapshot => {
                        snapshot_request_tx.send(()).unwrap();
                        vec![ServerMessage::Reply {
                            id,
                            reply: Reply::Snapshot(snapshot(
                                5,
                                vec![assistant_item(4, "partial authoritative text", true)],
                            )),
                        }]
                    }
                    _ => vec![ServerMessage::Reply {
                        id,
                        reply: Reply::Ok,
                    }],
                };
                for message in messages {
                    let mut encoded = serde_json::to_string(&message).unwrap();
                    encoded.push('\n');
                    write_half.write_all(encoded.as_bytes()).await.unwrap();
                    write_half.flush().await.unwrap();
                }
            }
        });

        let mut options = ClientOptions::new(Paths::under(directory.path()), "t");
        options.socket = Some(socket);
        options.spawn_daemon = false;
        let (client, mut events) = Client::connect(options).await.unwrap();
        assert!(matches!(
            events.recv().await,
            Some(ClientEvent::Connected { .. })
        ));
        let attached = client
            .attach(Attach::New {
                cwd: PathBuf::from("/tmp"),
                model: None,
                effort: None,
                mode: None,
            })
            .await
            .unwrap();
        let marker = events.recv().await.unwrap();
        let ClientEvent::ResyncNeeded { context } = marker else {
            panic!("expected resync marker")
        };
        let mut state = RunState::new(&attached, context.clone(), TurnId::new("trn_headless"));
        state.apply_event(&Event::TextDelta {
            agent: AgentId::main(),
            item_seq: 4,
            text: "partial".into(),
        });

        assert_eq!(
            recover_resync(&client, context, &mut state).await,
            [" authoritative text"]
        );
        assert!(snapshot_request_rx.try_recv().is_ok());
        assert!(
            state
                .apply_event(&Event::ItemUpdated(assistant_item(
                    4,
                    "partial authoritative text",
                    true,
                )))
                .is_empty()
        );

        client.close().await;
        server.await.unwrap();
    }
}
