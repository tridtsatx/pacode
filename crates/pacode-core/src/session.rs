//! `Session`: one conversation with a main agent and its subagents.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use pacode_exec::TaskManager;
use pacode_mcp::McpPool;
use pacode_provider::ProviderRegistry;
use pacode_store::Store;
use pacode_tools::{AgentSpec, ToolRegistry};
use pacode_types::{
    AgentId, AgentInfo, CallId, Config, Effort, Mode, ModelFlags, ModelRoute, PermissionDecision,
    PermissionId, PermissionRequest, Plan, SessionId, SessionMeta, SessionSnapshot, TaskInfo,
    TurnId, UsageTotals, ZdrMode,
};

use crate::CoreError;
use crate::agent::Agent;
use crate::permissions::PermissionState;
use crate::transcript::EventSink;

const MAX_RECOVERY_ADMISSIONS: usize = 256;

fn optimistic_snapshot<T>(events: &EventSink, mut read: impl FnMut(u64) -> T) -> T {
    let mut attempts = 0;
    loop {
        let seq_before = events.last_seq();
        let state = read(seq_before);
        let seq_after = events.last_seq();
        if seq_before == seq_after || attempts >= 16 {
            return state;
        }
        attempts += 1;
        std::thread::yield_now();
    }
}

struct DetachPlan {
    new_id: AgentId,
    detached_info: AgentInfo,
    detached_tools: ToolRegistry,
    fresh_main: Arc<Agent>,
    turn_prompt: Option<String>,
}

/// Take every agent's turn lock, holding them all until the returned guards
/// drop. A running turn owns its agent's lock, and a turn can call back into
/// `submit_lock` (spawning a subagent is a tool call), so the only order these
/// locks may be taken in is the one the turn already holds: turn lock first,
/// `submit_lock` second. `agents` must come from the `AgentId`-ordered map so
/// two callers locking the same session cannot cross.
pub(crate) async fn lock_turns(agents: &[Arc<Agent>]) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut guards = Vec::with_capacity(agents.len());
    for agent in agents {
        guards.push(agent.turn_lock.clone().lock_owned().await);
    }
    guards
}

enum RecoveryResponseState {
    Missing,
    Incomplete { dangling_tools: bool },
    Complete,
}

fn detached_message_rows(main: &Agent) -> Vec<(u64, Arc<pacode_types::Message>)> {
    let mut rows = main
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .rows();
    let pending = main.persisted_pending_user_messages();
    let excluded_seqs = main.discarded_message_seqs();
    rows.retain(|(seq, _)| !excluded_seqs.contains(seq));
    rows.extend(pending);
    rows.sort_unstable_by_key(|(seq, _)| *seq);
    rows
}

pub struct Session {
    pub id: SessionId,
    pub meta: RwLock<SessionMeta>,
    /// Main agent plus subagents, insertion order = spawn order.
    pub agents: RwLock<BTreeMap<AgentId, Arc<Agent>>>,
    pub(crate) agent_aliases: std::sync::Mutex<HashMap<AgentId, Arc<Agent>>>,
    pub plan: RwLock<Plan>,
    pub usage: RwLock<UsageTotals>,
    pub permissions: PermissionState,
    /// Questions the model asked and is waiting on.
    pub questions: crate::questions::QuestionState,
    pub events: EventSink,
    pub tasks: Arc<TaskManager>,
    pub store: Store,
    pub submit_lock: tokio::sync::Mutex<()>,
    pub(crate) closed: AtomicBool,
    pub(crate) recovery_admissions: std::sync::Mutex<VecDeque<(AgentId, String)>>,
    pub config: Arc<Config>,
    pub providers: Arc<ProviderRegistry>,
    /// Built-in + MCP tools available in this session (subagents get subsets).
    pub tools: RwLock<ToolRegistry>,
    pub mcp: Arc<McpPool>,
    pub plugins: Arc<pacode_plugin::PluginHost>,
    pub app_version: String,
    pub skills: Arc<pacode_skills::SkillRegistry>,
    /// Cron jobs and monitors of this session. Dropping the session stops them.
    pub scheduler: Arc<crate::schedule::Scheduler>,
    /// Per-route user flags (`zdr_capable`, `favorite`) loaded from `prefs.toml`
    /// at session open; `SetModelFlag` updates this copy, the TUI persists it.
    pub model_flags: RwLock<HashMap<String, ModelFlags>>,
    /// Clone of `Core::global_events` (shared seq + channel): how a session
    /// publishes `Event::SessionStatus` to clients attached to other sessions.
    pub global_events: EventSink,
    pub(crate) idle_watch: crate::core::IdleWatch,
    /// Where this session's tokens went. Fed on the paths that already know the
    /// numbers — one record per provider request, one per tool call — so the
    /// accounting costs a read of what was computed rather than a second
    /// estimate. Shared, because the report is built for a client that may not
    /// be the one whose turn produced the records.
    pub accounting: Arc<crate::analyze::SessionAccounting>,
}

impl Session {
    /// Record the answer on the question's transcript item, so the conversation
    /// shows what was asked and what was chosen rather than only the tool result.
    pub fn record_question_answer(
        &self,
        question: &pacode_types::Question,
        answer: &pacode_types::QuestionAnswer,
    ) {
        let Some(agent) = self.agent_for_host(&question.agent) else {
            return;
        };
        let seq = {
            let transcript = agent.transcript.lock().unwrap_or_else(|p| p.into_inner());
            transcript.find_question_seq(&question.id)
        };
        let Some(seq) = seq else {
            return;
        };
        let item = pacode_types::TranscriptItem {
            seq,
            agent: question.agent.clone(),
            ts_ms: pacode_types::time::now_ms(),
            kind: pacode_types::TranscriptKind::Question {
                question: question.clone(),
                answer: Some(answer.clone()),
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(pacode_types::Event::ItemUpdated(item));
    }

    /// Tell the main agent that a monitor it started has fired. Injections reach
    /// the model between steps, never mid-stream (spec §6.2).
    pub fn inject_monitor_fired(self: &Arc<Self>, monitor: &pacode_types::MonitorInfo) {
        let Some(main) = self.main_agent() else {
            return;
        };
        let text = format!(
            "Monitor {} fired: {}",
            monitor.label,
            monitor.condition.describe()
        );
        self.inject_and_wake(&main, crate::inject::Injection::SystemNotice(text));
    }

    /// Queue an injection for `agent` and, when it is the idle main agent, start
    /// the turn that delivers it. A notice pushed at an idle agent would
    /// otherwise sit in the queue until some unrelated event started a turn.
    /// Subagents are never woken here: their only driver is their own turn.
    pub(crate) fn inject_and_wake(
        self: &Arc<Self>,
        agent: &Arc<Agent>,
        injection: crate::inject::Injection,
    ) {
        if self.hold_notice(agent, &injection) {
            return;
        }
        let wake = agent.id().is_main();
        agent.injections.push(injection);
        if wake && !agent.is_running() && !agent.stop_requested() {
            crate::turn::lifecycle::request_turn(self.clone(), agent.clone());
        }
    }

    /// Park `injection` in `agent`'s notice lock when the lock is on. `true`
    /// means the caller must *not* push it to the live queue and must not start
    /// a turn for it — a turn woken by a parked notice delivers nothing and
    /// costs a whole model round trip.
    ///
    /// The badge event fires only when the count actually moved: a twenty-notice
    /// backlog must not cost twenty events.
    pub(crate) fn hold_notice(
        &self,
        agent: &Arc<Agent>,
        injection: &crate::inject::Injection,
    ) -> bool {
        let before = agent.notice_lock.held();
        if !agent.notice_lock.hold_if_locked(injection) {
            return false;
        }
        let held = agent.notice_lock.held();
        if held != before {
            self.emit_notice_lock(agent, held);
        }
        true
    }

    /// The badge numbers for `agent`'s lock, as the wire carries them. Emitted
    /// on a transition only — the flip, an arrival that moved the count, or a
    /// release that took one off — never once per drain step.
    pub(crate) fn emit_notice_lock(&self, agent: &Arc<Agent>, held: usize) {
        self.events.emit(pacode_types::Event::NoticeLockChanged {
            session: self.id.clone(),
            locked: agent.notice_lock.is_locked(),
            held,
            dropped: agent.notice_lock.dropped(),
        });
    }

    pub fn main_agent(&self) -> Option<Arc<Agent>> {
        self.agents.read().ok()?.get(&AgentId::main()).cloned()
    }

    pub fn agent(&self, id: &AgentId) -> Option<Arc<Agent>> {
        self.agents.read().ok()?.get(id).cloned()
    }

    /// The transcript row `call` is drawing into, while it is still running.
    /// Output arriving after the call finished has nowhere to go: its result
    /// already replaced the live preview.
    pub(crate) fn running_tool_call_seq(&self, agent: &AgentId, call: &CallId) -> Option<u64> {
        let agent = self.agent_for_host(agent)?;
        let transcript = agent.transcript.lock().unwrap_or_else(|p| p.into_inner());
        transcript.running_tool_call_seq(call)
    }

    pub(crate) fn agent_for_host(&self, id: &AgentId) -> Option<Arc<Agent>> {
        self.agent(id).or_else(|| {
            self.agent_aliases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(id)
                .cloned()
        })
    }

    pub(crate) fn queue_recovery_admissions(
        &self,
        agent: &AgentId,
        admissions: impl IntoIterator<Item = String>,
    ) {
        self.enqueue_recovery_admissions(agent, admissions, false);
    }

    /// Re-queue an admission whose claim failed. It goes back to the head: the
    /// turn it belonged to gave up on it first, so replaying it before anything
    /// queued after it keeps the recovery order the store asked for. Reports
    /// whether the admission was newly queued — an already-queued one is left
    /// where it is rather than duplicated.
    pub(crate) fn queue_recovery_admission_first(
        &self,
        agent: &AgentId,
        admission: String,
    ) -> bool {
        self.enqueue_recovery_admissions(agent, [admission], true)
    }

    fn enqueue_recovery_admissions(
        &self,
        agent: &AgentId,
        admissions: impl IntoIterator<Item = String>,
        front: bool,
    ) -> bool {
        let mut queued_any = false;
        let mut queue = self
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for admission in admissions {
            if queue.iter().any(|(queued_agent, queued_admission)| {
                queued_agent == agent && queued_admission == &admission
            }) {
                continue;
            }
            if queue.len() >= MAX_RECOVERY_ADMISSIONS {
                log::warn!(
                    "recovery admission queue reached capacity {MAX_RECOVERY_ADMISSIONS} for session {}",
                    self.id
                );
                if front {
                    queue.pop_back();
                } else {
                    break;
                }
            }
            if front {
                queue.push_front((agent.clone(), admission));
            } else {
                queue.push_back((agent.clone(), admission));
            }
            queued_any = true;
        }
        queued_any
    }

    pub(crate) fn take_recovery_admission(&self, agent: &AgentId) -> Option<String> {
        let mut queue = self
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = queue
            .iter()
            .position(|(queued_agent, _)| queued_agent == agent)?;
        queue.remove(index).map(|(_, admission)| admission)
    }

    pub(crate) fn take_all_recovery_admissions(&self) -> Vec<(AgentId, String)> {
        let mut queue = self
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.drain(..).collect()
    }

    pub(crate) fn reassign_recovery_agent(&self, from: &AgentId, to: &AgentId) {
        let mut queue = self
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (agent, _) in &mut *queue {
            if agent == from {
                *agent = to.clone();
            }
        }
    }

    pub(crate) fn take_recovery_admissions(&self, agent: &AgentId) -> Vec<String> {
        let mut queue = self
            .recovery_admissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut admissions = Vec::new();
        queue.retain(|(queued_agent, admission)| {
            if queued_agent == agent {
                admissions.push(admission.clone());
                false
            } else {
                true
            }
        });
        admissions
    }

    pub(crate) async fn mark_admissions_terminal(
        &self,
        admissions: impl IntoIterator<Item = String>,
        terminal_stop: &str,
    ) {
        for admission in admissions {
            let _ = self
                .store
                .mark_message_admission_terminal(&self.id, &admission, Some(terminal_stop))
                .await;
        }
    }

    async fn cancel_admissions(&self, terminal_stop: &str) {
        let admissions = match self
            .store
            .load_message_admissions_for_session(&self.id)
            .await
        {
            Ok(admissions) => admissions,
            Err(error) => {
                log::warn!("failed to load admissions for cancel fence: {error}");
                return;
            }
        };
        let ids = admissions
            .into_iter()
            .filter(|admission| admission.state != pacode_store::MessageAdmissionState::Terminal)
            .map(|admission| {
                if let Some(agent) = self.agent_for_host(&admission.agent) {
                    agent.cancel_admission(&admission.message_id);
                }
                admission.message_id
            })
            .collect::<Vec<_>>();
        self.mark_admissions_terminal(ids, terminal_stop).await;
    }

    pub(crate) fn alias_agent(&self, id: AgentId, agent: Arc<Agent>) {
        self.agent_aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, agent);
    }

    pub fn agent_infos(&self) -> Vec<AgentInfo> {
        self.agents
            .read()
            .map(|a| a.values().map(|agent| agent.info()).collect())
            .unwrap_or_default()
    }

    pub fn meta(&self) -> SessionMeta {
        self.meta
            .read()
            .map(|m| m.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    pub fn tasks_of_session(&self) -> Vec<TaskInfo> {
        self.tasks.list(Some(&self.id))
    }

    /// Any live subagent, running turn, or running task.
    pub fn is_busy(&self) -> bool {
        let has_running_agent = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .any(|agent| agent.is_running() || agent.status().is_active())
            })
            .unwrap_or(false);
        if has_running_agent {
            return true;
        }
        if self.scheduler.jobs().iter().any(|job| job.enabled)
            || self
                .scheduler
                .monitors()
                .iter()
                .any(|monitor| monitor.status.is_live())
        {
            return true;
        }
        self.tasks_of_session()
            .iter()
            .any(|t| t.status == pacode_types::TaskStatus::Running)
    }

    pub fn has_live_work(&self) -> bool {
        if self.is_busy() {
            return true;
        }
        !self.permissions.pending().is_empty() || !self.questions.pending().is_empty()
    }

    /// The slot-badge status this session would show right now:
    /// `NeedsInput` while a permission or question is waiting, `Working` while
    /// any agent holds a turn, `Done` once everything went quiet. `Idle` is
    /// never produced here — it is the client's resting state, not a
    /// transition the daemon reports.
    fn session_status_kind(&self) -> pacode_types::SessionStatusKind {
        use pacode_types::SessionStatusKind as Kind;
        if !self.permissions.pending().is_empty() || !self.questions.pending().is_empty() {
            return Kind::NeedsInput;
        }
        let any_running = self
            .agents
            .read()
            .map(|agents| {
                agents
                    .get(&AgentId::main())
                    .is_some_and(|agent| agent.is_running())
                    || agents
                        .iter()
                        .filter(|(id, _)| !id.is_main())
                        .any(|(_, agent)| agent.is_running())
            })
            .unwrap_or(false);
        if any_running {
            Kind::Working
        } else {
            Kind::Done
        }
    }

    /// Publish the current slot-badge status on the daemon-wide stream so
    /// clients attached to another session see the transition. Called at every
    /// seam that can flip it: turn start/end, permission/question ask and
    /// resolve. Recomputing from live state (rather than trusting the caller's
    /// intent) keeps a resolve correct while other asks stay pending.
    pub fn emit_session_status(&self) {
        self.global_events.emit(pacode_types::Event::SessionStatus {
            session: self.id.clone(),
            status: self.session_status_kind(),
        });
        self.idle_watch.notify(self);
    }

    /// Start a turn on the main agent, or queue a steer injection when a turn is
    /// running. Persists the user message. Returns immediately.
    pub async fn submit_user_message(self: &Arc<Self>, text: String) -> Result<(), CoreError> {
        self.submit_user_message_inner(text, Vec::new(), None)
            .await
            .map(|_| ())
    }

    /// [`submit_user_message`](Self::submit_user_message) plus image attachments:
    /// each `ImageSource` is appended to the persisted user message as a
    /// `ContentBlock::Image` so providers can encode it (or degrade it to a
    /// placeholder when the model lacks vision).
    pub async fn submit_user_message_with_images(
        self: &Arc<Self>,
        text: String,
        images: Vec<pacode_types::ImageSource>,
    ) -> Result<(), CoreError> {
        self.submit_user_message_inner(text, images, None)
            .await
            .map(|_| ())
    }

    pub(crate) async fn submit_user_message_with_images_idempotent(
        self: &Arc<Self>,
        text: String,
        images: Vec<pacode_types::ImageSource>,
        message_id: String,
        fingerprint: u64,
    ) -> Result<(), CoreError> {
        self.submit_user_message_inner(text, images, Some((message_id, fingerprint)))
            .await
            .map(|_| ())
    }

    async fn submit_user_message_inner(
        self: &Arc<Self>,
        text: String,
        images: Vec<pacode_types::ImageSource>,
        admission: Option<(String, u64)>,
    ) -> Result<(), CoreError> {
        let _submit_guard = self.submit_lock.lock().await;
        let admission = admission.or_else(|| {
            Some((
                format!("internal-{}", TurnId::generate()),
                crate::core::user_message_fingerprint(&text, &images),
            ))
        });
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        let main = self
            .main_agent()
            .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;
        if main.stop_requested() {
            return Err(CoreError::Invalid("session is closed".into()));
        }

        if let Some((message_id, fingerprint)) = admission.as_ref()
            && let Some(existing) = self
                .store
                .get_message_admission(&self.id, message_id)
                .await?
        {
            if existing.fingerprint != *fingerprint {
                return Err(CoreError::Invalid(format!(
                    "message_id {message_id:?} was already used with different content"
                )));
            }
            if existing.state != pacode_store::MessageAdmissionState::Terminal
                || existing.message_id.starts_with("acp-")
            {
                let owner = self
                    .agent_for_host(&existing.agent)
                    .ok_or_else(|| CoreError::AgentNotFound(existing.agent.clone()))?;
                self.redrive_admission(&owner, &existing).await?;
            }
            return Ok(());
        }

        // Hard isolation: while ZDR is active the chat endpoint must be a
        // `zdr_capable` route. Normally the enable flow picks one first; this
        // guards resumes and flag removals.
        let meta = self.meta();
        if self.zdr_active() && !self.model_flags(&meta.model.to_string()).zdr_capable {
            return Err(CoreError::Invalid(format!(
                "zdr_blocked: current model {} is not flagged zdr_capable; pick a ZDR model (Alt+Z picker) or disable ZDR",
                meta.model
            )));
        }
        drop(meta);

        let cwd = self.meta().cwd;
        let expanded_text = crate::expand::expand_user_message(
            &text,
            &cwd,
            self.config.context.tool_output_cap_chars,
        );

        let mut content = vec![pacode_types::ContentBlock::Text {
            text: expanded_text,
        }];
        content.extend(
            images
                .into_iter()
                .map(|source| pacode_types::ContentBlock::Image { source }),
        );
        let message = pacode_types::Message::new(pacode_types::Role::User, content);
        if let Some(max_seq) = self.store.max_message_seq(&self.id, &main.id()).await? {
            let mut history = main.history.lock().unwrap_or_else(|p| p.into_inner());
            history.next_seq = history.next_seq.max(max_seq.saturating_add(1));
        }
        let reservation = match admission.as_ref() {
            Some((message_id, _)) => {
                main.reserve_user_message_with_admission(message, message_id.clone())
            }
            None => main.reserve_user_message(message),
        };
        let admission_result = match admission.as_ref() {
            None => self
                .store
                .append_message(
                    &self.id,
                    &main.id(),
                    reservation.seq(),
                    reservation.message(),
                )
                .await
                .map(|_| pacode_store::MessageAdmissionResult::Appended),
            Some((message_id, fingerprint)) => {
                let turn_id = format!("trn_{message_id}");
                self.store
                    .append_user_message_with_turn(
                        &self.id,
                        &main.id(),
                        reservation.seq(),
                        message_id,
                        *fingerprint,
                        Some(turn_id.as_str()),
                        reservation.message(),
                    )
                    .await
            }
        };
        match admission_result {
            Ok(pacode_store::MessageAdmissionResult::Appended) => {}
            Ok(pacode_store::MessageAdmissionResult::Duplicate) => {
                main.rollback_user_message(&reservation);
                if let Some(message_id) = admission.as_ref().map(|(message_id, _)| message_id)
                    && let Some(existing) = self
                        .store
                        .get_message_admission(&self.id, message_id)
                        .await?
                {
                    let owner = self
                        .agent_for_host(&existing.agent)
                        .ok_or_else(|| CoreError::AgentNotFound(existing.agent.clone()))?;
                    self.redrive_admission(&owner, &existing).await?;
                }
                return Ok(());
            }
            Ok(pacode_store::MessageAdmissionResult::Conflict) => {
                main.rollback_user_message(&reservation);
                let message_id = admission
                    .as_ref()
                    .map(|(message_id, _)| message_id.as_str())
                    .unwrap_or_default();
                return Err(CoreError::Invalid(format!(
                    "message_id {message_id:?} was already used with different content"
                )));
            }
            Err(error) => {
                main.rollback_user_message(&reservation);
                return Err(error.into());
            }
        }
        reservation.persist();

        // `history_before_turn` is the detach baseline: history as it stood
        // before the user message that started the next turn. Snapshot it
        // whenever none is pending — not only when the agent is idle: a submit
        // landing mid-turn (steer) or in the post-block gap still precedes
        // the turn that will consume this message, and only here is the
        // pre-message history still known.
        {
            let mut hbt = main
                .history_before_turn
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if hbt.is_none() {
                *hbt = Some(
                    main.history
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone(),
                );
            }
        }

        let item_seq = main
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = pacode_types::TranscriptItem {
            seq: item_seq,
            agent: main.id(),
            ts_ms: pacode_types::now_ms(),
            kind: pacode_types::TranscriptKind::User { text: text.clone() },
        };
        main.transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(pacode_types::Event::ItemAdded(item));

        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            if meta.first_prompt.is_none() {
                meta.first_prompt = Some(text.clone());
            }
        }
        self.touch();

        let _ = self
            .plugins
            .run_hooks(&pacode_plugin::HookEvent::OnMessage {
                role: "user".to_string(),
                text: text.clone(),
            })
            .await;

        crate::turn::lifecycle::start_turn_or_steer(
            self.clone(),
            main,
            admission
                .as_ref()
                .map(|(message_id, _)| message_id.as_str()),
        )
        .await?;

        Ok(())
    }

    async fn redrive_admission(
        self: &Arc<Self>,
        main: &Arc<Agent>,
        admission: &pacode_store::MessageAdmission,
    ) -> Result<(), CoreError> {
        if let Some(seq) = admission.message_seq {
            let restored = {
                let history = main.history.lock().unwrap_or_else(|p| p.into_inner());
                history.message_seqs.contains(&seq)
            };
            if !restored && let Some(message) = self.load_message_row(&admission.agent, seq).await?
            {
                main.history
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .restore_row(seq, message);
            }
        }
        let acp_admission = admission.message_id.starts_with("acp-");
        if let Some(current) = main.admission_turn(&admission.message_id) {
            if acp_admission {
                self.emit_admission_boundary(
                    main,
                    &admission.message_id,
                    Some(current.turn),
                    current.stop,
                );
            }
            return Ok(());
        }
        if acp_admission
            && admission.state == pacode_store::MessageAdmissionState::Running
            && main.is_running()
        {
            self.emit_admission_boundary(
                main,
                &admission.message_id,
                admission.turn_id.clone(),
                None,
            );
            return Ok(());
        }
        if acp_admission && admission.state == pacode_store::MessageAdmissionState::Terminal {
            let stop = match admission.terminal_stop.as_deref() {
                Some("interrupted") => pacode_types::TurnStop::Interrupted,
                Some("failed") => pacode_types::TurnStop::Failed {
                    message: "admitted turn failed".into(),
                },
                _ => pacode_types::TurnStop::Completed,
            };
            self.emit_admission_boundary(
                main,
                &admission.message_id,
                admission.turn_id.clone(),
                Some(stop),
            );
            return Ok(());
        }
        if admission.state == pacode_store::MessageAdmissionState::Running && main.is_running() {
            return Ok(());
        }
        if let Some(seq) = admission.message_seq
            && self.recovery_response_complete(main, seq).await?
        {
            self.store
                .mark_message_admission_terminal(&self.id, &admission.message_id, Some("recovered"))
                .await?;
            if acp_admission {
                self.emit_admission_boundary(
                    main,
                    &admission.message_id,
                    admission.turn_id.clone(),
                    Some(pacode_types::TurnStop::Completed),
                );
            }
            return Ok(());
        }
        crate::turn::lifecycle::start_turn_or_steer(
            self.clone(),
            main.clone(),
            Some(admission.message_id.as_str()),
        )
        .await
    }

    fn emit_admission_boundary(
        &self,
        main: &Arc<Agent>,
        message_id: &str,
        turn_id: Option<TurnId>,
        stop: Option<pacode_types::TurnStop>,
    ) {
        let turn = turn_id.unwrap_or_else(|| TurnId::new(format!("trn_{message_id}")));
        self.events.emit(pacode_types::Event::TurnStarted {
            agent: main.id(),
            turn: turn.clone(),
        });
        if let Some(stop) = stop {
            self.events.emit(pacode_types::Event::TurnEnded {
                agent: main.id(),
                turn,
                usage: None,
                stop,
            });
        }
    }

    fn recovery_response_state(main: &Agent, message_seq: u64) -> RecoveryResponseState {
        let rows = main
            .history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rows();
        let mut unanswered = HashSet::new();
        let mut saw_assistant = false;
        for (seq, message) in rows {
            if seq <= message_seq {
                continue;
            }
            match message.role {
                pacode_types::Role::Assistant => {
                    saw_assistant = true;
                    let tool_uses = message
                        .tool_uses()
                        .map(|(call_id, _, _)| call_id.clone())
                        .collect::<Vec<_>>();
                    if tool_uses.is_empty() {
                        return if unanswered.is_empty() {
                            RecoveryResponseState::Complete
                        } else {
                            RecoveryResponseState::Incomplete {
                                dangling_tools: true,
                            }
                        };
                    }
                    unanswered.extend(tool_uses);
                }
                pacode_types::Role::Tool => {
                    for block in &message.content {
                        if let pacode_types::ContentBlock::ToolResult { call_id, .. } = block {
                            unanswered.remove(call_id);
                        }
                    }
                }
                pacode_types::Role::User | pacode_types::Role::System => {}
            }
        }
        if !saw_assistant {
            RecoveryResponseState::Missing
        } else {
            RecoveryResponseState::Incomplete {
                dangling_tools: !unanswered.is_empty(),
            }
        }
    }

    async fn recovery_response_complete(
        self: &Arc<Self>,
        agent: &Arc<Agent>,
        message_seq: u64,
    ) -> Result<bool, CoreError> {
        match Self::recovery_response_state(agent, message_seq) {
            RecoveryResponseState::Complete => Ok(true),
            RecoveryResponseState::Incomplete {
                dangling_tools: true,
            } => {
                crate::turn::tools::stub_dangling_tool_calls(self, agent).await;
                Ok(false)
            }
            RecoveryResponseState::Missing | RecoveryResponseState::Incomplete { .. } => Ok(false),
        }
    }

    pub(crate) async fn recover_message_admissions(self: &Arc<Self>) -> Result<(), CoreError> {
        let mut grouped: HashMap<AgentId, Vec<pacode_store::MessageAdmission>> = HashMap::new();
        for admission in self
            .store
            .load_message_admissions_for_session(&self.id)
            .await?
        {
            if admission.state == pacode_store::MessageAdmissionState::Terminal {
                if let Some(agent) = self.agent_for_host(&admission.agent)
                    && let Some(turn) = admission.turn_id.clone()
                {
                    let stop = match admission.terminal_stop.as_deref() {
                        Some("interrupted") => pacode_types::TurnStop::Interrupted,
                        Some("failed") => pacode_types::TurnStop::Failed {
                            message: "admitted turn failed".into(),
                        },
                        _ => pacode_types::TurnStop::Completed,
                    };
                    agent.seed_terminal_turn(&turn, &stop);
                }
                continue;
            }
            grouped
                .entry(admission.agent.clone())
                .or_default()
                .push(admission);
        }
        for (agent_id, mut admissions) in grouped {
            let Some(agent) = self.agent_for_host(&agent_id) else {
                continue;
            };
            if agent.stop_requested() {
                continue;
            }
            admissions.sort_by_key(|admission| admission.message_seq.unwrap_or(u64::MAX));
            let mut unresolved = Vec::with_capacity(admissions.len());
            for admission in admissions {
                if agent.admission_turn(&admission.message_id).is_some() {
                    continue;
                }
                if let Some(seq) = admission.message_seq {
                    if let Some(message) = self.load_message_row(&agent_id, seq).await? {
                        agent
                            .history
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .restore_row(seq, message);
                    }
                    if !agent.is_running() && self.recovery_response_complete(&agent, seq).await? {
                        self.store
                            .mark_message_admission_terminal(
                                &self.id,
                                &admission.message_id,
                                Some("recovered"),
                            )
                            .await?;
                        continue;
                    }
                }
                unresolved.push(admission);
            }
            if agent.is_running() {
                self.queue_recovery_admissions(
                    &agent_id,
                    unresolved.into_iter().map(|admission| admission.message_id),
                );
                continue;
            }
            if let Some(admission) = unresolved.first() {
                self.queue_recovery_admissions(
                    &agent_id,
                    unresolved
                        .iter()
                        .skip(1)
                        .map(|admission| admission.message_id.clone()),
                );
                crate::turn::lifecycle::start_turn_or_steer(
                    self.clone(),
                    agent.clone(),
                    Some(admission.message_id.as_str()),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Cancel the main agent's running turn (history kept).
    pub fn interrupt(&self) {
        // Token first: a parked permission waiter must see the cancellation
        // (its biased cancel arm then beats the dropped-sender arm, so the
        // deny reason lands as "interrupted", not "aborted"). Whatever it had
        // not reached yet is cleaned up below.
        if let Some(main) = self.main_agent()
            && let Some(cancel) = main
                .cancel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
        {
            cancel.cancel();
        }
        // A question the turn is blocked on has to go with the turn, or the tool
        // call keeps waiting for an answer nobody will give now.
        self.cancel_questions_for(&AgentId::main());
        // Same for a pending permission: without this the entry stays in the
        // pending map and its transcript cell never resolves, which can shadow
        // a later live request in clients.
        self.cancel_permissions_for(&AgentId::main());
    }

    /// Dismiss every question an agent is waiting on, telling clients to drop
    /// their pickers.
    pub fn cancel_questions_for(&self, agent: &AgentId) {
        for question in self.questions.cancel_for_agent(agent) {
            let answer = pacode_types::QuestionAnswer::cancelled();
            self.record_question_answer(&question, &answer);
            self.events.emit(pacode_types::Event::QuestionResolved {
                question: question.id,
                answer,
            });
        }
        self.emit_session_status();
    }

    /// Deny every permission `agent` is still waiting on (interrupt/stop_agent).
    /// Dropping the pending entry makes the waiting call resolve to `Deny`, and
    /// each request gets the same `PermissionResolved` + transcript notice an
    /// answered one would — no stale Permission cell left shadowing clients.
    pub fn cancel_permissions_for(&self, agent: &AgentId) {
        for req in self.permissions.cancel_for_agent(agent) {
            let seq = self.agent_for_host(&req.agent).and_then(|a| {
                a.transcript
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .find_permission_seq(&req.id)
            });
            self.emit_permission_resolution(
                &req,
                seq,
                PermissionDecision::Deny,
                Some("interrupted".to_string()),
            );
        }
    }

    /// Emit `PermissionResolved` and swap the transcript `Permission` cell for
    /// the outcome notice at `item_seq` (`None` skips the cell — e.g. the tail
    /// rolled over). Shared by `SessionHost::request_permission` after its wait
    /// and by `cancel_permissions_for`, so a cancelled request lands exactly
    /// like an answered one.
    pub fn emit_permission_resolution(
        &self,
        req: &PermissionRequest,
        item_seq: Option<u64>,
        decision: PermissionDecision,
        reason: Option<String>,
    ) {
        self.events.emit(pacode_types::Event::PermissionResolved {
            permission: req.id.clone(),
            decision,
            reason: reason.clone(),
        });
        self.emit_session_status();
        let Some(seq) = item_seq else {
            return;
        };
        let (level, text) = match decision {
            PermissionDecision::AllowOnce => (
                pacode_types::ToastLevel::Info,
                "Permission allowed".to_string(),
            ),
            PermissionDecision::AllowSession => (
                pacode_types::ToastLevel::Info,
                "Permission allowed for session".to_string(),
            ),
            PermissionDecision::Deny => (
                pacode_types::ToastLevel::Warn,
                match &reason {
                    Some(reason) => format!("Permission denied: {reason}"),
                    None => "Permission denied".to_string(),
                },
            ),
        };
        let notice_item = pacode_types::TranscriptItem {
            seq,
            agent: req.agent.clone(),
            ts_ms: pacode_types::time::now_ms(),
            kind: pacode_types::TranscriptKind::Notice { level, text },
        };
        if let Some(agent) = self.agent_for_host(&req.agent) {
            agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .upsert(notice_item.clone());
        }
        self.events
            .emit(pacode_types::Event::ItemUpdated(notice_item));
    }

    pub fn resolve_permission(
        &self,
        id: &PermissionId,
        decision: PermissionDecision,
        reason: Option<String>,
    ) -> bool {
        self.permissions.resolve(id, decision, reason)
    }

    /// Switch the session model. The route must resolve against this session's
    /// provider registry — the turn loop resolves the same way, so an unknown
    /// provider is rejected here instead of failing the next turn.
    ///
    /// The route propagates to the main agent's `AgentInfo` and to every live
    /// subagent still on the old session model: those inherited it via
    /// `spec.model = None`, while subagents spawned with an explicit model keep
    /// theirs. Each updated info is persisted and broadcast, so the next loop
    /// iteration of a running turn sees the new route too.
    pub async fn set_model(&self, route: ModelRoute) -> Result<(), CoreError> {
        if self.zdr_active() && !self.model_flags(&route.to_string()).zdr_capable {
            return Err(CoreError::Invalid(format!(
                "zdr_blocked: {route} is not flagged zdr_capable; mark it in the model picker or disable ZDR"
            )));
        }
        self.providers.resolve(&route).await?;

        let old_model = {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut meta.model, route.clone())
        };

        self.update_agent_infos(|info| {
            if info.id.is_main() || (info.status.is_live() && info.model == old_model) {
                info.model = route.clone();
                true
            } else {
                false
            }
        })
        .await;

        self.touch();
        Ok(())
    }

    /// Same propagation as `set_model`, for the reasoning effort.
    pub async fn set_effort(&self, effort: Effort) {
        let old_effort = {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            std::mem::replace(&mut meta.effort, effort)
        };

        self.update_agent_infos(|info| {
            if info.id.is_main() || (info.status.is_live() && info.effort == old_effort) {
                info.effort = effort;
                true
            } else {
                false
            }
        })
        .await;

        self.touch();
    }

    pub fn set_mode(&self, mode: Mode) {
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.mode = mode;
        }
        self.touch();
    }

    /// ZDR is active when the config forces it (`always`) or the session toggle
    /// is on under `zdr = "session"`. While active, network tools, MCP calls
    /// and subagent spawns are denied and model switching is restricted.
    pub fn zdr_active(&self) -> bool {
        match self.config.zdr {
            ZdrMode::Always => true,
            ZdrMode::Session => self.meta().zdr,
            ZdrMode::Off => false,
        }
    }

    /// Session ZDR toggle; persists through `meta` (touch upserts the row and
    /// emits `SessionUpdated`, which the TUI snapshot carries).
    ///
    /// Enabling requires the current route to be flagged `zdr_capable` —
    /// otherwise the submit guard would just reject the next prompt anyway,
    /// so the refusal happens here with an actionable error.
    pub fn set_zdr(&self, zdr: bool) -> Result<(), CoreError> {
        if zdr {
            let route = self.meta().model;
            if !self.model_flags(&route.to_string()).zdr_capable {
                return Err(CoreError::Invalid(format!(
                    "zdr_blocked: {route} is not flagged zdr_capable; mark it in the model picker first"
                )));
            }
        }
        {
            let mut meta = self.meta.write().unwrap_or_else(|p| p.into_inner());
            meta.zdr = zdr;
        }
        self.touch();
        Ok(())
    }

    /// Session-wide notice lock: while it is on, every resident agent parks the
    /// injections it would otherwise deliver. The held deques live per agent
    /// under this one switch — a per-agent lock would need an `AgentId` in the
    /// request and give the user a control they have no use for.
    ///
    /// Unlocking re-drives whatever is holding, and that re-drive is the only
    /// wake available: the toggle arrived over the wire with no push behind it,
    /// so without this the first release would wait for the next unrelated
    /// event. `true` when the switch actually moved.
    pub async fn set_notice_lock(self: &Arc<Self>, locked: bool) -> bool {
        let now = std::time::Instant::now();
        let agents: Vec<Arc<Agent>> = self
            .agents
            .read()
            .map(|agents| agents.values().cloned().collect())
            .unwrap_or_default();
        // `|=` on purpose: `any` would stop at the first agent that moved the
        // switch and leave the rest of the session locked.
        let mut changed = false;
        for agent in &agents {
            changed |= agent.notice_lock.set_locked(locked, now);
        }
        if !changed {
            return false;
        }
        let held: usize = agents.iter().map(|agent| agent.notice_lock.held()).sum();
        let dropped: u64 = agents.iter().map(|agent| agent.notice_lock.dropped()).sum();
        self.events.emit(pacode_types::Event::NoticeLockChanged {
            session: self.id.clone(),
            locked,
            held,
            dropped,
        });
        if !locked {
            for agent in &agents {
                if agent.notice_lock.held() == 0 || agent.stop_requested() {
                    continue;
                }
                // A stopped subagent is resumed, not re-driven: `request_turn`
                // refuses it, and `continue_agent` is what the user already uses
                // to bring it back.
                if !agent.id().is_main() && agent.status() == pacode_types::AgentStatus::Stopped {
                    let _ = self.continue_agent(&agent.id()).await;
                } else {
                    crate::turn::lifecycle::request_turn(self.clone(), agent.clone());
                }
            }
        }
        true
    }

    /// Flags for one `provider/model` key; absent entry means all-false.
    pub fn model_flags(&self, key: &str) -> ModelFlags {
        self.model_flags
            .read()
            .map(|f| f.get(key).copied().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Merge a flag update for one `provider/model` key.
    ///
    /// While ZDR is active, `zdr_capable` cannot be stripped from the
    /// currently routed model: the turn loop re-resolves that route on every
    /// iteration, so unmarking it mid-turn would wedge the session.
    pub fn set_model_flags(&self, key: String, flags: ModelFlags) -> Result<(), CoreError> {
        if self.zdr_active()
            && self.meta().model.to_string() == key
            && self.model_flags(&key).zdr_capable
            && !flags.zdr_capable
        {
            return Err(CoreError::Invalid(
                "zdr_blocked: cannot unmark the active route while ZDR is on".into(),
            ));
        }
        let mut map = self.model_flags.write().unwrap_or_else(|p| p.into_inner());
        if flags == ModelFlags::default() {
            map.remove(&key);
        } else {
            map.insert(key, flags);
        }
        Ok(())
    }

    /// Run `update` against each agent's `AgentInfo`; where it returns `true`,
    /// persist the row and emit `AgentUpdated`.
    async fn update_agent_infos(&self, update: impl Fn(&mut AgentInfo) -> bool) {
        let agents: Vec<Arc<Agent>> = self
            .agents
            .read()
            .map(|a| a.values().cloned().collect())
            .unwrap_or_default();
        for agent in agents {
            let updated_info = {
                let mut info = agent.info.write().unwrap_or_else(|p| p.into_inner());
                if update(&mut info) {
                    Some(info.clone())
                } else {
                    None
                }
            };
            let Some(info) = updated_info else {
                continue;
            };
            self.publish_agent(&agent, info).await;
        }
    }

    /// Persist `info` as `agent`'s row and broadcast it — unless the agent has
    /// left the session. Both happen under the agent's `message_write_lock`,
    /// which `/clear` holds while it drops subagents and purges their rows, so
    /// a write either lands before the purge (and goes with it) or finds the
    /// agent gone. A write that slipped past would bring a cleared subagent
    /// back: its row on the next resume, its card in every rail.
    pub(crate) async fn publish_agent(&self, agent: &Arc<Agent>, info: AgentInfo) {
        let _guard = agent.message_write_lock.clone().lock_owned().await;
        let resident = self
            .agent_for_host(&agent.id())
            .is_some_and(|current| Arc::ptr_eq(&current, agent));
        if !resident {
            return;
        }
        if let Err(e) = self
            .store
            .upsert_agent(&self.id, &info, agent.prompt().as_deref())
            .await
        {
            log::warn!(
                "failed to persist agent {} in session {}: {e}",
                info.id,
                self.id
            );
        }
        self.events.emit(pacode_types::Event::AgentUpdated(info));
    }

    /// Spawn a subagent (depth 1, no concurrency cap) and start its turn.
    pub async fn spawn_agent(self: &Arc<Self>, spec: AgentSpec) -> Result<AgentId, CoreError> {
        let _submit_guard = self.submit_lock.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        let parent = self
            .main_agent()
            .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;
        if parent.stop_requested() {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        let parent_info = parent.info();
        let id = AgentId::generate();
        let name = spec.name.unwrap_or_else(|| {
            let count = self.agents.read().unwrap_or_else(|p| p.into_inner()).len();
            format!("agent-{count}")
        });

        // The default is parity: a subagent starts from the orchestrator's own
        // tool set, so nothing has to be restated to give it working tools.
        // `tools` narrows that to an include list and `exclude_tools` subtracts
        // from whichever applies — which is the cheap direction, because at 128
        // subagents every tool schema is re-sent on every step, and dropping the
        // handful a subagent cannot use costs a short list rather than a
        // near-complete one. Exclude wins where both name the same tool.
        let tools = {
            let g = self.tools.read().unwrap_or_else(|p| p.into_inner());
            let mut names: Vec<String> = match &spec.tools {
                Some(allowed) => allowed
                    .iter()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect(),
                None => pacode_tools::subagent_tool_names(&g),
            };
            if let Some(dropped) = &spec.exclude_tools {
                let dropped: std::collections::HashSet<&str> = dropped
                    .iter()
                    .map(|n| n.trim())
                    .filter(|n| !n.is_empty())
                    .collect();
                names.retain(|n| !dropped.contains(n.as_str()));
            }
            // Depth 1 is structural, not a preference, so it is enforced after
            // both lists have been applied: no combination of arguments can
            // hand a subagent the means to spawn one.
            let names: Vec<&str> = names
                .iter()
                .map(String::as_str)
                .filter(|n| *n != pacode_tools::builtin::agent::NAME)
                .collect();
            g.subset(names)
        };

        let model = spec.model.unwrap_or_else(|| parent_info.model.clone());
        let effort = spec.effort.unwrap_or(parent_info.effort);

        let mut history = crate::agent::History::default();
        if spec.fork {
            let parent_hist = parent.history.lock().unwrap_or_else(|p| p.into_inner());
            history.messages = parent_hist.messages.clone();
            history.message_seqs = parent_hist.message_seqs.clone();
            history.free_seqs = parent_hist.free_seqs.clone();
            history.summary = parent_hist.summary.clone();
            history.summary_upto = parent_hist.summary_upto;
            history.next_seq = parent_hist.next_seq;
            history.estimated_tokens = parent_hist.estimated_tokens;
        }
        history.push(pacode_types::Message::user(&spec.prompt));
        let persisted_history = history.rows();

        let now = pacode_types::now_ms();
        let info = AgentInfo {
            id: id.clone(),
            name,
            kind: pacode_types::AgentKind::Sub,
            status: pacode_types::AgentStatus::Idle,
            activity: None,
            started_at_ms: now,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model,
            effort,
            parent: Some(parent_info.id),
            summary: None,
            error: None,
        };

        // `session_open`: the store write and the in-memory registration — the
        // stage whose cost grows with what the session already holds, and so the
        // one that would show a fleet degrading as it fills.
        let open_started = std::time::Instant::now();
        let persisted = self
            .store
            .create_agent_with_history(
                &self.id,
                &info,
                Some(spec.prompt.as_str()),
                &persisted_history,
            )
            .await;
        pacode_types::record_launch_session_open(open_started.elapsed());
        persisted?;

        if self.closed.load(Ordering::SeqCst) || parent.stop_requested() {
            let mut stopped = info;
            stopped.status = pacode_types::AgentStatus::Stopped;
            stopped.finished_at_ms = Some(pacode_types::now_ms());
            let _ = self
                .store
                .upsert_agent(&self.id, &stopped, Some(spec.prompt.as_str()))
                .await;
            return Err(CoreError::Invalid("session is closed".into()));
        }

        let agent = Arc::new(Agent::new(
            id.clone(),
            info.clone(),
            history,
            tools,
            self.config.session.history_page,
            Some(spec.prompt),
        ));

        let mut agents = self.agents.write().unwrap_or_else(|p| p.into_inner());
        if self.closed.load(Ordering::SeqCst) || parent.stop_requested() {
            drop(agents);
            agent.request_stop();
            agent.set_status(pacode_types::AgentStatus::Stopped, None);
            let stopped = agent.info();
            let _ = self
                .store
                .upsert_agent(&self.id, &stopped, agent.prompt().as_deref())
                .await;
            return Err(CoreError::Invalid("session is closed".into()));
        }
        agents.insert(id.clone(), agent.clone());
        // Starts the clock the turn loop reads for time-to-first-token. Set
        // right after registration — as late as possible while still preceding
        // the provider call.
        agent.mark_spawned_at(std::time::Instant::now());
        drop(agents);

        self.events.emit(pacode_types::Event::AgentAdded(info));
        crate::turn::lifecycle::start_turn(self.clone(), agent);

        Ok(id)
    }

    /// Cancel the main agent's turn and stop every live subagent
    /// (`Request::InterruptAll`). The main agent keeps `Interrupt` semantics
    /// (lands `Idle`); subagents land `Stopped` with their history retained.
    pub async fn interrupt_all(&self) {
        let main = self.main_agent();
        if let Some(main) = &main {
            main.invalidate_turn_generation();
            let _ = main.cancel_admission_turn();
        }
        self.interrupt();
        let main_turn = match &main {
            Some(main) => Some(main.turn_lock.clone().lock_owned().await),
            None => None,
        };
        let _submit_guard = self.submit_lock.lock().await;
        if let Some(main) = &main {
            self.sweep_main_admissions(main).await;
        }
        self.cancel_admissions("interrupted").await;
        let sub_ids: Vec<AgentId> = self
            .agents
            .read()
            .map(|a| {
                a.values()
                    .filter(|agent| !agent.id().is_main() && agent.status().is_live())
                    .map(|agent| agent.id())
                    .collect()
            })
            .unwrap_or_default();
        for id in sub_ids {
            if let Err(e) = self.stop_agent(&id).await {
                log::warn!(
                    "interrupt_all: failed to stop agent {id} in {}: {e}",
                    self.id
                );
            }
        }
        let recovery = self.take_all_recovery_admissions();
        let mut recovery_ids = Vec::with_capacity(recovery.len());
        for (agent_id, admission) in recovery {
            if let Some(agent) = self.agent_for_host(&agent_id) {
                agent.cancel_admission(&admission);
            }
            recovery_ids.push(admission);
        }
        self.mark_admissions_terminal(recovery_ids, "interrupted")
            .await;
        if let (Some(main), Some(turn)) = (&main, main_turn) {
            let _turn = turn;
            self.sweep_main_admissions(main).await;
        }
        self.cancel_admissions("interrupted").await;
    }

    /// Drop everything the main agent was still holding for a user message:
    /// the admission it is answering, messages persisted but not yet folded in,
    /// queued injections and the recovery backlog.
    pub(crate) async fn sweep_main_admissions(&self, main: &Arc<Agent>) {
        let _ = main.cancel_admission_turn();
        let pending = main.discard_pending_user_messages();
        for admission in &pending {
            main.cancel_admission(admission);
        }
        self.mark_admissions_terminal(pending, "interrupted").await;
        main.discard_injections();
        let recovery = self.take_recovery_admissions(&main.id());
        for admission in &recovery {
            main.cancel_admission(admission);
        }
        self.mark_admissions_terminal(recovery, "interrupted").await;
    }

    /// Re-drive a `Stopped` agent: append the orchestrator's resume nudge to
    /// its retained history and start a fresh turn on a new cancel token — the
    /// same path `spawn_agent` takes. Errors while the agent is still running
    /// (let the stop land first) or already in a terminal state.
    pub async fn continue_agent(self: &Arc<Self>, id: &AgentId) -> Result<(), CoreError> {
        let _submit_guard = self.submit_lock.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        let agent = self
            .agent(id)
            .ok_or_else(|| CoreError::AgentNotFound(id.clone()))?;
        if agent.is_running() {
            return Err(CoreError::Invalid(format!("agent {id} is still running")));
        }
        if agent.status() != pacode_types::AgentStatus::Stopped {
            return Err(CoreError::Invalid(format!(
                "agent {id} is not stopped (status: {:?})",
                agent.status()
            )));
        }

        let generation = agent.turn_generation();
        let message_guard = agent.message_write_lock.clone().lock_owned().await;
        let current = self
            .agent(id)
            .ok_or_else(|| CoreError::AgentNotFound(id.clone()))?;
        if !Arc::ptr_eq(&current, &agent)
            || agent.turn_generation() != generation
            || agent.status() != pacode_types::AgentStatus::Stopped
            || self.closed.load(Ordering::SeqCst)
        {
            return Err(CoreError::Invalid(format!(
                "agent {id} changed before resume"
            )));
        }

        agent.commit_pending_user_messages();
        let (seq, arc_msg) = agent
            .history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(pacode_types::Message::user(
                "[resumed by orchestrator: continue your task]",
            ));
        self.store
            .append_message(&self.id, &agent.id(), seq, &arc_msg)
            .await?;
        let current = self
            .agent(id)
            .ok_or_else(|| CoreError::AgentNotFound(id.clone()))?;
        if !Arc::ptr_eq(&current, &agent)
            || agent.turn_generation() != generation
            || self.closed.load(Ordering::SeqCst)
        {
            return Err(CoreError::Invalid(format!(
                "agent {id} changed during resume"
            )));
        }

        let item = pacode_types::TranscriptItem {
            seq: agent
                .transcript
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .next_seq(),
            agent: id.clone(),
            ts_ms: pacode_types::now_ms(),
            kind: pacode_types::TranscriptKind::Notice {
                level: pacode_types::ToastLevel::Info,
                text: "resumed by orchestrator".to_string(),
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        self.events.emit(pacode_types::Event::ItemAdded(item));

        agent.clear_stop_request();
        agent.set_status(pacode_types::AgentStatus::Idle, None);
        if agent.turn_generation() != generation || self.closed.load(Ordering::SeqCst) {
            agent.request_stop();
            agent.set_status(pacode_types::AgentStatus::Stopped, None);
            return Err(CoreError::Invalid(format!(
                "agent {id} changed before restart"
            )));
        }
        self.events
            .emit(pacode_types::Event::AgentUpdated(agent.info()));
        crate::turn::lifecycle::start_turn(self.clone(), agent);
        drop(message_guard);
        Ok(())
    }

    pub async fn stop_agent(&self, id: &AgentId) -> Result<(), CoreError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| CoreError::AgentNotFound(id.clone()))?;
        let expected_id = agent.id();
        agent.invalidate_turn_generation();
        let _message_guard = agent.message_write_lock.clone().lock_owned().await;
        let current = self.agent(&expected_id);
        if !current.is_some_and(|current| Arc::ptr_eq(&current, &agent)) {
            agent.request_stop();
            return Err(CoreError::AgentNotFound(expected_id));
        }
        agent.request_stop();
        let mut admissions = agent.discard_pending_user_messages();
        if let Some(admission) = agent.cancel_admission_turn() {
            admissions.push(admission);
        }
        for admission in &admissions {
            agent.cancel_admission(admission);
        }
        self.mark_admissions_terminal(admissions, "interrupted")
            .await;
        agent.discard_injections();
        agent.set_status(pacode_types::AgentStatus::Stopped, None);
        if let Some(cancel) = agent
            .cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
        self.cancel_questions_for(&expected_id);
        self.cancel_permissions_for(&expected_id);
        for admission in self.take_recovery_admissions(&expected_id) {
            let _ = self
                .store
                .mark_message_admission_terminal(&self.id, &admission, Some("interrupted"))
                .await;
        }
        let info = agent.info();
        let _ = self
            .store
            .upsert_agent(&self.id, &info, agent.prompt().as_deref())
            .await;
        self.events.emit(pacode_types::Event::AgentUpdated(info));
        Ok(())
    }

    fn prepare_detach(&self, main: &Arc<Agent>) -> DetachPlan {
        let history_before = main
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let prior_history = history_before.unwrap_or_else(|| {
            let history = main.history.lock().unwrap_or_else(|p| p.into_inner());
            let mut rows = history.rows();
            rows.pop();
            let estimated_tokens = rows
                .iter()
                .map(|(_, message)| message.estimate_tokens())
                .sum::<u32>()
                + history
                    .summary
                    .as_deref()
                    .map(pacode_types::estimate_tokens)
                    .unwrap_or(0);
            let next_seq = rows
                .last()
                .map(|(seq, _)| seq.saturating_add(1))
                .unwrap_or_else(|| history.summary_upto.saturating_add(1));
            crate::agent::History {
                message_seqs: rows.iter().map(|(seq, _)| *seq).collect(),
                free_seqs: Default::default(),
                messages: rows.into_iter().map(|(_, message)| message).collect(),
                next_seq,
                summary: history.summary.clone(),
                summary_upto: history.summary_upto,
                estimated_tokens,
            }
        });
        let turn_prompt = {
            let history = main.history.lock().unwrap_or_else(|p| p.into_inner());
            history
                .messages
                .iter()
                .rev()
                .find(|message| message.role == pacode_types::Role::User)
                .map(|message| message.text())
        };
        let new_id = AgentId::generate();
        let name = {
            let count = self.agents.read().map(|agents| agents.len()).unwrap_or(1);
            format!("agent-{count}")
        };
        let mut detached_info = main.info();
        detached_info.id = new_id.clone();
        detached_info.name = name;
        detached_info.kind = pacode_types::AgentKind::Sub;
        detached_info.parent = Some(AgentId::main());

        let detached_tools = {
            let tools = self.tools.read().unwrap_or_else(|p| p.into_inner());
            let names = pacode_tools::subagent_tool_names(&tools);
            tools.subset(names.iter().map(String::as_str))
        };
        let main_tools = self.tools.read().unwrap_or_else(|p| p.into_inner()).clone();
        let fresh_main_info = pacode_types::AgentInfo {
            id: AgentId::main(),
            name: "main".to_string(),
            kind: pacode_types::AgentKind::Main,
            status: pacode_types::AgentStatus::Idle,
            activity: None,
            started_at_ms: pacode_types::now_ms(),
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: detached_info.model.clone(),
            effort: detached_info.effort,
            parent: None,
            summary: None,
            error: None,
        };
        let mut fresh_transcript =
            crate::transcript::TranscriptState::new(self.config.session.history_page as usize * 2);
        let prior_tuples = prior_history.rows();
        // Same band discipline as resume: the tail numbers upward from above
        // the store's reserved range, and the floor says where the store takes
        // over. Detach keeps every message in `prior_history`, so nothing is
        // older than its first row and there is no store page to serve.
        fresh_transcript.reserve_history_band();
        for entry in crate::transcript::history_to_items(&AgentId::main(), &prior_tuples) {
            let mut item = entry.item;
            item.seq = fresh_transcript.next_seq();
            fresh_transcript.upsert(item);
        }

        DetachPlan {
            new_id,
            detached_info,
            detached_tools,
            fresh_main: Arc::new({
                let agent = Agent::new_with_transcript(
                    AgentId::main(),
                    fresh_main_info,
                    prior_history,
                    main_tools,
                    fresh_transcript,
                    None,
                );
                agent.seed_history_floor(None);
                agent
            }),
            turn_prompt,
        }
    }

    pub(crate) async fn append_agent_message(
        &self,
        agent: &Agent,
        seq: u64,
        message: &pacode_types::Message,
    ) -> Result<(), CoreError> {
        let _guard = agent.message_write_lock.clone().lock_owned().await;
        self.store
            .append_message(&self.id, &agent.id(), seq, message)
            .await?;
        Ok(())
    }

    /// One persisted message row, addressed by its seq. A keyset page of one
    /// instead of the agent's whole history: admission recovery needs single
    /// rows, and a long session's history is tens of thousands of messages.
    pub(crate) async fn load_message_row(
        &self,
        agent: &AgentId,
        seq: u64,
    ) -> Result<Option<Arc<pacode_types::Message>>, CoreError> {
        let page = self
            .store
            .load_messages_after(&self.id, agent, seq.checked_sub(1), 1)
            .await?;
        Ok(page
            .rows
            .into_iter()
            .find(|row| row.seq == seq)
            .map(|row| Arc::new(row.message)))
    }

    async fn persist_detached_messages(&self, main: &Agent) -> Result<(), CoreError> {
        for (seq, message) in detached_message_rows(main) {
            self.append_agent_message(main, seq, &message).await?;
        }
        Ok(())
    }

    fn commit_detach(
        &self,
        main: &Arc<Agent>,
        plan: DetachPlan,
    ) -> Result<(AgentId, AgentInfo), CoreError> {
        if self.closed.load(Ordering::SeqCst) || main.stop_requested() {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        if !main.is_running() {
            return Err(CoreError::Invalid(
                "main turn ended before detach commit".to_string(),
            ));
        }
        let mut agents = self.agents.write().unwrap_or_else(|p| p.into_inner());
        let current = agents.get(&AgentId::main()).cloned();
        if !current.is_some_and(|current| Arc::ptr_eq(&current, main)) || !main.id().is_main() {
            return Err(CoreError::Invalid(
                "main agent changed before detach commit".to_string(),
            ));
        }

        let DetachPlan {
            new_id,
            detached_info,
            detached_tools,
            fresh_main,
            turn_prompt,
        } = plan;
        let old_id = main.id();
        self.reassign_recovery_agent(&old_id, &new_id);
        self.alias_agent(new_id.clone(), main.clone());
        self.permissions.reassign_agent(&old_id, &new_id);
        self.questions.reassign_agent(&old_id, &new_id);
        self.tasks.reassign_owner(&self.id, &old_id, &new_id);
        let mut transcript = main.transcript.lock().unwrap_or_else(|p| p.into_inner());
        main.set_id(new_id.clone());
        main.set_prompt(turn_prompt);
        *main.tools.write().unwrap_or_else(|p| p.into_inner()) = detached_tools;
        *main.info.write().unwrap_or_else(|p| p.into_inner()) = detached_info.clone();
        for item in &mut transcript.tail {
            item.agent = new_id.clone();
        }
        for item in transcript.tool_items.values_mut() {
            item.agent = new_id.clone();
        }
        drop(transcript);
        *main
            .history_before_turn
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        agents.insert(new_id.clone(), main.clone());
        agents.insert(AgentId::main(), fresh_main);
        Ok((new_id, detached_info))
    }

    /// Detach a running turn on the main agent into a background subagent.
    pub async fn detach_main_turn(self: &Arc<Self>) -> Result<AgentId, CoreError> {
        log::info!(
            "detaching running turn on main agent in session {}",
            self.id
        );
        let _submit_guard = self.submit_lock.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Invalid("session is closed".into()));
        }
        let main = self
            .main_agent()
            .ok_or_else(|| CoreError::AgentNotFound(AgentId::main()))?;

        if !main.is_running() {
            log::warn!("failed to detach main turn: no turn is running on the main agent");
            return Err(CoreError::Invalid(
                "no turn is running on the main agent".to_string(),
            ));
        }

        let plan = self.prepare_detach(&main);
        let old_id = main.id();
        let detached_id = plan.new_id.clone();
        let detached_rows = detached_message_rows(&main);
        let (retained_seqs, summary_upto) = {
            let history = plan
                .fresh_main
                .history
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                history.message_seqs.clone(),
                history.summary.as_ref().map(|_| history.summary_upto),
            )
        };
        let _message_guard = main.message_write_lock.clone().lock_owned().await;
        self.store
            .detach_agent(
                &self.id,
                &plan.detached_info,
                plan.turn_prompt.as_deref(),
                &detached_rows,
                &retained_seqs,
                summary_upto,
                &old_id,
                &detached_id,
            )
            .await?;

        let (new_id, detached_info) = match self.commit_detach(&main, plan) {
            Ok(result) => result,
            Err(error) => {
                let _ = self
                    .store
                    .rollback_detach(&self.id, &detached_id, &detached_rows, &old_id)
                    .await;
                return Err(error);
            }
        };
        drop(_message_guard);
        if let Err(error) = self.persist_detached_messages(&main).await {
            log::warn!(
                "failed to reconcile detached messages session={} agent={new_id}: {error}",
                self.id
            );
        }

        self.events
            .emit(pacode_types::Event::AgentAdded(detached_info));
        if let Some(fresh_main) = self.main_agent() {
            self.events
                .emit(pacode_types::Event::AgentUpdated(fresh_main.info()));
        }

        log::info!(
            "detached running turn on main agent into background subagent {new_id} in session {}",
            self.id
        );

        Ok(new_id)
    }

    pub fn set_plan(&self, plan: Plan) {
        if let Ok(mut p) = self.plan.write() {
            *p = plan.clone();
        }
        let store = self.store.clone();
        let session_id = self.id.clone();
        let plan_clone = plan.clone();
        tokio::spawn(async move {
            let _ = store.save_plan(&session_id, &plan_clone).await;
        });
        self.events.emit(pacode_types::Event::PlanUpdated(plan));
    }

    /// Everything a client needs to render from scratch (main agent transcript tail
    /// of `session.history_page` items).
    pub fn snapshot(&self) -> SessionSnapshot {
        optimistic_snapshot(&self.events, |seq| {
            let main = self.main_agent();
            let (transcript, has_more_history) = main
                .as_ref()
                .map(|m| m.transcript_tail(None, self.config.session.history_page as usize))
                .unwrap_or_default();
            let active_turn = main.as_ref().and_then(|agent| agent.current_turn());
            let terminal_turn = main.as_ref().and_then(|agent| agent.terminal_turn());
            let terminal_error = main.as_ref().and_then(|agent| agent.terminal_error());
            let turn_active = active_turn.is_some();
            SessionSnapshot {
                pending_questions: self.questions.pending(),
                cron_jobs: self.scheduler.jobs(),
                monitors: self.scheduler.monitors(),
                meta: self.meta(),
                agents: self.agent_infos(),
                plan: self.plan.read().unwrap_or_else(|p| p.into_inner()).clone(),
                tasks: self.tasks_of_session(),
                usage: self.usage.read().unwrap_or_else(|p| p.into_inner()).clone(),
                transcript,
                has_more_history,
                pending_permissions: self.permissions.pending(),
                turn_active,
                active_turn,
                terminal_turn,
                terminal_error,
                seq,
            }
        })
    }

    /// Recompute `UsageTotals` after a turn and emit `UsageUpdated`.
    pub async fn record_usage(
        &self,
        agent: &AgentId,
        turn_id: &TurnId,
        turn: u32,
        usage: pacode_types::Usage,
        context_tokens: u32,
    ) -> Result<(), CoreError> {
        let model_name = self
            .agent_for_host(agent)
            .map(|agent| agent.info().model.model)
            .unwrap_or_else(|| self.meta().model.model);
        let pricing = self.config.pricing_for(&model_name);
        let cost_usd = pricing.map(|pricing| pricing.cost_usd(&usage));
        if !self
            .store
            .add_usage_for_turn(
                &self.id,
                agent,
                turn,
                Some(turn_id.as_str()),
                &usage,
                cost_usd,
            )
            .await?
        {
            return Ok(());
        }

        let totals = {
            let mut u = self.usage.write().unwrap_or_else(|p| p.into_inner());
            u.input = u.input.saturating_add(usage.input_tokens);
            u.output = u.output.saturating_add(usage.output_tokens);
            u.reasoning = u.reasoning.saturating_add(usage.reasoning_tokens);
            u.cache_read = u.cache_read.saturating_add(usage.cache_read_tokens);
            u.cache_write = u.cache_write.saturating_add(usage.cache_write_tokens);
            u.turns = u.turns.saturating_add(1);
            u.context_tokens = context_tokens;
            u.last_activity_ms = pacode_types::now_ms();
            if let Some(cost_usd) = cost_usd {
                u.cost_usd = Some(u.cost_usd.unwrap_or(0.0) + cost_usd);
            }
            u.clone()
        };

        if let Some(a) = self.agent_for_host(agent) {
            if let Ok(mut info) = a.info.write() {
                info.tokens_in = info.tokens_in.saturating_add(usage.input_tokens);
                info.tokens_out = info.tokens_out.saturating_add(usage.output_tokens);
            }
            let info = a.info();
            self.events
                .emit(pacode_types::Event::AgentUpdated(info.clone()));
            self.store
                .upsert_agent(&self.id, &info, a.prompt().as_deref())
                .await?;
        }

        self.events.emit(pacode_types::Event::UsageUpdated(totals));
        Ok(())
    }

    /// Persist meta (`updated_at` bumped) and emit `SessionUpdated`.
    pub fn touch(&self) {
        let meta = {
            let mut m = self.meta.write().unwrap_or_else(|p| p.into_inner());
            m.updated_at_ms = pacode_types::now_ms();
            m.clone()
        };
        let store = self.store.clone();
        let m_clone = meta.clone();
        tokio::spawn(async move {
            let _ = store.upsert_session(&m_clone).await;
        });
        self.events.emit(pacode_types::Event::SessionUpdated(meta));
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
