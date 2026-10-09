//! Results of background work — daemon replies and off-thread jobs — and
//! folding each one into state. Every result carries the contexts it was
//! started under and is dropped when they are no longer current.

use pacode_client::ClientEventContext;
use pacode_types::state::ToastLevel;
use pacode_types::time::now_ms;
use pacode_types::{Attach, Reply, Request, TranscriptKind};

use super::attach::fold_attach_finished;
use super::panel::{fold_load_history_reply, fold_load_panel_reply};
use super::prompt::{fold_user_message_done, fold_user_message_step_done};
use super::reply::{
    ReplyScope, deferred_reply_is_current, handle_reply, reply_scope_is_current,
    session_reply_is_current,
};
use super::resync::fold_resync_done;
use crate::keys::Action;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, PanelTarget, SessionContext, UserMessageInFlight};

pub(super) enum BgResponse {
    AttachFinished {
        context: SessionContext,
        token: u64,
        request_client_context: ClientEventContext,
        client_context: ClientEventContext,
        attach: Attach,
        result: Result<pacode_types::SessionSnapshot, pacode_client::ClientError>,
    },
    Reply {
        scope: ReplyScope,
        kind: Option<crate::cache::ListKind>,
        global_refresh: bool,
        res: Result<Reply, pacode_client::ClientError>,
    },
    ResyncDone {
        context: SessionContext,
        client_context: ClientEventContext,
        result: Result<Reply, pacode_client::ClientError>,
    },
    UserMessageDone {
        ticket: UserMessageInFlight,
        client_context: ClientEventContext,
        result: Result<Reply, pacode_client::ClientError>,
    },
    UserMessageStepDone {
        ticket: UserMessageInFlight,
        client_context: ClientEventContext,
        request: Request,
        result: Result<Reply, pacode_client::ClientError>,
    },
    LoadPanelReply {
        context: SessionContext,
        client_context: ClientEventContext,
        target: PanelTarget,
        token: u64,
        result: Result<Reply, pacode_client::ClientError>,
    },
    LoadHistoryReply {
        context: SessionContext,
        client_context: ClientEventContext,
        before_seq: Option<u64>,
        result: Result<Reply, pacode_client::ClientError>,
    },
    BashCompleted {
        run_id: u64,
        context: SessionContext,
        client_context: ClientEventContext,
        result: Result<crate::bash::BashOutput, String>,
    },
    AtCompleted {
        context: SessionContext,
        client_context: ClientEventContext,
        key: crate::state::input::AtKey,
        candidates: Vec<crate::at_complete::AtCandidate>,
    },
    BashCompletionDone {
        context: SessionContext,
        client_context: ClientEventContext,
        key: crate::state::input::BashKey,
        candidates: Vec<crate::bash::complete::Candidate>,
    },
    FilePreviewReady {
        context: SessionContext,
        client_context: ClientEventContext,
        key: crate::state::files::PreviewKey,
        resolved: std::path::PathBuf,
        text: Option<crate::state::files::TextPreview>,
    },
    FileStatusesReady {
        context: SessionContext,
        client_context: ClientEventContext,
        statuses: std::collections::BTreeMap<String, pacode_vcs::FileStatus>,
        at_version: u64,
    },
    FileDiffReady {
        context: SessionContext,
        client_context: ClientEventContext,
        path: String,
        diff: pacode_vcs::FileDiff,
    },
    GitShowReady {
        context: SessionContext,
        client_context: ClientEventContext,
        hash: String,
        res: Result<Reply, pacode_client::ClientError>,
    },
    GitDiffReady {
        context: SessionContext,
        client_context: ClientEventContext,
        hash: String,
        res: Result<Reply, pacode_client::ClientError>,
    },
    TaskTailReady {
        context: SessionContext,
        client_context: ClientEventContext,
        tail: crate::state::rail::TaskTail,
    },
    ThumbReady {
        context: SessionContext,
        client_context: ClientEventContext,
        token: usize,
        cell_w: u16,
        cell_h: u16,
        preview: Option<pacode_image::Preview>,
    },
    CopyDone {
        context: SessionContext,
        client_context: ClientEventContext,
        auto: bool,
        chars: usize,
    },
}

pub(super) fn handle_bg_response(res: BgResponse, state: &mut AppState) -> Vec<Action> {
    dedup_actions(fold_bg_response(res, state))
}

/// Actions dispatched as one batch: the same request twice in a batch is one
/// request, not two. Post-attach flushes drain the pending queue and then ask
/// for the same lists again, and a plugin command can repeat its own send.
pub(super) fn dedup_actions(actions: Vec<Action>) -> Vec<Action> {
    let mut seen: Vec<Request> = Vec::new();
    let mut out: Vec<Action> = Vec::with_capacity(actions.len());
    for action in actions {
        if let Action::Send(ref request) = action {
            if seen.contains(request) {
                log::debug!("duplicate request in one batch: {}", request.name());
                continue;
            }
            seen.push(request.clone());
        }
        out.push(action);
    }
    out
}

fn fold_bg_response(res: BgResponse, state: &mut AppState) -> Vec<Action> {
    match res {
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context,
            client_context,
            attach,
            result,
        } => fold_attach_finished(
            state,
            context,
            token,
            request_client_context,
            client_context,
            attach,
            result,
        ),
        BgResponse::Reply {
            scope,
            kind,
            global_refresh,
            res,
        } => {
            if !reply_scope_is_current(state, &scope) {
                return Vec::new();
            }
            if global_refresh {
                state.finish_global_refresh_request();
            }
            if let Some(kind) = kind {
                let ok = matches!(&res, Ok(r) if !matches!(r, Reply::Error { .. }));
                state.lists.on_reply(kind, ok);
                if !ok && matches!(kind, crate::cache::ListKind::GitLog) {
                    state.git.fail_log();
                    state.dirty = true;
                }
            }
            let context = match &scope {
                ReplyScope::Session { context, .. } => Some(context),
                ReplyScope::Global { .. } | ReplyScope::Ui { .. } => None,
            };
            handle_reply(res, state, context)
        }
        BgResponse::ResyncDone {
            context,
            client_context,
            result,
        } => fold_resync_done(state, context, client_context, result),
        BgResponse::UserMessageDone {
            ticket,
            client_context,
            result,
        } => fold_user_message_done(state, ticket, client_context, result),
        BgResponse::UserMessageStepDone {
            ticket,
            client_context,
            request,
            result,
        } => fold_user_message_step_done(state, ticket, client_context, request, result),
        BgResponse::AtCompleted {
            context,
            client_context,
            key,
            candidates,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            state.input.at_comp.finish_scan(key, candidates);
            state.dirty = true;
            vec![]
        }
        BgResponse::BashCompletionDone {
            context,
            client_context,
            key,
            candidates,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            state.input.bash_comp.finish_scan(key, candidates);
            state.dirty = true;
            vec![]
        }
        BgResponse::FilePreviewReady {
            context,
            client_context,
            key,
            resolved,
            text,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context)
                || state.files.preview_in_flight.as_ref() != Some(&key)
            {
                return Vec::new();
            }
            state.files.preview_in_flight = None;
            state.files.cached_preview = Some(crate::state::files::CachedPreview {
                key,
                resolved,
                text,
            });
            state.dirty = true;
            vec![]
        }
        BgResponse::FileStatusesReady {
            context,
            client_context,
            statuses,
            at_version,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            if at_version != state.files.rows_version {
                state.files.statuses_in_flight = false;
                return Vec::new();
            }
            state.files.apply_statuses(statuses, at_version);
            state.dirty = true;
            vec![]
        }
        BgResponse::GitShowReady {
            context,
            client_context,
            hash,
            res,
        } => {
            if !session_reply_is_current(state, &context, &client_context)
                || state.git.show_in_flight.as_deref() != Some(hash.as_str())
            {
                return Vec::new();
            }
            match res {
                Ok(Reply::GitShow { hash: h, stat }) => state.git.apply_show(h, stat),
                Ok(Reply::Error { message }) => state.git.fail_show(hash, message),
                Ok(_) => state.git.fail_show(hash, "unexpected reply".to_string()),
                Err(e) => state.git.fail_show(hash, e.to_string()),
            }
            state.dirty = true;
            vec![]
        }
        BgResponse::GitDiffReady {
            context,
            client_context,
            hash,
            res,
        } => {
            if !session_reply_is_current(state, &context, &client_context)
                || !matches!(
                    &state.panel.target,
                    Some(PanelTarget::CommitDiff { hash: current }) if current == &hash
                )
            {
                return Vec::new();
            }
            match res {
                Ok(Reply::GitDiff {
                    hash: h,
                    patch,
                    truncated,
                }) => state.git.apply_diff(h, patch, truncated),
                Ok(Reply::Error { message }) => state.git.fail_diff(hash, message),
                Ok(_) => state.git.fail_diff(hash, "unexpected reply".to_string()),
                Err(e) => state.git.fail_diff(hash, e.to_string()),
            }
            state.dirty = true;
            vec![]
        }
        BgResponse::TaskTailReady {
            context,
            client_context,
            tail,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context)
                || state.rail.task_tail_in_flight.as_ref() != Some(&tail.key())
            {
                return Vec::new();
            }
            state.rail.task_tail_in_flight = None;
            state.rail.task_tail = Some(tail);
            state.dirty = true;
            vec![]
        }
        BgResponse::FileDiffReady {
            context,
            client_context,
            path,
            diff,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            if state.files.diff_in_flight.as_deref() != Some(path.as_str()) {
                return Vec::new();
            }
            state.files.diff_in_flight = None;
            if !matches!(
                &state.panel.target,
                Some(PanelTarget::FileDiff { path: p }) if *p == path
            ) {
                // The panel moved off the file while the read ran: release the
                // marker, or re-entering that file's diff would never re-arm.
                return Vec::new();
            }
            state.panel.diff = Some(crate::state::PanelDiff {
                path,
                patch: diff.patch,
                status: diff.status,
                truncated: diff.truncated,
                note: diff.note,
                rendered: None,
            });
            state.dirty = true;
            vec![]
        }
        BgResponse::ThumbReady {
            context,
            client_context,
            token,
            cell_w,
            cell_h,
            preview,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            if !state.image_strip.in_flight.remove(&(token, cell_w, cell_h)) {
                return Vec::new();
            }
            state.image_strip.thumbs.insert(
                token,
                crate::state::images::CachedThumb {
                    cell_w,
                    cell_h,
                    preview,
                },
            );
            state.dirty = true;
            vec![]
        }
        BgResponse::CopyDone {
            context,
            client_context,
            auto,
            chars,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            state.on_copy_done(auto, chars);
            vec![]
        }
        BgResponse::LoadPanelReply {
            context,
            client_context,
            target,
            token,
            result,
        } => fold_load_panel_reply(state, context, client_context, target, token, result),
        BgResponse::LoadHistoryReply {
            context,
            client_context,
            before_seq,
            result,
        } => fold_load_history_reply(state, context, client_context, before_seq, result),
        BgResponse::BashCompleted {
            run_id,
            context,
            client_context,
            result,
        } => {
            if !deferred_reply_is_current(state, &context, &client_context)
                || !state.finish_bash_run(run_id)
            {
                return Vec::new();
            }
            state.dirty = true;
            match result {
                Ok(out) => {
                    log::debug!(
                        "bash-mode captured completed: exit_code={:?}, truncated={}",
                        out.exit_code,
                        out.truncated
                    );
                    let now = now_ms();
                    state.transcript.push_cell(Cell {
                        id: now,
                        kind: CellKind::Item(TranscriptKind::BashCommand {
                            command: out.command,
                            output: out.output,
                            exit_code: out.exit_code,
                            truncated: out.truncated,
                        }),
                        version: 0,
                        ts_ms: now,
                        stats: None,
                        perm_explain: None,
                    });
                    state.transcript.scroll_to_bottom();
                }
                Err(err) => {
                    log::warn!("bash-mode captured error: {err}");
                    let now = now_ms();
                    state.transcript.push_cell(Cell {
                        id: now,
                        kind: CellKind::Item(TranscriptKind::Notice {
                            level: ToastLevel::Error,
                            text: err,
                        }),
                        version: 0,
                        ts_ms: now,
                        stats: None,
                        perm_explain: None,
                    });
                    state.transcript.scroll_to_bottom();
                }
            }
            vec![]
        }
    }
}
