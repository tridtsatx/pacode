//! Sending prompts: the prompt gate, the prompt queue and pending batches,
//! `[Image N]` token resolution, the detach-then-send batch steps, and
//! folding the user-message replies.

use std::sync::Arc;
use std::time::Instant;

use pacode_client::{Client, ClientEventContext};
use pacode_types::state::ToastLevel;
use pacode_types::{Reply, Request};
use tokio::sync::mpsc;

use super::bg_response::BgResponse;
use super::reply::{handle_reply, session_reply_is_current};
use crate::keys::Action;
use crate::state::{AppState, PromptBatch, UserMessageInFlight};

pub(super) fn prompt_gate_open(state: &AppState) -> bool {
    state.meta.is_some()
        && matches!(state.connection, crate::state::Connection::Connected)
        && !state.awaiting_snapshot
        && !state.resync_want
        && !state.turn_active
        && state.user_msgs_in_flight == 0
        && !state.turn_boundary_pending
}

/// A terminal hands over a bracketed paste of any size in one event, and every
/// step downstream — the prompt's normalize pass, vim's `Vec<char>` — walks
/// the whole payload. Cut it at the prompt's own cap here, so the loop never
/// touches more than the cap and the loss is reported instead of silent.
pub(super) fn bound_paste(state: &mut AppState, text: String, now: Instant) -> String {
    let cap = crate::paste::PASTE_MAX_CHARS;
    if text.chars().nth(cap).is_none() {
        return text;
    }
    log::warn!(
        "pasted text truncated to {cap} chars ({} bytes)",
        text.len()
    );
    state.push_toast(
        ToastLevel::Warn,
        format!("Pasted text truncated to {cap} chars"),
        Some(format!("{} bytes received", text.len())),
        now,
    );
    text.chars().take(cap).collect()
}

pub(super) fn resolve_request(state: &mut AppState, request: Request) -> Request {
    match request {
        Request::UserMessage {
            text,
            images,
            message_id,
        } => {
            let (text, resolved) =
                crate::state::images::resolve_image_tokens(&state.pasted_images, &text);
            if !resolved.is_empty() {
                state.pasted_images.mark_submitted(&text);
            }
            let mut images = images;
            images.extend(resolved);
            Request::UserMessage {
                text,
                images,
                message_id,
            }
        }
        other => other,
    }
}

pub(super) fn queue_user_message_request(state: &mut AppState, request: Request) {
    let Request::UserMessage { text, .. } = request else {
        return;
    };
    if state.input.queue_push(text.clone()) {
        state.dirty = true;
        return;
    }
    state.input.restore_text(text);
    state.push_toast(
        ToastLevel::Warn,
        "Prompt queue full".to_string(),
        Some(format!(
            "Maximum {} prompts queued",
            crate::state::input::PROMPT_QUEUE_CAP
        )),
        Instant::now(),
    );
}

pub(super) fn queue_sequence(state: &mut AppState, requests: Vec<Request>, text: String) {
    if state.session.queue_sequence(PromptBatch { requests }) {
        state.dirty = true;
        return;
    }
    state.input.restore_text(text);
    state.push_toast(
        ToastLevel::Warn,
        "Prompt queue full".to_string(),
        Some(format!(
            "Maximum {} prompts queued",
            crate::state::input::PROMPT_QUEUE_CAP
        )),
        Instant::now(),
    );
}

fn fail_prompt_batch(state: &mut AppState, id: u64) {
    if state.session.finish_user_message(id, true).is_none() {
        return;
    }
    state.push_toast(
        ToastLevel::Warn,
        "prompt not sent".into(),
        Some("press Enter to retry".into()),
        Instant::now(),
    );
}

fn spawn_prompt_batch_step(
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    ticket: UserMessageInFlight,
    request: Request,
    client_context: ClientEventContext,
) {
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    tokio::spawn(async move {
        let result = cl
            .request_sequence(client_context.clone(), [request.clone()])
            .await
            .and_then(|mut replies| {
                replies
                    .pop()
                    .ok_or(pacode_client::ClientError::UnexpectedReply {
                        request: request.name().to_string(),
                        reply: "empty sequence reply".into(),
                    })
            });
        let _ = tx.send(BgResponse::UserMessageStepDone {
            ticket,
            client_context,
            request,
            result,
        });
    });
}

/// `Action::SendSequence`: a detach-then-send batch, queued while the
/// prompt gate is closed.
pub(super) fn send_sequence(
    requests: Vec<Request>,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let user_text = match requests.as_slice() {
        [Request::DetachTurn, Request::UserMessage { text, .. }] => text.clone(),
        _ => return,
    };
    if state.meta.is_none()
        || !matches!(state.connection, crate::state::Connection::Connected)
        || state.awaiting_snapshot
        || state.resync_want
        || state.user_msgs_in_flight > 0
        || state.turn_boundary_pending
        || state.initial_prompt.is_some()
        || state.has_retry_request()
    {
        queue_sequence(state, requests, user_text);
        return;
    }
    let client_context = client.event_context();
    if client_context.session.is_none() {
        queue_sequence(state, requests, user_text);
        return;
    }
    let requests = requests
        .into_iter()
        .map(|request| resolve_request(state, request))
        .collect::<Vec<_>>();
    let context = state.session_context();
    let Some(ticket) = state
        .session
        .begin_user_message_sequence(context, PromptBatch { requests })
    else {
        state.input.restore_text(user_text);
        return;
    };
    let Some(request) = ticket.sequence.requests.first().cloned() else {
        fail_prompt_batch(state, ticket.id);
        return;
    };
    spawn_prompt_batch_step(client, bg_tx, ticket, request, client_context);
}

/// `Action::ContinuePromptBatch`: send the next step of a batch in flight.
pub(super) fn continue_prompt_batch(
    id: u64,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let Some(ticket) = state.session.user_message_ticket(id) else {
        return;
    };
    if state.meta.is_none()
        || !matches!(state.connection, crate::state::Connection::Connected)
        || state.awaiting_snapshot
        || state.resync_want
    {
        fail_prompt_batch(state, id);
        return;
    }
    let client_context = client.event_context();
    if client_context.session.is_none() {
        fail_prompt_batch(state, id);
        return;
    }
    let Some(request) = ticket.sequence.requests.first().cloned() else {
        fail_prompt_batch(state, id);
        return;
    };
    spawn_prompt_batch_step(client, bg_tx, ticket, request, client_context);
}

/// `BgResponse::UserMessageDone`.
pub(super) fn fold_user_message_done(
    state: &mut AppState,
    ticket: UserMessageInFlight,
    client_context: ClientEventContext,
    result: Result<Reply, pacode_client::ClientError>,
) -> Vec<Action> {
    if !session_reply_is_current(state, &ticket.context, &client_context) {
        return Vec::new();
    }
    let failed = !matches!(&result, Ok(Reply::Ok));
    if state
        .session
        .finish_user_message(ticket.id, failed)
        .is_none()
    {
        return Vec::new();
    }
    let mut actions = handle_reply(result, state, Some(&ticket.context));
    if failed {
        state.push_toast(
            ToastLevel::Warn,
            "prompt not sent".into(),
            Some("press Enter to retry".into()),
            Instant::now(),
        );
    }
    if !failed
        && !state.turn_boundary_pending
        && let Some(req) = state.drain_prompt_queue()
    {
        actions.push(Action::Send(req));
    }
    actions
}

/// `BgResponse::UserMessageStepDone`.
pub(super) fn fold_user_message_step_done(
    state: &mut AppState,
    ticket: UserMessageInFlight,
    client_context: ClientEventContext,
    request: Request,
    result: Result<Reply, pacode_client::ClientError>,
) -> Vec<Action> {
    if !session_reply_is_current(state, &ticket.context, &client_context) {
        return Vec::new();
    }
    let succeeded = matches!(
        (&request, &result),
        (Request::DetachTurn, Ok(Reply::TurnDetached { .. }))
            | (Request::UserMessage { .. }, Ok(Reply::Ok))
    );
    if succeeded {
        let Some(remaining) = state
            .session
            .complete_user_message_step(ticket.id, &request)
        else {
            return Vec::new();
        };
        if !remaining.requests.is_empty() {
            return vec![Action::ContinuePromptBatch { id: ticket.id }];
        }
        if state
            .session
            .finish_user_message(ticket.id, false)
            .is_none()
        {
            return Vec::new();
        }
        let mut actions = handle_reply(result, state, Some(&ticket.context));
        if !state.turn_boundary_pending
            && let Some(req) = state.drain_prompt_queue()
        {
            actions.push(Action::Send(req));
        }
        return actions;
    }
    if state.session.finish_user_message(ticket.id, true).is_none() {
        return Vec::new();
    }
    let actions = match result {
        Ok(reply) => handle_reply(
            Err(pacode_client::ClientError::UnexpectedReply {
                request: request.name().into(),
                reply: format!("{reply:?}"),
            }),
            state,
            Some(&ticket.context),
        ),
        Err(err) => handle_reply(Err(err), state, Some(&ticket.context)),
    };
    state.push_toast(
        ToastLevel::Warn,
        "prompt not sent".into(),
        Some("press Enter to retry".into()),
        Instant::now(),
    );
    actions
}
