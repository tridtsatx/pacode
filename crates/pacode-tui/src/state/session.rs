use std::collections::{HashSet, VecDeque};
use std::io::{self, Write};
use std::sync::{Arc, atomic::AtomicU64};
use std::time::Instant;

use pacode_client::ClientEventContext;
use pacode_render::RenderCacheKey;
use pacode_types::ids::ClientId;
use pacode_types::{AgentId, AgentStatus, Attach, Event, Request, SessionId, SessionMeta, TurnId};

use super::{
    FilesState, Focus, GitState, InputState, PanelTarget, PendingCopy, RailState, Selection,
};
use crate::clipboard_read::PastedImages;
use crate::state::activity::Phase;
use crate::state::images::ImageStripState;
use crate::state::questions::PendingQuestions;
use crate::state::transcript::Transcript;
use crate::state::vim::VimState;

const RETRY_QUEUE_CAP: usize = 16;
const DEFERRED_SEQUENCE_CAP: usize = 16;
const SESSION_EVENT_BUFFER_CAP: usize = 1024;
const SESSION_EVENT_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnEndMatch {
    Current,
    Duplicate,
    Stale,
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContext {
    pub generation: u64,
    pub deferred_epoch: u64,
    pub slot: usize,
    pub session: Option<SessionId>,
}

impl SessionContext {
    pub fn matches(
        &self,
        generation: u64,
        deferred_epoch: u64,
        slot: usize,
        session: Option<&SessionId>,
    ) -> bool {
        self.generation == generation
            && self.deferred_epoch == deferred_epoch
            && self.slot == slot
            && self.session.as_ref() == session
    }

    /// Whether the context still describes the generation that issued it. The
    /// deferred epoch is deliberately not part of this: a reconnect bumps it to
    /// strand the work in flight, and the retries that work leaves behind
    /// belong to the same session generation.
    pub fn issued_by(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiContext {
    pub generation: u64,
    pub deferred_epoch: u64,
    pub slot: usize,
}

#[derive(Clone, Debug)]
pub struct PendingAttach {
    pub context: SessionContext,
    pub attach: Attach,
    pub token: u64,
    pub client_session_epoch: Arc<AtomicU64>,
    pub retry_requested: bool,
    pub retry_count: u8,
}

impl PendingAttach {
    pub fn client_session_epoch(&self) -> u64 {
        self.client_session_epoch
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_client_session_epoch(&self, epoch: u64) {
        self.client_session_epoch
            .store(epoch, std::sync::atomic::Ordering::Release);
    }

    pub fn epoch_signal(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.client_session_epoch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BufferedEventKey {
    connection_epoch: u64,
    session_epoch: u64,
    session: Option<SessionId>,
    seq: u64,
}

impl BufferedEventKey {
    fn new(context: &ClientEventContext, seq: u64) -> Self {
        Self {
            connection_epoch: context.connection_epoch,
            session_epoch: context.session_epoch,
            session: context.session.clone(),
            seq,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BufferedSessionEvent {
    pub(crate) context: ClientEventContext,
    pub(crate) first_seq: u64,
    pub(crate) seq: u64,
    pub(crate) event: Option<Event>,
    pub(crate) bytes: usize,
}

#[derive(Default)]
struct EventByteCounter {
    bytes: usize,
}

impl Write for EventByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn buffered_event_bytes(context: &ClientEventContext, event: Option<&Event>) -> usize {
    let mut counter = EventByteCounter::default();
    if let Some(event) = event
        && serde_json::to_writer(&mut counter, event).is_err()
    {
        return usize::MAX;
    }
    counter
        .bytes
        .saturating_add(std::mem::size_of::<BufferedSessionEvent>())
        .saturating_add(std::mem::size_of::<ClientEventContext>())
        .saturating_add(context.session.as_ref().map_or(0, |id| id.as_str().len()))
}

fn coalesce_buffered_event(existing: &Event, incoming: Event) -> Option<Event> {
    match (existing, incoming) {
        (
            Event::TextDelta {
                agent,
                item_seq,
                text: old_text,
            },
            Event::TextDelta {
                agent: new_agent,
                item_seq: new_item_seq,
                text: new_text,
            },
        ) if agent == &new_agent && *item_seq == new_item_seq => {
            let mut text = old_text.clone();
            text.push_str(&new_text);
            Some(Event::TextDelta {
                agent: new_agent,
                item_seq: new_item_seq,
                text,
            })
        }
        (
            Event::ReasoningDelta {
                agent,
                item_seq,
                text: old_text,
            },
            Event::ReasoningDelta {
                agent: new_agent,
                item_seq: new_item_seq,
                text: new_text,
            },
        ) if agent == &new_agent && *item_seq == new_item_seq => {
            let mut text = old_text.clone();
            text.push_str(&new_text);
            Some(Event::ReasoningDelta {
                agent: new_agent,
                item_seq: new_item_seq,
                text,
            })
        }
        (Event::ItemAdded(old) | Event::ItemUpdated(old), Event::ItemUpdated(item))
            if old.agent == item.agent && old.seq == item.seq =>
        {
            Some(Event::ItemUpdated(item))
        }
        (Event::AgentAdded(old) | Event::AgentUpdated(old), Event::AgentUpdated(info))
            if old.id == info.id =>
        {
            Some(Event::AgentUpdated(info))
        }
        (Event::TaskAdded(old) | Event::TaskUpdated(old), Event::TaskUpdated(info))
            if old.id == info.id =>
        {
            Some(Event::TaskUpdated(info))
        }
        (Event::PlanUpdated(_), Event::PlanUpdated(plan)) => Some(Event::PlanUpdated(plan)),
        (Event::UsageUpdated(_), Event::UsageUpdated(usage)) => Some(Event::UsageUpdated(usage)),
        (Event::SessionUpdated(_), Event::SessionUpdated(meta)) => {
            Some(Event::SessionUpdated(meta))
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptBatch {
    pub requests: Vec<Request>,
}

impl PromptBatch {
    pub fn single(request: Request) -> Self {
        Self {
            requests: vec![request],
        }
    }

    pub fn advance(&mut self, expected: &Request) -> bool {
        if self.requests.first() != Some(expected) {
            return false;
        }
        self.requests.remove(0);
        true
    }

    pub fn without_detach_turn(mut self) -> Self {
        if matches!(self.requests.first(), Some(Request::DetachTurn)) {
            self.requests.remove(0);
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct UserMessageInFlight {
    pub id: u64,
    pub context: SessionContext,
    pub request: Request,
    pub sequence: PromptBatch,
    turn_generation: u64,
}

pub struct SessionUiState {
    pub generation: u64,
    pub deferred_epoch: u64,
    pub meta: Option<SessionMeta>,
    pub turn_active: bool,
    active_turn: Option<TurnId>,
    terminal_turn: Option<TurnId>,
    terminal_turn_unknown: bool,
    turn_generation: u64,
    pub user_msgs_in_flight: usize,
    pub turn_boundary_pending: bool,
    turn_cycle_observed: bool,
    pub transcript: Transcript,
    pub panel: PanelState,
    pub files: FilesState,
    pub git: GitState,
    pub rail: RailState,
    pub input: InputState,
    pub vim: VimState,
    pub focus: Focus,
    pub questions: PendingQuestions,
    pub running_bash: Option<tokio::sync::oneshot::Sender<()>>,
    running_bash_id: Option<u64>,
    next_bash_run_id: u64,
    pub pasted_images: PastedImages,
    pub image_strip: ImageStripState,
    pub selection: Selection,
    pub pending_chord: Option<crossterm::event::KeyEvent>,
    pub turn_started_at: Option<Instant>,
    pub anim_frame: u64,
    /// The animation clock a sweep is drawn at: elapsed time and the period
    /// actually in force. Both halves matter — a sweep positioned off elapsed
    /// time alone would move one cell per *nominal* tick and jump `multiplier`
    /// cells on a degraded cadence, which is the stutter the degraded cadence
    /// exists to prevent.
    pub anim: pacode_render::AnimClock,
    /// The last `/analyze` reading. Held on the state rather than inside the
    /// overlay so a scroll does not re-ask the daemon, and so a client that
    /// reconnects while the page is open keeps what it was reading.
    pub analyze_report: Option<pacode_types::analyze::AnalyzeReport>,
    pub phase: Option<(Phase, Instant)>,
    pub phase_elapsed_ms: u64,
    pub started_at: Instant,
    pub second_parity: bool,
    pub last_focus_key: u8,
    pub overlay_since: Option<Instant>,
    pub pending_copy: Option<PendingCopy>,
    pub pending_requests: VecDeque<Request>,
    pub last_event_seq: Option<u64>,
    pub resync_want: bool,
    pub resync_from_prompt: bool,
    pub resync_in_flight: bool,
    pub resync_sent_at: Option<Instant>,
    pub in_flight_user_messages: VecDeque<UserMessageInFlight>,
    pub pending_sequences: VecDeque<PromptBatch>,
    pub retry_requests: VecDeque<PromptBatch>,
    buffered_events: VecDeque<BufferedSessionEvent>,
    buffered_event_keys: HashSet<BufferedEventKey>,
    buffered_event_bytes: usize,
    buffered_events_overflowed: bool,
    next_user_message_id: u64,
    next_attach_token: u64,
}

impl SessionUiState {
    pub fn new(transcript_cells: usize) -> Self {
        Self {
            generation: 0,
            deferred_epoch: 0,
            meta: None,
            turn_active: false,
            active_turn: None,
            terminal_turn: None,
            terminal_turn_unknown: false,
            turn_generation: 0,
            user_msgs_in_flight: 0,

            turn_boundary_pending: false,
            turn_cycle_observed: false,
            transcript: Transcript::new(transcript_cells),
            panel: PanelState::new(transcript_cells),
            files: FilesState::default(),
            git: GitState::default(),
            rail: RailState::default(),
            input: InputState::default(),
            vim: VimState::default(),
            focus: Focus::Normal,
            questions: PendingQuestions::default(),
            running_bash: None,
            running_bash_id: None,
            next_bash_run_id: 0,
            pasted_images: PastedImages::new(),
            image_strip: ImageStripState::default(),
            selection: Selection::default(),
            pending_chord: None,
            turn_started_at: None,
            anim_frame: 0,
            anim: pacode_render::AnimClock::default(),
            analyze_report: None,
            phase: None,
            phase_elapsed_ms: 0,
            started_at: Instant::now(),
            second_parity: false,
            last_focus_key: 0,
            overlay_since: None,
            pending_copy: None,
            pending_requests: VecDeque::new(),
            last_event_seq: None,
            resync_want: false,
            resync_from_prompt: false,
            resync_in_flight: false,
            resync_sent_at: None,
            in_flight_user_messages: VecDeque::new(),
            pending_sequences: VecDeque::new(),
            retry_requests: VecDeque::new(),
            buffered_events: VecDeque::new(),
            buffered_event_keys: HashSet::new(),
            buffered_event_bytes: 0,
            buffered_events_overflowed: false,

            next_user_message_id: 0,

            next_attach_token: 0,
        }
    }

    pub fn reset(&mut self, transcript_cells: usize) {
        let generation = self.generation.wrapping_add(1);
        let generation = if generation == 0 { 1 } else { generation };
        let deferred_epoch = self.deferred_epoch;
        let turn_generation = self.turn_generation;
        let next_user_message_id = self.next_user_message_id;
        let next_attach_token = self.next_attach_token;
        let next_bash_run_id = self.next_bash_run_id;
        // The composer is the user's, not the session's: the draft, prompt and
        // `!` histories follow them to the next session, and so do the pasted
        // images the draft points at (dropping the store would also delete
        // files already submitted). What is tied to the session leaves: the
        // queue (`leave_active_slot` stashes it per session), history
        // browsing, and the cwd-keyed completion caches.
        let mut input = std::mem::take(&mut self.input);
        input.prompt_queue.clear();
        input.history_index = None;
        input.bash_history_index = None;
        input.at_comp = Default::default();
        input.bash_comp = Default::default();
        let pasted_images = std::mem::take(&mut self.pasted_images);
        *self = Self::new(transcript_cells);
        self.input = input;
        self.pasted_images = pasted_images;
        self.generation = generation;
        self.deferred_epoch = deferred_epoch;
        self.turn_generation = turn_generation;
        self.next_user_message_id = next_user_message_id;
        self.next_attach_token = next_attach_token;
        self.next_bash_run_id = next_bash_run_id;
    }

    pub fn start_bash_run(&mut self, cancel: tokio::sync::oneshot::Sender<()>) -> u64 {
        self.next_bash_run_id = self.next_bash_run_id.wrapping_add(1);
        let id = self.next_bash_run_id;
        self.running_bash = Some(cancel);
        self.running_bash_id = Some(id);
        id
    }

    pub fn finish_bash_run(&mut self, id: u64) -> bool {
        if self.running_bash_id != Some(id) {
            return false;
        }
        self.running_bash = None;
        self.running_bash_id = None;
        true
    }

    pub fn invalidate_bash_run(&mut self) {
        self.running_bash = None;
        self.running_bash_id = None;
    }

    pub fn begin_user_message(
        &mut self,
        context: SessionContext,
        request: Request,
    ) -> UserMessageInFlight {
        let request = Self::with_client_message_id(request);
        self.begin_user_message_ticket(UserMessageInFlight {
            id: 0,
            context,
            request: request.clone(),
            sequence: PromptBatch::single(request),
            turn_generation: self.turn_generation,
        })
    }

    pub fn begin_user_message_sequence(
        &mut self,
        context: SessionContext,
        sequence: PromptBatch,
    ) -> Option<UserMessageInFlight> {
        let mut requests = sequence.requests;
        let index = requests
            .iter()
            .position(|request| matches!(request, Request::UserMessage { .. }))?;
        let request = Self::with_client_message_id(requests[index].clone());
        requests[index] = request.clone();
        Some(self.begin_user_message_ticket(UserMessageInFlight {
            id: 0,
            context,
            request,
            sequence: PromptBatch { requests },
            turn_generation: self.turn_generation,
        }))
    }

    fn begin_user_message_ticket(
        &mut self,
        mut ticket: UserMessageInFlight,
    ) -> UserMessageInFlight {
        self.next_user_message_id = self.next_user_message_id.wrapping_add(1);
        ticket.id = self.next_user_message_id;
        self.turn_boundary_pending = true;
        self.turn_cycle_observed = false;
        self.in_flight_user_messages.push_back(ticket.clone());
        self.user_msgs_in_flight = self.in_flight_user_messages.len();
        ticket
    }

    fn with_client_message_id(request: Request) -> Request {
        match request {
            Request::UserMessage {
                text,
                images,
                message_id,
            } => Request::UserMessage {
                text,
                images,
                message_id: message_id.or_else(|| Some(format!("tui-{}", ClientId::generate()))),
            },
            request => request,
        }
    }

    pub fn complete_user_message_step(
        &mut self,
        id: u64,
        request: &Request,
    ) -> Option<PromptBatch> {
        let position = self
            .in_flight_user_messages
            .iter()
            .position(|ticket| ticket.id == id)?;
        let ticket = &self.in_flight_user_messages[position];
        if !ticket.context.issued_by(self.generation) {
            // A ticket from a session that is gone still holds the prompt gate,
            // so it leaves the queue rather than staying in it.
            self.in_flight_user_messages.remove(position);
            self.user_msgs_in_flight = self.in_flight_user_messages.len();
            return None;
        }
        let ticket = &mut self.in_flight_user_messages[position];
        ticket
            .sequence
            .advance(request)
            .then(|| ticket.sequence.clone())
    }

    pub fn user_message_ticket(&self, id: u64) -> Option<UserMessageInFlight> {
        self.in_flight_user_messages
            .iter()
            .find(|ticket| ticket.id == id)
            .cloned()
    }

    pub fn finish_user_message(&mut self, id: u64, failed: bool) -> Option<Request> {
        let pos = self
            .in_flight_user_messages
            .iter()
            .position(|ticket| ticket.id == id)?;
        let ticket = self.in_flight_user_messages.remove(pos)?;
        self.user_msgs_in_flight = self.in_flight_user_messages.len();
        if !ticket.context.issued_by(self.generation) {
            return None;
        }
        if failed {
            self.push_retry_batch(ticket.sequence.clone().without_detach_turn());
            if self.user_msgs_in_flight == 0 {
                self.turn_boundary_pending = false;
            }
        } else if self.user_msgs_in_flight == 0
            && self.turn_boundary_pending
            && !self.turn_active
            && ticket.turn_generation < self.turn_generation
            && self.turn_cycle_observed
        {
            self.turn_boundary_pending = false;
        } else if self.user_msgs_in_flight == 0
            && self.turn_boundary_pending
            && !self.turn_active
            && !self.turn_cycle_observed
        {
            self.resync_want = true;
            self.resync_from_prompt = true;
        }
        Some(ticket.request)
    }

    pub fn fail_in_flight_user_messages(&mut self) {
        let batches = std::mem::take(&mut self.in_flight_user_messages)
            .into_iter()
            .map(|ticket| ticket.sequence.without_detach_turn());
        for batch in batches {
            self.push_retry_batch(batch);
        }
        self.user_msgs_in_flight = 0;
        self.turn_boundary_pending = true;
        self.turn_cycle_observed = false;
    }

    fn advance_turn_generation(&mut self) {
        loop {
            self.turn_generation = self.turn_generation.wrapping_add(1);
            if self.turn_generation != 0 {
                return;
            }
        }
    }

    fn mark_turn_cycle_observed(&mut self) {
        self.advance_turn_generation();
        self.turn_cycle_observed = true;
        if self.resync_from_prompt {
            self.resync_from_prompt = false;
            self.resync_want = false;
        }
        if self.user_msgs_in_flight == 0 {
            self.turn_boundary_pending = false;
        }
    }

    pub fn observe_main_agent_status(&mut self, status: AgentStatus, now: Instant) {
        if status.is_active() {
            if self.active_turn.is_none() {
                self.observe_turn_started_for(None);
            } else {
                self.turn_active = true;
            }
            if self.turn_started_at.is_none() {
                self.turn_started_at = Some(now);
            }
        } else {
            if self.turn_active {
                if let Some(turn) = self.active_turn.clone() {
                    self.observe_turn_end(&turn);
                } else {
                    self.observe_turn_ended();
                }
            }
            self.turn_started_at = None;
        }
        let turn_active = self.turn_active;
        self.rail.update_idle(turn_active, now);
    }

    pub fn observe_turn_started(&mut self) {
        self.observe_turn_started_for(None);
    }

    pub fn observe_turn_started_for(&mut self, turn: Option<&TurnId>) -> bool {
        if turn.is_some_and(|turn| self.active_turn.as_ref() == Some(turn)) && self.turn_active {
            return false;
        }
        if turn.is_some_and(|turn| self.terminal_turn.as_ref() == Some(turn)) {
            return false;
        }
        if turn.is_none() && self.turn_active {
            return false;
        }
        self.active_turn = turn.cloned();
        self.terminal_turn_unknown = false;
        self.turn_active = true;
        self.advance_turn_generation();
        self.turn_cycle_observed = false;
        if self.resync_from_prompt {
            self.resync_from_prompt = false;
            self.resync_want = false;
        }
        true
    }

    pub fn observe_turn_ended(&mut self) {
        self.active_turn = None;
        self.terminal_turn = None;
        self.terminal_turn_unknown = true;
        self.turn_active = false;
        self.mark_turn_cycle_observed();
    }

    pub(crate) fn observe_turn_end(&mut self, turn: &TurnId) -> TurnEndMatch {
        let disposition = if self.active_turn.as_ref() == Some(turn)
            || self.active_turn.is_none() && self.turn_active
        {
            TurnEndMatch::Current
        } else if self.active_turn.is_none()
            && (self.terminal_turn.as_ref() == Some(turn)
                || self.terminal_turn_unknown && !self.turn_active)
        {
            TurnEndMatch::Duplicate
        } else {
            TurnEndMatch::Stale
        };
        if disposition == TurnEndMatch::Current {
            self.active_turn = None;
            self.terminal_turn = Some(turn.clone());
            self.terminal_turn_unknown = false;
            self.turn_active = false;
            self.mark_turn_cycle_observed();
        } else if disposition == TurnEndMatch::Duplicate {
            self.terminal_turn_unknown = false;
        }
        disposition
    }

    pub fn reconcile_turn_boundary(&mut self, turn_active: bool) {
        if turn_active {
            self.turn_active = true;
            self.terminal_turn_unknown = false;
            if self.active_turn.is_none() {
                self.advance_turn_generation();
                self.turn_cycle_observed = false;
            }
            return;
        }
        if self.active_turn.is_some() {
            self.observe_turn_ended();
        } else if self.user_msgs_in_flight == 0 {
            self.turn_boundary_pending = false;
            self.turn_cycle_observed = true;
        }
    }

    pub fn reconcile_turn_fence(
        &mut self,
        turn_active: bool,
        active_turn: Option<&TurnId>,
        terminal_turn: Option<&TurnId>,
    ) {
        if turn_active {
            self.turn_active = true;
            self.terminal_turn_unknown = false;
            if let Some(turn) = active_turn {
                if self.terminal_turn.as_ref() == Some(turn) {
                    self.turn_active = false;
                    return;
                }
                if self.active_turn.as_ref() != Some(turn) {
                    self.active_turn = Some(turn.clone());
                    self.advance_turn_generation();
                    self.turn_cycle_observed = false;
                }
            } else {
                self.reconcile_turn_boundary(true);
            }
            return;
        }
        self.turn_active = false;
        self.terminal_turn_unknown = false;
        if let Some(turn) = terminal_turn {
            if self.active_turn.as_ref() == Some(turn) {
                self.observe_turn_end(turn);
            } else if self.active_turn.is_none() && self.terminal_turn.as_ref() != Some(turn) {
                self.terminal_turn = Some(turn.clone());
                self.mark_turn_cycle_observed();
            }
        } else {
            self.reconcile_turn_boundary(false);
        }
    }

    pub fn advance_deferred_epoch(&mut self) {
        loop {
            self.deferred_epoch = self.deferred_epoch.wrapping_add(1);
            if self.deferred_epoch != 0 {
                return;
            }
        }
    }

    pub fn cancel_deferred_work(&mut self) {
        self.invalidate_bash_run();
        self.transcript.loading_history = false;
        self.input.at_comp.in_flight = None;
        self.input.bash_comp.in_flight = None;
        self.files.preview_in_flight = None;
        self.files.statused_at = None;
        self.files.statuses_in_flight = false;
        self.files.diff_in_flight = None;
        self.rail.task_tail_in_flight = None;
        self.image_strip.in_flight.clear();
        self.git.loading = false;
        self.git.loading_more = false;
        self.git.log_after = None;
        self.git.show_in_flight = None;
        self.resync_in_flight = false;
        self.resync_from_prompt = false;
    }

    fn discard_oldest_buffered_event(&mut self) {
        if let Some(buffered) = self.buffered_events.pop_front() {
            self.buffered_event_keys
                .remove(&BufferedEventKey::new(&buffered.context, buffered.seq));
            self.buffered_event_bytes = self.buffered_event_bytes.saturating_sub(buffered.bytes);
            self.buffered_events_overflowed = true;
        }
    }

    fn reserve_buffered_event(&mut self, bytes: usize) -> bool {
        if bytes > SESSION_EVENT_BUFFER_BYTES {
            self.buffered_events_overflowed = true;
            return false;
        }
        while self.buffered_events.len() >= SESSION_EVENT_BUFFER_CAP
            || self.buffered_event_bytes > SESSION_EVENT_BUFFER_BYTES.saturating_sub(bytes)
        {
            if self.buffered_events.is_empty() {
                self.buffered_events_overflowed = true;
                return false;
            }
            self.discard_oldest_buffered_event();
        }
        true
    }

    pub fn buffer_event(&mut self, context: ClientEventContext, seq: u64, event: Event) -> bool {
        let key = BufferedEventKey::new(&context, seq);
        if !self.buffered_event_keys.insert(key.clone()) {
            return false;
        }
        let bytes = buffered_event_bytes(&context, Some(&event));
        let coalesced = self.buffered_events.back().and_then(|buffered| {
            if buffered.context != context || buffered.seq != seq.wrapping_sub(1) {
                return None;
            }
            let existing = buffered.event.as_ref()?;
            coalesce_buffered_event(existing, event.clone()).map(|merged| {
                (
                    buffered.seq,
                    buffered.bytes,
                    merged.clone(),
                    buffered_event_bytes(&context, Some(&merged)),
                )
            })
        });
        if let Some((old_seq, old_bytes, merged, merged_bytes)) = coalesced
            && merged_bytes <= SESSION_EVENT_BUFFER_BYTES
        {
            let old_key = BufferedEventKey::new(&context, old_seq);
            self.buffered_event_keys.remove(&old_key);
            self.buffered_event_keys.insert(key);
            if let Some(buffered) = self.buffered_events.back_mut() {
                buffered.seq = seq;
                buffered.event = Some(merged);
                buffered.bytes = merged_bytes;
            }
            self.buffered_event_bytes = self
                .buffered_event_bytes
                .saturating_sub(old_bytes)
                .saturating_add(merged_bytes);
            while self.buffered_event_bytes > SESSION_EVENT_BUFFER_BYTES
                && self.buffered_events.len() > 1
            {
                self.discard_oldest_buffered_event();
            }
            return true;
        }
        if !self.reserve_buffered_event(bytes) {
            self.buffered_event_keys.remove(&key);
            return false;
        }
        self.buffered_event_bytes = self.buffered_event_bytes.saturating_add(bytes);
        self.buffered_events.push_back(BufferedSessionEvent {
            context,
            first_seq: seq,
            seq,
            event: Some(event),
            bytes,
        });
        true
    }

    pub(crate) fn buffer_filtered_event(&mut self, context: ClientEventContext, seq: u64) -> bool {
        let key = BufferedEventKey::new(&context, seq);
        if !self.buffered_event_keys.insert(key.clone()) {
            return false;
        }
        let old_filtered_seq = self
            .buffered_events
            .back()
            .filter(|buffered| {
                buffered.context == context
                    && buffered.seq == seq.wrapping_sub(1)
                    && buffered.event.is_none()
            })
            .map(|buffered| buffered.seq);
        if let Some(old_seq) = old_filtered_seq {
            if let Some(buffered) = self.buffered_events.back_mut() {
                buffered.seq = seq;
            }
            self.buffered_event_keys
                .remove(&BufferedEventKey::new(&context, old_seq));
            return true;
        }
        let bytes = buffered_event_bytes(&context, None);
        if !self.reserve_buffered_event(bytes) {
            self.buffered_event_keys.remove(&key);
            return false;
        }
        self.buffered_event_bytes = self.buffered_event_bytes.saturating_add(bytes);
        self.buffered_events.push_back(BufferedSessionEvent {
            context,
            first_seq: seq,
            seq,
            event: None,
            bytes,
        });
        true
    }

    pub(crate) fn take_buffered_events(&mut self) -> (Vec<BufferedSessionEvent>, bool) {
        let events = self.buffered_events.drain(..).collect();
        self.buffered_event_keys.clear();
        self.buffered_event_bytes = 0;
        (events, std::mem::take(&mut self.buffered_events_overflowed))
    }

    pub fn clear_buffered_events(&mut self) {
        self.buffered_events.clear();
        self.buffered_event_keys.clear();
        self.buffered_event_bytes = 0;
        self.buffered_events_overflowed = false;
    }

    pub fn buffered_event_count(&self) -> usize {
        self.buffered_events.len()
    }

    pub fn buffered_event_bytes(&self) -> usize {
        self.buffered_event_bytes
    }

    pub fn allocate_attach_token(&mut self) -> u64 {
        loop {
            self.next_attach_token = self.next_attach_token.wrapping_add(1);
            if self.next_attach_token != 0 {
                return self.next_attach_token;
            }
        }
    }

    fn push_retry_batch(&mut self, batch: PromptBatch) {
        if self.retry_requests.len() >= RETRY_QUEUE_CAP {
            self.retry_requests.pop_front();
        }
        self.retry_requests.push_back(batch);
    }

    pub fn take_retry_batch(&mut self) -> Option<PromptBatch> {
        if self.turn_active || self.user_msgs_in_flight > 0 || self.turn_boundary_pending {
            return None;
        }
        self.retry_requests.pop_front()
    }

    pub fn has_retry_request(&self) -> bool {
        !self.retry_requests.is_empty()
    }

    pub fn queue_sequence(&mut self, sequence: PromptBatch) -> bool {
        if self.pending_sequences.len() >= DEFERRED_SEQUENCE_CAP {
            return false;
        }
        self.pending_sequences.push_back(sequence);
        true
    }

    pub fn has_pending_sequence(&self) -> bool {
        !self.pending_sequences.is_empty()
    }

    pub fn drop_pending_detach_turns(&mut self) {
        for sequence in &mut self.pending_sequences {
            if matches!(sequence.requests.first(), Some(Request::DetachTurn)) {
                sequence.requests.remove(0);
            }
        }
    }

    pub fn take_pending_sequence(&mut self) -> Option<PromptBatch> {
        self.pending_sequences.pop_front()
    }
}

impl Default for SessionUiState {
    fn default() -> Self {
        Self::new(300)
    }
}

pub struct PanelState {
    pub target: Option<PanelTarget>,
    /// The agent whose items `agent_transcript` holds. The panel follows the
    /// focus, and focus moves on its own: a stream for one agent is written into
    /// this buffer while the header already names the next one, so the buffer
    /// carries the agent it belongs to and the paint path drops a mismatch
    /// rather than showing one agent's answer under another's name.
    agent_owner: Option<AgentId>,
    pub agent_transcript: Transcript,
    /// The target whose output `task_lines` holds — the same ownership question
    /// for the log-shaped panels (a task's spool output, an MCP server's log).
    lines_owner: Option<PanelTarget>,
    pub task_lines: Vec<String>,
    pub task_total_lines: u64,
    pub scroll: usize,
    pub diff: Option<PanelDiff>,
}

impl PanelState {
    pub fn new(transcript_cells: usize) -> Self {
        Self {
            target: None,
            agent_owner: None,
            agent_transcript: Transcript::new(transcript_cells),
            lines_owner: None,
            task_lines: Vec::new(),
            task_total_lines: 0,
            scroll: 0,
            diff: None,
        }
    }

    /// The agent transcript to paint for `agent`, emptied first when the panel
    /// has moved to another agent.
    pub fn agent_transcript_for(&mut self, agent: &AgentId) -> &mut Transcript {
        if self
            .agent_owner
            .as_ref()
            .is_some_and(|owner| owner != agent)
        {
            self.agent_transcript.reset(Vec::new(), false);
        }
        self.agent_owner = Some(agent.clone());
        &mut self.agent_transcript
    }

    /// The agent transcript, but only while it belongs to `agent`.
    pub fn owned_agent_transcript(&self, agent: &AgentId) -> Option<&Transcript> {
        (self.agent_owner.as_ref() == Some(agent)).then_some(&self.agent_transcript)
    }

    /// The spool/log lines to paint for `target`, emptied first when the panel
    /// has moved to another target.
    pub fn lines_for(&mut self, target: &PanelTarget) -> &[String] {
        if self
            .lines_owner
            .as_ref()
            .is_some_and(|owner| owner != target)
        {
            self.task_lines.clear();
            self.task_total_lines = 0;
        }
        self.lines_owner = Some(target.clone());
        &self.task_lines
    }

    /// The spool/log lines, but only while they belong to `target`.
    pub fn owned_lines(&self, target: &PanelTarget) -> Option<&[String]> {
        (self.lines_owner.as_ref() == Some(target)).then_some(&self.task_lines[..])
    }
}

impl Default for PanelState {
    fn default() -> Self {
        Self::new(300)
    }
}

pub struct PanelDiff {
    pub path: String,
    pub patch: Option<String>,
    pub status: Option<crate::state::files::FileStatus>,
    pub truncated: bool,
    pub note: Option<String>,
    pub rendered: Option<(RenderCacheKey, Vec<ratatui::text::Line<'static>>)>,
}
