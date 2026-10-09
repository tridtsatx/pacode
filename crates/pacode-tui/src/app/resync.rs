//! Healing a gap in the event stream: the seq-gap `GetSnapshot` resync and
//! its barrier, and the global list refresh after a daemon-wide resync or a
//! reconnect.

use std::time::Instant;

use pacode_client::ClientEventContext;
use pacode_types::{Reply, Request};

use crate::keys::Action;
use crate::state::{AppState, SessionContext};

pub(super) fn clear_list_loading(state: &mut AppState) {
    state.lists.models.in_flight = false;
    state.lists.plugins.in_flight = false;
    state.lists.marketplace.in_flight = false;
    state.lists.mcp.in_flight = false;
    state.lists.sessions.in_flight = false;
    state.lists.git.in_flight = false;
}

pub(super) fn take_resync_action(state: &mut AppState, now: Instant) -> Option<Action> {
    let request = state.take_resync_request(now)?;
    clear_list_loading(state);
    Some(Action::Send(request))
}

pub(super) fn take_global_resync_actions(state: &mut AppState, requested: bool) -> Vec<Action> {
    if !requested
        || state.meta.is_none()
        || state.pending_attach.is_some()
        || !matches!(state.connection, crate::state::Connection::Connected)
    {
        return Vec::new();
    }
    let context = state.ui_context();
    if state.global_refresh_context_is_current(context) {
        return Vec::new();
    }
    // A list already waiting in the pending queue is about to be asked for by
    // the attach/snapshot flush, so the refresh must not ask a second time.
    let queued: Vec<crate::cache::ListKind> = state
        .pending_requests
        .iter()
        .filter_map(crate::cache::ListKind::for_request)
        .collect();
    let actions = global_resync_actions()
        .into_iter()
        .filter(|action| match action {
            Action::Send(request) => crate::cache::ListKind::for_request(request)
                .is_none_or(|kind| !queued.contains(&kind) && !state.lists.get(kind).in_flight),
            _ => false,
        })
        .collect::<Vec<_>>();
    if !state.begin_global_refresh(actions.len()) {
        return Vec::new();
    }
    state.record_global_refresh_context(context);
    actions
}

fn release_failed_resync(state: &mut AppState, now: Instant) -> Vec<Action> {
    state.release_resync_barrier(true, now);
    state
        .drain_prompt_queue()
        .map(Action::Send)
        .into_iter()
        .collect()
}

/// `BgResponse::ResyncDone`: apply the resync snapshot, or release the
/// barrier so the loop can retry.
pub(super) fn fold_resync_done(
    state: &mut AppState,
    context: SessionContext,
    client_context: ClientEventContext,
    result: Result<Reply, pacode_client::ClientError>,
) -> Vec<Action> {
    if !state.resync_reply_is_current(&context, &client_context) {
        return Vec::new();
    }
    let mut actions = match result {
        Ok(Reply::Attached(snapshot) | Reply::Snapshot(snapshot)) => {
            let Some(actions) = apply_resync_snapshot(state, snapshot, &context, &client_context)
            else {
                log::warn!("snapshot resync returned a stale snapshot");
                return release_failed_resync(state, Instant::now());
            };
            actions
        }
        Ok(Reply::Error { message }) => {
            log::warn!("snapshot resync answered with error: {message}");
            return release_failed_resync(state, Instant::now());
        }
        Ok(reply) => {
            log::warn!("snapshot resync returned an unexpected reply: {reply:?}");
            return release_failed_resync(state, Instant::now());
        }
        Err(error) => {
            log::warn!("snapshot resync failed: {error}");
            return release_failed_resync(state, Instant::now());
        }
    };
    if let Some(request) = state.drain_prompt_queue() {
        actions.push(Action::Send(request));
    }
    actions
}

fn apply_resync_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> Option<Vec<Action>> {
    if !state.context_is_current(context)
        || snapshot.seq < state.last_event_seq.unwrap_or(0)
        || !state.accepts_event_snapshot(client_context, &snapshot)
    {
        return None;
    }
    let project_changed = state
        .meta
        .as_ref()
        .is_some_and(|meta| meta.cwd != snapshot.meta.cwd);
    if !state.apply_snapshot(snapshot, Instant::now()) {
        return None;
    }
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    if project_changed {
        actions.push(Action::GitLog { after: None });
    }
    Some(actions)
}

fn global_resync_actions() -> Vec<Action> {
    vec![
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListMcpServers),
        Action::Send(Request::ListSessions { limit: 50 }),
        Action::Send(Request::ListModels),
        Action::Send(Request::ListAuth),
    ]
}
