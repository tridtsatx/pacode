//! Toasts and transcript notices: the toast strip and its expiry, notice
//! lines, background-completion reports, login progress, and the deferred
//! clipboard copy's report.

use std::time::Instant;

use pacode_types::time::now_ms;
use pacode_types::{Event, ToastLevel, TranscriptKind};

use super::{
    AppState, BackgroundKind, BackgroundOutcome, BackgroundResult, Cell, CellKind, Focus, Overlay,
};

/// A background job shorter than this is not reported in the transcript: it is
/// over before the reader could act on it, and the rail already carried it.
pub const BACKGROUND_NOTICE_MIN_MS: u64 = 30_000;

/// Whether a finished background job earns a transcript line. Failures always do,
/// however brief: a command that died immediately is exactly what must be seen.
pub fn background_worth_reporting(outcome: BackgroundOutcome, duration_ms: u64) -> bool {
    match outcome {
        BackgroundOutcome::Completed => duration_ms >= BACKGROUND_NOTICE_MIN_MS,
        BackgroundOutcome::Failed | BackgroundOutcome::Killed => true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub level: ToastLevel,
    pub title: String,
    pub detail: Option<String>,
    /// Liveness link to an input token (e.g. `[Image 3]`): once the label is
    /// gone from the input text the toast is dead — a "pasted" line must not
    /// outlive the token it describes.
    pub token_label: Option<String>,
    pub shown_at: Instant,
}

pub const TOAST_TTL_SECS: u64 = 6;
pub const TOAST_MAX: usize = 2;
/// Toast exit window: the strip line fades to `theme.faint` this long before
/// expiry.
pub const TOAST_EXIT_MS: u64 = 800;

impl AppState {
    pub(crate) fn apply_login_progress(
        &mut self,
        provider: String,
        stage: pacode_types::LoginStage,
        now: Instant,
    ) {
        match stage {
            pacode_types::LoginStage::OpenUrl { url, opened } => {
                let detail = if opened {
                    format!("Opened browser to sign in to {provider}: {url}")
                } else {
                    format!("Open this URL in your browser to sign in to {provider}:\n{url}")
                };
                self.push_toast(ToastLevel::Info, "Login".into(), Some(detail), now);
            }
            pacode_types::LoginStage::Waiting => {
                self.push_toast(
                    ToastLevel::Info,
                    "Login".into(),
                    Some(format!("Waiting for authorization from {provider}...")),
                    now,
                );
            }
            pacode_types::LoginStage::Exchanging => {
                self.push_toast(
                    ToastLevel::Info,
                    "Login".into(),
                    Some(format!("Exchanging authorization code for {provider}...")),
                    now,
                );
            }
            pacode_types::LoginStage::Done { label } => {
                self.push_toast(ToastLevel::Success, "Signed in".into(), Some(label), now);
                if matches!(self.focus, Focus::Overlay(Overlay::LoginPicker { .. })) {
                    self.focus = Focus::Normal;
                }
            }
            pacode_types::LoginStage::Failed { message } => {
                self.push_toast(
                    ToastLevel::Error,
                    format!("Login failed: {provider}"),
                    Some(message),
                    now,
                );
                if matches!(self.focus, Focus::Overlay(Overlay::LoginPicker { .. })) {
                    self.focus = Focus::Normal;
                }
            }
        }
        self.dirty = true;
    }

    pub fn push_toast(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
    ) {
        self.push_toast_inner(level, title, detail, None, now);
    }

    /// `push_toast` plus a liveness link: the toast expires early once
    /// `token_label` leaves the input text (token deleted or submitted).
    pub fn push_toast_linked(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        now: Instant,
        token_label: String,
    ) {
        self.push_toast_inner(level, title, detail, Some(token_label), now);
    }

    fn push_toast_inner(
        &mut self,
        level: ToastLevel,
        title: String,
        detail: Option<String>,
        token_label: Option<String>,
        now: Instant,
    ) {
        let toast = Toast {
            level,
            title,
            detail,
            token_label,
            shown_at: now,
        };
        self.toasts.push_back(toast);
        while self.toasts.len() > TOAST_MAX {
            self.toasts.pop_front();
        }
        self.dirty = true;
    }

    /// Drop expired toasts and toasts whose linked token left the input;
    /// returns true when something changed.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        let input_text = self.input.text.clone();
        self.toasts.retain(|t| {
            now.saturating_duration_since(t.shown_at).as_secs() < TOAST_TTL_SECS
                && t.token_label
                    .as_ref()
                    .is_none_or(|label| input_text.contains(label.as_str()))
        });
        let changed = self.toasts.len() != before;
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Whether a toast sits inside its fade-out window, i.e. the fast tick
    /// has visible work. Bounded: the last `TOAST_EXIT_MS` of each toast.
    pub fn toast_needs_anim(&self, now: Instant) -> bool {
        let ttl_ms = TOAST_TTL_SECS * 1000;
        self.toasts.iter().any(|t| {
            let age = now.saturating_duration_since(t.shown_at).as_millis() as u64;
            ttl_ms.saturating_sub(age) < TOAST_EXIT_MS
        })
    }

    pub fn push_notice(&mut self, text: String) {
        self.push_notice_with_level(ToastLevel::Info, text);
    }

    /// The one way a notice enters the transcript: it is the answer to
    /// something the user just did, so the view scrolls to it even when the
    /// user was reading further up.
    pub fn push_notice_with_level(&mut self, level: ToastLevel, text: String) {
        let now = now_ms();
        self.transcript.push_cell(Cell {
            id: now,
            kind: CellKind::Item(TranscriptKind::Notice { level, text }),
            version: 0,
            ts_ms: now,
            stats: None,
            perm_explain: None,
        });
        self.transcript.scroll_to_bottom();
        self.dirty = true;
    }

    /// Observe background tasks and subagents ending and record a line in the transcript.
    ///
    /// Only jobs that actually ran for a while are worth interrupting the reader
    /// for: anything shorter than `BACKGROUND_NOTICE_MIN_MS` finished about as
    /// fast as reading about it would take, and the rail already showed it. The
    /// line is the only report — the toast that used to accompany it said the
    /// same thing twice.
    pub fn observe_background_completion(&mut self, event: &Event) {
        match event {
            Event::TaskUpdated(info) if info.status.is_terminal() => {
                let was_terminal = self
                    .rail
                    .task(&info.id)
                    .is_some_and(|task| task.status.is_terminal());
                if was_terminal {
                    return;
                }
                let outcome = match info.status {
                    pacode_types::TaskStatus::Completed => {
                        if info.exit_code.unwrap_or(0) == 0 {
                            BackgroundOutcome::Completed
                        } else {
                            BackgroundOutcome::Failed
                        }
                    }
                    pacode_types::TaskStatus::Failed => BackgroundOutcome::Failed,
                    pacode_types::TaskStatus::Killed => BackgroundOutcome::Killed,
                    pacode_types::TaskStatus::Running => return,
                };
                let duration_ms = info.duration_ms(now_ms());
                if !background_worth_reporting(outcome, duration_ms) {
                    return;
                }
                let label = if info.command.is_empty() {
                    info.label.clone()
                } else {
                    info.command.clone()
                };
                self.push_background_result(BackgroundResult {
                    kind: BackgroundKind::Task,
                    label,
                    outcome,
                    exit_code: info.exit_code,
                    duration_ms,
                });
            }
            Event::AgentUpdated(info) if !info.id.is_main() && !info.status.is_live() => {
                let was_finished = self
                    .rail
                    .agent(&info.id)
                    .is_some_and(|agent| !agent.status.is_live());
                if was_finished {
                    return;
                }
                let outcome = match info.status {
                    pacode_types::AgentStatus::Finished => BackgroundOutcome::Completed,
                    pacode_types::AgentStatus::Stopped => BackgroundOutcome::Killed,
                    pacode_types::AgentStatus::Failed => BackgroundOutcome::Failed,
                    pacode_types::AgentStatus::Idle
                    | pacode_types::AgentStatus::Thinking
                    | pacode_types::AgentStatus::RunningTool
                    | pacode_types::AgentStatus::WaitingApproval => return,
                };
                let duration_ms = info.duration_ms(now_ms());
                if !background_worth_reporting(outcome, duration_ms) {
                    return;
                }
                self.push_background_result(BackgroundResult {
                    kind: BackgroundKind::Agent,
                    label: info.name.clone(),
                    outcome,
                    exit_code: None,
                    duration_ms,
                });
            }
            _ => {}
        }
    }

    fn push_background_result(&mut self, result: BackgroundResult) {
        let now = now_ms();
        self.transcript.push_cell(Cell {
            id: now,
            kind: CellKind::BackgroundResult(result),
            version: 0,
            ts_ms: now,
            stats: None,
            perm_explain: None,
        });
        self.transcript.scroll_to_bottom();
        self.dirty = true;
    }

    /// The deferred clipboard copy finished — post the toasts draw used to
    /// post inline (§8.1 moved the IO off the frame; the reporting follows it).
    pub fn on_copy_done(&mut self, auto: bool, chars: usize) {
        let detail = auto.then(|| "disable autocopy in /config".to_string());
        self.push_toast(
            ToastLevel::Info,
            format!("copied {chars} chars"),
            detail,
            Instant::now(),
        );
        if crate::clipboard::remote_hint().is_some() && !self.clipboard_warned {
            self.clipboard_warned = true;
            self.push_toast(
                ToastLevel::Warn,
                "remote clipboard".to_string(),
                Some(crate::clipboard::REMOTE_HINT_TEXT.to_string()),
                Instant::now(),
            );
        }
    }
}
