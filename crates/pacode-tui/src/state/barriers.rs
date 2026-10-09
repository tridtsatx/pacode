//! Requests the prompt path waits behind, from send to reply. A prompt sent
//! after a settings change (route, effort, mode, ZDR) must not reach the daemon
//! ahead of it and run on the old settings; a prompt queued before a `/clear`
//! belongs to the conversation being cleared and must not drain into the gap,
//! where the clear would sweep it or it would open the cleared context.
//!
//! A barrier drops on its reply (`BgResponse::BarrierReply`), on
//! `ContextCleared` for a clear, and on a connection loss, after which the
//! reply's client context is stale and it never folds. A session switch resets
//! the session state, barriers included.

use std::time::Instant;

use pacode_types::Request;
use pacode_types::state::ToastLevel;

use super::{AppState, SessionUiState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestBarrier {
    Clear,
    Settings,
}

impl RequestBarrier {
    pub fn for_request(request: &Request) -> Option<Self> {
        match request {
            Request::ClearContext => Some(Self::Clear),
            Request::SetModel(_)
            | Request::SetEffort(_)
            | Request::SetMode(_)
            | Request::SetZdr(_) => Some(Self::Settings),
            Request::Hello(_)
            | Request::Attach(_)
            | Request::Detach
            | Request::Shutdown { .. }
            | Request::Ping
            | Request::ListSessions { .. }
            | Request::RuntimeStats
            | Request::ListModels
            | Request::RefreshModels
            | Request::ListMcpServers
            | Request::RestartMcpServer { .. }
            | Request::SetMcpServerEnabled { .. }
            | Request::GetMcpPrompt { .. }
            | Request::ListPlugins
            | Request::RunPluginCommand { .. }
            | Request::BrowseMarketplace { .. }
            | Request::InstallPlugin { .. }
            | Request::UninstallPlugin { .. }
            | Request::ListAuth
            | Request::Login { .. }
            | Request::Logout { .. }
            | Request::SetAuthAccount { .. }
            | Request::Analyze
            | Request::UserMessage { .. }
            | Request::Interrupt
            | Request::InterruptAll
            | Request::CloseSession
            | Request::PermissionReply { .. }
            | Request::ExplainPermission { .. }
            | Request::SetNoticeLock(_)
            | Request::SetModelFlag { .. }
            | Request::StopAgent(_)
            | Request::KillTask(_)
            | Request::AckTask(_)
            | Request::GetSnapshot
            | Request::GetHistory { .. }
            | Request::GetTaskOutput { .. }
            | Request::Compact
            | Request::DetachTurn
            | Request::AnswerQuestion { .. }
            | Request::ListCronJobs
            | Request::AddCronJob { .. }
            | Request::RemoveCronJob(_)
            | Request::SetCronEnabled { .. }
            | Request::RunCronJobNow(_)
            | Request::ListMonitors
            | Request::StopMonitor(_)
            | Request::GitLog { .. }
            | Request::GitShow { .. }
            | Request::GitDiff { .. } => None,
        }
    }
}

impl SessionUiState {
    /// Whether queued prompts, retries and pending sequences must wait.
    pub fn prompt_path_held(&self) -> bool {
        self.clear_in_flight || self.settings_in_flight > 0
    }

    pub fn raise_barrier(&mut self, barrier: RequestBarrier) {
        match barrier {
            RequestBarrier::Clear => self.clear_in_flight = true,
            RequestBarrier::Settings => self.settings_in_flight += 1,
        }
    }

    pub fn lower_barrier(&mut self, barrier: RequestBarrier) {
        match barrier {
            RequestBarrier::Clear => self.clear_in_flight = false,
            RequestBarrier::Settings => {
                self.settings_in_flight = self.settings_in_flight.saturating_sub(1);
            }
        }
    }

    /// The connection is gone: no reply in flight will fold any more.
    pub fn drop_request_barriers(&mut self) {
        self.clear_in_flight = false;
        self.settings_in_flight = 0;
    }
}

impl AppState {
    /// The conversation was cleared (our `/clear` answered `Ok`, or any
    /// client's `ContextCleared` arrived): the prompts waiting to go out were
    /// typed for it, so they go too, and a toast says how many.
    pub fn drop_prompts_of_cleared_context(&mut self, now: Instant) {
        self.clear_in_flight = false;
        let mut dropped = self.input.prompt_queue.len();
        self.input.queue_clear();
        self.session.pending_sequences.retain(|sequence| {
            let prompt = sequence
                .requests
                .iter()
                .any(|request| matches!(request, Request::UserMessage { .. }));
            dropped += usize::from(prompt);
            !prompt
        });
        if dropped == 0 {
            return;
        }
        let s = if dropped == 1 { "" } else { "s" };
        self.push_toast(
            ToastLevel::Warn,
            format!("{dropped} queued prompt{s} dropped"),
            Some("they belonged to the cleared conversation".to_string()),
            now,
        );
        self.dirty = true;
    }
}
