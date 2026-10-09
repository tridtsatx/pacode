//! Attaching a session: the background attach request and its retry, the
//! session switch, folding the attach reply, and the flush of everything that
//! waited for a session (queued requests, `initial_prompt`, the post-attach
//! list batch).

use std::sync::{Arc, atomic::Ordering};
use std::time::Instant;

use pacode_client::{Client, ClientEventContext};
use pacode_types::state::ToastLevel;
use pacode_types::{Attach, Request};
use tokio::sync::mpsc;

use super::bg_response::BgResponse;
use super::prompt::prompt_gate_open;
use crate::keys::Action;
use crate::state::{AppState, SessionContext};

#[derive(Clone, Copy)]
pub(super) enum AttachRequestKind {
    Attach,
    Switch,
    Retry,
}

pub(super) struct AttachRequest {
    pub(super) context: SessionContext,
    pub(super) token: u64,
    pub(super) request_client_context: ClientEventContext,
    pub(super) attach: Attach,
    pub(super) kind: AttachRequestKind,
    pub(super) timing_started: Option<Instant>,
}

pub(super) fn spawn_attach_request(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    request: AttachRequest,
) {
    state.attach_in_flight = true;
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    tokio::spawn(async move {
        let result = match request.kind {
            AttachRequestKind::Attach => cl.attach(request.attach.clone()).await,
            AttachRequestKind::Switch => cl.switch_session(request.attach.clone()).await,
            AttachRequestKind::Retry => cl.retry_attach(request.attach.clone()).await,
        };
        if let Some(started) = request.timing_started {
            crate::timing::mark(crate::timing::enabled(), "attach_snapshot", started);
        }
        let client_context = cl.event_context();
        let _ = tx.send(BgResponse::AttachFinished {
            context: request.context,
            token: request.token,
            request_client_context: request.request_client_context,
            client_context,
            attach: request.attach,
            result,
        });
    });
}

pub(super) fn dispatch_pending_attach_retry(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let Some(pending) = state.take_pending_attach_retry() else {
        return;
    };
    spawn_attach_request(
        state,
        client,
        bg_tx,
        AttachRequest {
            context: pending.context,
            token: pending.token,
            request_client_context: client.event_context(),
            attach: pending.attach,
            kind: AttachRequestKind::Retry,
            timing_started: None,
        },
    );
}

/// `Action::SwitchSession`: leave the active slot and attach the target.
pub(super) fn switch_session(
    attach: Attach,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    if state.pending_attach.as_ref().is_some_and(|pending| {
        pending.attach == attach && (pending.retry_requested || state.attach_in_flight)
    }) {
        return;
    }
    if state.meta.is_some() {
        state.leave_active_slot();
    }
    if state
        .pending_attach
        .as_ref()
        .is_none_or(|pending| pending.attach != attach)
    {
        state.begin_attach(attach.clone());
    }
    if state.attach_in_flight {
        return;
    }
    let attach_client_context = client.event_context();
    let token = state
        .set_pending_attach_client_epoch(attach_client_context.session_epoch.wrapping_add(1))
        .expect("pending attach must exist");
    let attach_epoch = state
        .pending_attach_epoch_signal()
        .expect("pending attach epoch must exist");
    attach_epoch.store(
        attach_client_context.session_epoch.wrapping_add(1),
        Ordering::Release,
    );
    let context = state.session_context();
    spawn_attach_request(
        state,
        client,
        bg_tx,
        AttachRequest {
            context,
            token,
            request_client_context: attach_client_context,
            attach,
            kind: AttachRequestKind::Switch,
            timing_started: None,
        },
    );
}

/// `BgResponse::AttachFinished`: apply the snapshot of the attach still
/// pending, or retry / fail it.
pub(super) fn fold_attach_finished(
    state: &mut AppState,
    context: SessionContext,
    token: u64,
    request_client_context: ClientEventContext,
    client_context: ClientEventContext,
    attach: Attach,
    result: Result<pacode_types::SessionSnapshot, pacode_client::ClientError>,
) -> Vec<Action> {
    if request_client_context.connection_epoch > client_context.connection_epoch
        || request_client_context.session_epoch > client_context.session_epoch
    {
        return Vec::new();
    }
    if !state.context_is_current(&context)
        || state.pending_attach.as_ref().is_none_or(|pending| {
            pending.context != context || pending.attach != attach || pending.token != token
        })
    {
        return Vec::new();
    }
    match result {
        Ok(snapshot) => {
            let had_pending_attach = state.pending_attach.is_some();
            let Some(actions) =
                apply_attach_snapshot(state, snapshot, &context, token, &client_context, &attach)
            else {
                return Vec::new();
            };
            state.attach_in_flight = false;
            log::info!(
                "TUI attached to session {}",
                state
                    .meta
                    .as_ref()
                    .map(|meta| meta.id.as_str())
                    .unwrap_or("")
            );
            let mut ordered_actions = actions;
            ordered_actions.extend(flush_completed_attach(state, had_pending_attach));
            ordered_actions
        }
        Err(pacode_client::ClientError::Timeout(err)) => {
            state.attach_in_flight = false;
            if state.request_attach_retry() {
                log::warn!("session attach timed out; retrying the pending attempt: {err}");
                Vec::new()
            } else {
                log::error!("session attach retry limit reached: {err}");
                state.clear_pending_attach(format!("attach failed: {err}"));
                if context.generation == 0 && context.slot == 0 {
                    state.attach_error = Some(pacode_client::ClientError::Timeout(err));
                    state.quit = true;
                }
                Vec::new()
            }
        }
        Err(err) => {
            state.attach_in_flight = false;
            if attach_error_is_transient(&err) {
                log::warn!("session attach will retry after reconnect: {err}");
                return Vec::new();
            }
            let reason = err.to_string();
            state.clear_pending_attach(reason.clone());
            log::error!("session attach failed: {err}");
            state.push_notice_with_level(ToastLevel::Error, format!("attach failed: {reason}"));
            if context.generation == 0 && context.slot == 0 {
                state.attach_error = Some(err);
                state.quit = true;
            }
            Vec::new()
        }
    }
}

fn attach_error_is_transient(error: &pacode_client::ClientError) -> bool {
    matches!(error, pacode_client::ClientError::Disconnected)
}

/// Fold an attach/snapshot reply into state: record the slot, apply the
/// snapshot, flush requests queued while no session existed, and re-fetch
/// the git list on a project switch (§8.2). Shared by `Reply::Attached`/
/// `Reply::Snapshot` in [`handle_reply`] and `BgResponse::AttachFinished`.
fn apply_attach_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
    token: u64,
    client_context: &ClientEventContext,
    attach: &Attach,
) -> Option<Vec<Action>> {
    if !state.accepts_attach_with_token(context, token, client_context, attach, &snapshot) {
        return None;
    }
    state.pending_attach = None;
    state.accept_attach_context(client_context);
    Some(apply_snapshot_and_actions(state, snapshot))
}

pub(super) fn apply_scoped_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
) -> Vec<Action> {
    if !state.context_is_current(context)
        || state
            .meta
            .as_ref()
            .is_some_and(|meta| meta.id != snapshot.meta.id)
    {
        return Vec::new();
    }
    apply_snapshot_and_actions(state, snapshot)
}

fn apply_snapshot_and_actions(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
) -> Vec<Action> {
    let project_changed = state
        .meta
        .as_ref()
        .is_some_and(|meta| meta.cwd != snapshot.meta.cwd);
    state.apply_snapshot(snapshot, Instant::now());
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    if project_changed {
        actions.push(Action::GitLog { after: None });
    }
    actions
}

fn post_attach_actions(state: &mut AppState) -> Vec<Action> {
    let mut actions = Vec::new();
    if prompt_gate_open(state)
        && let Some(prompt) = state.initial_prompt.take()
    {
        actions.push(Action::Send(Request::UserMessage {
            text: prompt,
            images: Vec::new(),
            message_id: None,
        }));
    }
    actions.extend([
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListMcpServers),
        Action::Send(Request::ListSessions { limit: 50 }),
        Action::Send(Request::ListModels),
        Action::GitLog { after: None },
    ]);
    if !state.config.plugins.marketplace.is_empty() {
        let source = state.config.plugins.marketplace.clone();
        actions.push(Action::Send(Request::BrowseMarketplace {
            source,
            query: String::new(),
        }));
    }
    actions
}

fn flush_completed_attach(state: &mut AppState, had_pending_attach: bool) -> Vec<Action> {
    if !had_pending_attach || state.pending_attach.is_some() {
        return Vec::new();
    }
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    actions.extend(post_attach_actions(state));
    actions
}

pub(super) fn snapshot_actions_after_apply(
    state: &mut AppState,
    had_pending_attach: bool,
    snapshot_landed: bool,
    snapshot_applied: bool,
) -> Vec<Action> {
    if !snapshot_landed || !snapshot_applied {
        return Vec::new();
    }
    if had_pending_attach && state.pending_attach.is_none() {
        return flush_completed_attach(state, true);
    }
    state.pending_requests.drain(..).map(Action::Send).collect()
}
