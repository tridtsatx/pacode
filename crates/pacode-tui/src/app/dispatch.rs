//! Action dispatch: every `keys::Action` the loop receives ends here — local
//! work runs inline, daemon requests are spawned with their reply routed back
//! through `BgResponse`, and requests that cannot fly yet wait in
//! `state.pending_requests`.

use std::sync::Arc;
use std::time::Instant;

use pacode_client::Client;
use pacode_types::time::now_ms;
use pacode_types::{Request, TranscriptKind};
use tokio::sync::mpsc;

use super::attach::switch_session;
use super::bg_response::BgResponse;
use super::panel::{load_history, load_panel};
use super::prompt::{continue_prompt_batch, queue_user_message_request, send_sequence};
use super::reply::{ReplyScope, send_request};
use crate::keys::Action;
use crate::state::AppState;
use crate::state::transcript::{Cell, CellKind};

/// Cap on session-bound requests queued while the attach is in flight —
/// past it the oldest is dropped; a user cannot out-type a snapshot.
const PENDING_REQUESTS_CAP: usize = 16;

pub(super) fn dispatch_action(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    dispatch_action_inner(act, state, client, bg_tx, false);
}

pub(super) fn dispatch_global_refresh_action(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    dispatch_action_inner(act, state, client, bg_tx, true);
}

fn dispatch_action_inner(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    global_refresh: bool,
) {
    match act {
        Action::Quit => {
            state.pending_requests.clear();
            state.pending_attach = None;
            state.attach_in_flight = false;
            state.invalidate_bash_run();
            state.quit = true;
        }
        Action::SwitchSession { attach } => switch_session(attach, state, client, bg_tx),
        Action::SendSequence { requests } => send_sequence(requests, state, client, bg_tx),
        Action::ContinuePromptBatch { id } => continue_prompt_batch(id, state, client, bg_tx),
        Action::Send(req) => send_request(req, state, client, bg_tx, global_refresh),
        Action::RunBashInteractive { command } => {
            log::info!("bash-mode interactive start: {command}");
            let shell = crate::bash::user_shell();
            let cwd = state.cwd();
            let mouse = state.config.ui.mouse;
            let status = crate::terminal::run_suspended(mouse, || {
                std::process::Command::new(&shell)
                    .arg("-c")
                    .arg(&command)
                    .current_dir(&cwd)
                    .status()
            });
            let (exit_code, err_msg) = match status {
                Ok(Ok(s)) => {
                    log::debug!("bash-mode interactive exited with code {:?}", s.code());
                    (s.code(), None)
                }
                Ok(Err(e)) => {
                    log::warn!("bash-mode interactive failed to execute: {e}");
                    (Some(1), Some(e.to_string()))
                }
                Err(e) => {
                    log::warn!("terminal error during interactive bash command: {e}");
                    (Some(1), Some(e.to_string()))
                }
            };
            let now = now_ms();
            let output = err_msg.unwrap_or_default();
            state.transcript.push_cell(Cell {
                id: now,
                kind: CellKind::Item(TranscriptKind::BashCommand {
                    command,
                    output,
                    exit_code,
                    truncated: false,
                }),
                version: 0,
                ts_ms: now,
                stats: None,
                perm_explain: None,
            });
            state.transcript.scroll_to_bottom();
            state.dirty = true;
        }
        Action::RunBashCaptured { command } => {
            if state.awaiting_snapshot {
                return;
            }
            log::info!("bash-mode captured start: {command}");
            let context = state.session_context();
            let client_context = client.event_context();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            let run_id = state.start_bash_run(cancel_tx);
            state.dirty = true;
            let tx = bg_tx.clone();
            let cwd = state.cwd();
            tokio::spawn(async move {
                let result = crate::bash::run_captured(&command, &cwd, cancel_rx).await;
                let _ = tx.send(BgResponse::BashCompleted {
                    run_id,
                    context,
                    client_context,
                    result,
                });
            });
        }
        Action::GitLog { after } => {
            if state.awaiting_snapshot {
                queue_pending_request(state, Request::GitLog { after });
                return;
            }
            if state.meta.is_none() {
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                queue_pending_request(state, Request::GitLog { after });
                return;
            }
            state.git.begin_log(after.as_deref());
            state.lists.mark_sent(crate::cache::ListKind::GitLog);
            state.dirty = true;
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(client_context.clone(), Request::GitLog { after })
                    .await;
                let _ = tx.send(BgResponse::Reply {
                    scope: ReplyScope::Session {
                        context,
                        client_context,
                    },
                    kind: Some(crate::cache::ListKind::GitLog),
                    global_refresh: false,
                    res,
                });
            });
        }
        Action::GitShow { hash } => {
            if state.awaiting_snapshot {
                state.git.show_in_flight = None;
                return;
            }
            if state.meta.is_none() {
                state.git.show_in_flight = None;
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                state.git.show_in_flight = None;
                return;
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(
                        client_context.clone(),
                        Request::GitShow { hash: hash.clone() },
                    )
                    .await;
                let _ = tx.send(BgResponse::GitShowReady {
                    context,
                    client_context,
                    hash,
                    res,
                });
            });
        }
        Action::GitDiff { hash } => {
            if state.meta.is_none() || state.awaiting_snapshot {
                queue_pending_request(state, Request::GitDiff { hash });
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                queue_pending_request(state, Request::GitDiff { hash });
                return;
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(
                        client_context.clone(),
                        Request::GitDiff { hash: hash.clone() },
                    )
                    .await;
                let _ = tx.send(BgResponse::GitDiffReady {
                    context,
                    client_context,
                    hash,
                    res,
                });
            });
        }
        Action::LoadPanel => load_panel(state, client, bg_tx),
        Action::LoadHistory => load_history(state, client, bg_tx),
    }
}

pub(super) fn queue_pending_request(state: &mut AppState, req: Request) {
    if state.pending_requests.contains(&req) {
        log::debug!("pending request already queued: {}", req.name());
        return;
    }
    if state.pending_requests.len() >= PENDING_REQUESTS_CAP {
        let dropped = state.pending_requests.pop_front();
        log::warn!(
            "pending request queue full ({PENDING_REQUESTS_CAP}); dropped oldest: {dropped:?}"
        );
    }
    state.pending_requests.push_back(req);
}

/// A request the daemon cannot serve yet: the client holds no session for it,
/// so it waits for the next attach instead of vanishing. Losing it silently
/// strands whatever the user is blocked on — a permission reply, a question
/// answer, a cron edit.
pub(super) fn defer_unbound_request(state: &mut AppState, req: Request) {
    let is_user_message = matches!(&req, Request::UserMessage { .. });
    let is_resync = matches!(&req, Request::GetSnapshot);
    if is_user_message {
        queue_user_message_request(state, req);
    } else if is_resync {
        state.release_resync_barrier(false, Instant::now());
    } else {
        queue_pending_request(state, req);
    }
}
