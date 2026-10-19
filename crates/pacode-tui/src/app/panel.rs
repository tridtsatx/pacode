//! Paged loads: the side panel's agent history / task output (one load per
//! target in flight) and older pages of the main transcript.

use std::sync::Arc;
use std::time::Instant;

use pacode_client::{Client, ClientEventContext};
use pacode_types::state::ToastLevel;
use pacode_types::{AgentId, Reply, Request};
use tokio::sync::mpsc;

use super::bg_response::BgResponse;
use super::reply::session_reply_is_current;
use crate::keys::Action;
use crate::state::{AppState, PanelTarget, SessionContext};

struct PanelLoads {
    next: u64,
    in_flight: Option<(SessionContext, PanelTarget, u64)>,
    applied: u64,
}

thread_local! {
    static PANEL_LOADS: std::cell::RefCell<PanelLoads> = const {
        std::cell::RefCell::new(PanelLoads { next: 0, in_flight: None, applied: 0 })
    };
}

/// One panel load per (session, target): navigation emits `Action::LoadPanel`
/// per step, so without this a held arrow key runs a `GetHistory` page per
/// agent and the replies race into the same panel. Returns the token the reply
/// must carry.
pub(super) fn begin_panel_load(context: &SessionContext, target: &PanelTarget) -> Option<u64> {
    PANEL_LOADS.with(|cell| {
        let mut loads = cell.borrow_mut();
        if loads
            .in_flight
            .as_ref()
            .is_some_and(|(pending, panel, _)| pending == context && panel == target)
        {
            return None;
        }
        loads.next = loads.next.wrapping_add(1);
        let token = loads.next;
        loads.in_flight = Some((context.clone(), target.clone(), token));
        Some(token)
    })
}

/// Whether this reply is the newest load of its target. A page that a newer
/// load already replaced is dropped instead of overwriting the panel, and the
/// slot is released either way — a load left behind would refuse every later
/// request for the same target.
fn panel_load_reply_is_current(context: &SessionContext, target: &PanelTarget, token: u64) -> bool {
    PANEL_LOADS.with(|cell| {
        let mut loads = cell.borrow_mut();
        if loads
            .in_flight
            .as_ref()
            .is_none_or(|(pending, panel, sent)| {
                pending != context || panel != target || *sent != token
            })
        {
            return false;
        }
        loads.in_flight = None;
        if token <= loads.applied {
            return false;
        }
        loads.applied = token;
        true
    })
}

/// The `Subscribe` matching what the TUI draws live: the main agent, plus the agent the
/// panel shows. Other agents' deltas and items are dropped by the daemon and their rail
/// rows arrive coalesced; their tool calls still come through for the files list.
pub(super) fn interest_request(panel: Option<&PanelTarget>) -> Request {
    let mut agents = vec![AgentId::main()];
    if let Some(PanelTarget::Agent(id)) = panel
        && !id.is_main()
    {
        agents.push(id.clone());
    }
    Request::Subscribe { agents, all: false }
}

/// `Action::LoadPanel`: the open panel's agent history or task output.
pub(super) fn load_panel(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    if state.meta.is_none() || state.awaiting_snapshot {
        return;
    }
    let context = state.session_context();
    let client_context = client.event_context();
    if client_context.session.is_none() {
        return;
    }
    let Some(target) = state.panel.target.clone() else {
        return;
    };
    let request = match &target {
        PanelTarget::Agent(id) => Some(Request::GetHistory {
            agent: id.clone(),
            before_seq: None,
            limit: 300,
        }),
        PanelTarget::Task(id) => Some(Request::GetTaskOutput {
            task: id.clone(),
            tail_lines: 300,
        }),
        PanelTarget::McpServer(_)
        | PanelTarget::FileDiff { .. }
        | PanelTarget::CommitDiff { .. } => None,
    };
    let Some(request) = request else {
        return;
    };
    let Some(token) = begin_panel_load(&context, &target) else {
        return;
    };
    // An agent's live stream must be on before its history is read: an item added
    // between the two would otherwise be in neither. One task, Subscribe acknowledged
    // first, then the page.
    let subscribe =
        matches!(target, PanelTarget::Agent(_)).then(|| interest_request(Some(&target)));
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    tokio::spawn(async move {
        if let Some(subscribe) = subscribe
            && let Err(error) = cl
                .request_session_for(client_context.clone(), subscribe)
                .await
        {
            log::warn!("panel subscribe failed: {error}");
        }
        let result = cl
            .request_session_for(client_context.clone(), request)
            .await;
        let _ = tx.send(BgResponse::LoadPanelReply {
            context,
            client_context,
            target,
            token,
            result,
        });
    });
}

/// `Action::LoadHistory`: the next older page of the main transcript.
pub(super) fn load_history(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    if state.meta.is_none() || state.awaiting_snapshot || state.transcript.loading_history {
        return;
    }
    let context = state.session_context();
    let client_context = client.event_context();
    if client_context.session.is_none() {
        return;
    }
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    let first_cell_id = state.transcript.oldest_seq();
    let limit = state.config.session.history_page;
    log::debug!("history page requested: before_seq={first_cell_id:?}, limit={limit}");
    state.transcript.loading_history = true;
    tokio::spawn(async move {
        let result = cl
            .request_session_for(
                client_context.clone(),
                Request::GetHistory {
                    agent: AgentId::main(),
                    before_seq: first_cell_id,
                    limit,
                },
            )
            .await;
        let _ = tx.send(BgResponse::LoadHistoryReply {
            context,
            client_context,
            before_seq: first_cell_id,
            result,
        });
    });
}

/// `BgResponse::LoadPanelReply`.
pub(super) fn fold_load_panel_reply(
    state: &mut AppState,
    context: SessionContext,
    client_context: ClientEventContext,
    target: PanelTarget,
    token: u64,
    result: Result<Reply, pacode_client::ClientError>,
) -> Vec<Action> {
    if !panel_load_reply_is_current(&context, &target, token) {
        return Vec::new();
    }
    if !session_reply_is_current(state, &context, &client_context) {
        return Vec::new();
    }
    let shown = state.panel.target.as_ref() == Some(&target);
    match result {
        Ok(Reply::History {
            items, has_more, ..
        }) => {
            if shown {
                state.panel.agent_transcript.reset(items, has_more);
                state.dirty = true;
            }
        }
        Ok(Reply::TaskOutput {
            lines, total_lines, ..
        }) => {
            if shown {
                state.panel.task_lines = lines;
                state.panel.task_total_lines = total_lines;
                state.dirty = true;
            }
        }
        Ok(Reply::Error { message }) => {
            if shown {
                state.push_toast(
                    ToastLevel::Error,
                    "Panel load error".into(),
                    Some(message),
                    Instant::now(),
                );
            }
        }
        Err(e) if shown => {
            state.push_toast(
                ToastLevel::Error,
                "Panel load error".into(),
                Some(e.to_string()),
                Instant::now(),
            );
        }
        _ => {}
    }
    vec![]
}

/// `BgResponse::LoadHistoryReply`.
pub(super) fn fold_load_history_reply(
    state: &mut AppState,
    context: SessionContext,
    client_context: ClientEventContext,
    before_seq: Option<u64>,
    result: Result<Reply, pacode_client::ClientError>,
) -> Vec<Action> {
    if !session_reply_is_current(state, &context, &client_context) {
        return Vec::new();
    }
    if state.transcript.oldest_seq() != before_seq {
        // A page that no longer answers the oldest row would wedge the
        // flag: the next scroll would never ask for history again.
        state.transcript.loading_history = false;
        return Vec::new();
    }
    match result {
        Ok(Reply::History {
            items, has_more, ..
        }) => {
            log::debug!(
                "history page loaded: {} items, has_more={has_more}",
                items.len()
            );
            let shift = state.transcript.prepend(items, has_more);
            if !state.agent_replaces_dialog() {
                state.selection.rebase_lines(shift);
            }
            state.dirty = true;
        }
        _ => {
            log::warn!("history page request failed or returned non-history reply");
            state.transcript.loading_history = false;
        }
    }
    vec![]
}
