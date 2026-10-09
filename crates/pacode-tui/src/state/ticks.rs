//! What the event loop's clocks ask of state: the activity phase and its
//! timing, the transient animation windows, and whether each tick (stream,
//! 1 s, animation) has visible work.

use std::time::Instant;

use pacode_types::{ToolStatus, TranscriptKind};

use super::{AppState, CellKind, Connection, Focus, PanelTarget};

/// Overlay/picker unfold duration (the centred grow-in, spec §animation).
pub const OVERLAY_GROW_MS: u64 = 140;
/// Welcome cascade window after attach: the header animates only inside it.
pub const WELCOME_ANIM_MS: u64 = 400;

/// Animation frames in one blink of [`AppState::pulse_lit`], lit for half of
/// them. Tuned for the default `tick_ms` of 66 ms, which puts the blink at
/// ~1 s — the same beat as the `second_parity` fallback, so a pulse does not
/// change speed when the animation clock takes over from the 1 s tick.
pub const PULSE_FRAMES: u64 = 16;

impl AppState {
    /// The phase the session is in right now, from what the transcript and the rail
    /// show. Pure: it reads state only, never the clock.
    pub fn current_phase(&self) -> Option<crate::state::activity::Phase> {
        use crate::state::activity::Phase;

        if self.turn_active {
            // A tool that is executing wins: that is what the session is doing,
            // whatever the model streamed before starting it.
            if let Some(title) = self
                .transcript
                .cells
                .iter()
                .rev()
                .find_map(|c| match &c.kind {
                    CellKind::Item(TranscriptKind::ToolCall {
                        status: ToolStatus::Running,
                        title,
                        ..
                    }) => Some(title.clone()),
                    _ => None,
                })
            {
                return Some(Phase::Tool(title));
            }
            // Answer text already arriving (or still draining) is responding, not thinking.
            let responding = self.transcript.has_backlog()
                || self
                    .transcript
                    .cells
                    .iter()
                    .rev()
                    .find_map(|c| match &c.kind {
                        CellKind::Item(TranscriptKind::Assistant { complete, .. }) => {
                            Some(!complete)
                        }
                        CellKind::Item(TranscriptKind::Reasoning { .. }) => Some(false),
                        CellKind::Item(TranscriptKind::ToolCall { .. }) => Some(false),
                        _ => None,
                    })
                    == Some(true);
            return Some(if responding {
                Phase::Responding
            } else {
                Phase::Thinking
            });
        }

        // The main agent is idle — plain idle, whoever else is still running.
        // Live subagents and background tasks keep their rail entries and card
        // animations; they are just not the activity line's business.
        None
    }

    /// Recompute the phase and keep its start instant across redraws. Returns the
    /// phase with the milliseconds it has been running.
    pub fn tick_phase(&mut self, now: Instant) -> Option<(crate::state::activity::Phase, u64)> {
        let current = self.current_phase();
        let out = match (&current, &self.phase) {
            (Some(new), Some((old, since))) if new == old => {
                let elapsed = now.saturating_duration_since(*since).as_millis() as u64;
                Some((old.clone(), elapsed))
            }
            (Some(new), _) => {
                self.phase = Some((new.clone(), now));
                Some((new.clone(), 0))
            }
            (None, _) => {
                self.phase = None;
                None
            }
        };
        let elapsed = out.as_ref().map(|(_, ms)| *ms).unwrap_or(0);
        // A redraw between ticks must not change the reading, so only whole
        // seconds are kept; the sub-second remainder only paces the next tick.
        if crate::state::activity::displayed_secs(elapsed)
            != crate::state::activity::displayed_secs(self.phase_elapsed_ms)
        {
            self.dirty = true;
        }
        self.phase_elapsed_ms = elapsed;
        out
    }

    /// Whether the welcome cascade is still playing (bounded: first 400 ms
    /// after `started_at`).
    pub fn welcome_playing(&self, now: Instant) -> bool {
        (now.saturating_duration_since(self.started_at).as_millis() as u64) < WELCOME_ANIM_MS
    }

    /// Whether the current overlay/picker is inside its unfold window.
    pub fn overlay_growing(&self, now: Instant) -> bool {
        self.overlay_since.is_some_and(|t| {
            (now.saturating_duration_since(t).as_millis() as u64) < OVERLAY_GROW_MS
        })
    }

    /// Whether a short-lived animation (toast slide/fade, overlay unfold,
    /// welcome cascade) wants the fast tick instead of the ambient 125 ms one.
    pub fn fast_anim_active(&self, now: Instant) -> bool {
        self.toast_needs_anim(now) || self.welcome_playing(now) || self.overlay_growing(now)
    }

    /// The shared two-state blink phase for pulsing dots: driven by
    /// `anim_frame` while the fast anim tick is armed, by `second_parity` when
    /// only the 1 s tick runs. Either way it is a pure function of counters the
    /// existing timers already advance — it never arms a timer of its own.
    pub fn pulse_lit(&self, now: Instant) -> bool {
        if self.turn_active || self.transcript.has_backlog() || self.needs_anim_tick(now) {
            self.anim_frame % PULSE_FRAMES < PULSE_FRAMES / 2
        } else {
            self.second_parity
        }
    }

    /// Whether a periodic 1 s tick is needed (live agents or tasks: durations
    /// change, and the SESSION-stats debounce has to fire on a quiet screen).
    pub fn needs_second_tick(&self) -> bool {
        self.turn_active
            || self.rail.has_live_agents()
            || self.rail.has_running_tasks()
            || self.rail.idle_debounce_pending()
    }

    /// Whether the animation tick is needed: the turn running, an active panel
    /// agent, a pending permission pulse, a young overlay unfold, the welcome
    /// cascade, or the reconnect dots. Every arm is a visible animation.
    pub fn needs_anim_tick(&self, now: Instant) -> bool {
        if self.turn_active {
            return true;
        }
        if self.welcome_playing(now) || self.overlay_growing(now) {
            return true;
        }
        // A permission request pulses until it is answered; waiting on the
        // reader is activity, so the tick is allowed.
        if self.has_pending_permission() {
            return true;
        }
        // A `NeedsInput` slot badge blinks while it waits — armed only while
        // one exists (an unanswered question is the active slot's own badge).
        if !self.questions.is_empty()
            || self
                .slots
                .iter()
                .flatten()
                .any(|s| s.status == pacode_types::SessionStatusKind::NeedsInput)
        {
            return true;
        }
        if matches!(self.connection, Connection::Reconnecting { .. }) {
            return true;
        }
        match &self.focus {
            Focus::Panel {
                target: PanelTarget::Agent(id),
                ..
            } => self.rail.agent(id).is_some_and(|a| a.status.is_active()),
            _ => self
                .panel
                .target
                .as_ref()
                .and_then(|t| match t {
                    PanelTarget::Agent(id) => self.rail.agent(id),
                    PanelTarget::Task(_)
                    | PanelTarget::McpServer(_)
                    | PanelTarget::FileDiff { .. }
                    | PanelTarget::CommitDiff { .. } => None,
                })
                .is_some_and(|a| a.status.is_active()),
        }
    }

    /// Whether the paced stream needs its 33 ms tick.
    pub fn needs_stream_tick(&self) -> bool {
        self.transcript.has_backlog() || self.panel.agent_transcript.has_backlog()
    }

    pub fn tick_stream(&mut self, now: Instant) -> bool {
        let r1 = self.transcript.tick_stream(now);
        let r2 = self.panel.agent_transcript.tick_stream(now);
        if r1 || r2 {
            self.dirty = true;
            true
        } else {
            false
        }
    }
}
