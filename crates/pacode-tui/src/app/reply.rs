//! Daemon requests and their replies: which requests need a session, the
//! scope a reply must still match to be applied, sending a request, and
//! folding a reply into state.

use std::sync::Arc;

use pacode_client::{Client, ClientEventContext};
use pacode_types::state::ToastLevel;
use pacode_types::time::now_ms;
use pacode_types::{PluginCommandOutcome, Reply, Request, TranscriptKind};
use tokio::sync::mpsc;

use super::attach::apply_scoped_snapshot;
use super::bg_response::BgResponse;
use super::dispatch::{defer_unbound_request, queue_pending_request};
use super::prompt::{prompt_gate_open, queue_user_message_request, resolve_request};
use crate::keys::Action;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, SessionContext};

pub(super) enum ReplyScope {
    Global {
        context: crate::state::UiContext,
    },
    Ui {
        context: crate::state::UiContext,
    },
    Session {
        context: SessionContext,
        client_context: ClientEventContext,
    },
}

/// `Action::Send`: one request to the daemon. A user message the gate
/// holds back is queued, a session-bound request waits for an attach, and
/// the reply comes back as a `BgResponse` scoped to the state it was sent
/// from.
pub(super) fn send_request(
    req: Request,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    global_refresh: bool,
) {
    let is_user_message = matches!(&req, Request::UserMessage { .. });
    let is_resync = matches!(&req, Request::GetSnapshot);
    let session_bound = request_needs_session(&req);
    if is_user_message && (state.initial_prompt.is_some() || !prompt_gate_open(state)) {
        queue_user_message_request(state, req);
        return;
    }
    if session_bound && (state.meta.is_none() || (state.awaiting_snapshot && !is_resync)) {
        queue_pending_request(state, req);
        return;
    }
    let client_context = client.event_context();
    if session_bound && client_context.session.is_none() {
        defer_unbound_request(state, req);
        return;
    }
    let mut req = resolve_request(state, req);
    let kind = crate::cache::ListKind::for_request(&req);
    if let Some(kind) = kind {
        state.lists.mark_sent(kind);
    }
    match &req {
        Request::InstallPlugin { .. } | Request::UninstallPlugin { .. } => {
            state.invalidate_cache(crate::cache::ListKind::Plugins);
            state.invalidate_cache(crate::cache::ListKind::Commands);
        }
        Request::SetMcpServerEnabled { .. } | Request::RestartMcpServer { .. } => {
            state.invalidate_cache(crate::cache::ListKind::McpServers);
            state.invalidate_cache(crate::cache::ListKind::Commands);
        }
        // A git request that waited in the pending queue arrives here
        // instead of through `Action::GitLog`, so it takes the same
        // loading flags — a page must not be applied as a first page.
        Request::GitLog { after } => state.git.begin_log(after.as_deref()),
        _ => {}
    }
    let session_context = state.session_context();
    let scope = (!is_user_message).then(|| {
        reply_scope(
            &req,
            session_context.clone(),
            client_context.clone(),
            state.ui_context(),
        )
    });
    let ticket = match &req {
        Request::UserMessage { .. } => Some(
            state
                .session
                .begin_user_message(session_context, req.clone()),
        ),
        _ => None,
    };
    if let Some(ticket) = ticket.as_ref() {
        req = ticket.request.clone();
    }
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    tokio::spawn(async move {
        let res = if session_bound {
            cl.request_session_for(client_context.clone(), req).await
        } else {
            cl.request(req).await
        };
        if let Some(ticket) = ticket {
            let _ = tx.send(BgResponse::UserMessageDone {
                ticket,
                client_context,
                result: res,
            });
        } else if is_resync {
            if let Some(ReplyScope::Session {
                context,
                client_context,
            }) = scope
            {
                let _ = tx.send(BgResponse::ResyncDone {
                    context,
                    client_context,
                    result: res,
                });
            }
        } else if let Some(scope) = scope {
            let _ = tx.send(BgResponse::Reply {
                scope,
                kind,
                global_refresh,
                res,
            });
        }
    });
}

/// Whether the daemon needs an attached session to answer this request:
/// everything `Core::handle_global` serves (plus the connection-level
/// requests the daemon's serve loop answers itself) may fly before the first
/// attach lands; the rest waits in `state.pending_requests`.
fn request_needs_session(req: &Request) -> bool {
    match req {
        // Connection-level — `connection::serve_connection` answers these.
        Request::Hello(_)
        | Request::Attach(_)
        | Request::Detach
        | Request::Shutdown { .. }
        | Request::Ping
        // `Core::handle_global` — no session needed.
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
        | Request::SetAuthAccount { .. } => false,
        // Session-scoped: the report is this session's, so it waits for an
        // attach rather than flying early and reading an empty ledger.
        Request::Analyze => true,
        Request::UserMessage { .. }
        | Request::Interrupt
        | Request::InterruptAll
        | Request::CloseSession
        | Request::PermissionReply { .. }
        | Request::ExplainPermission { .. }
        | Request::SetModel(_)
        | Request::SetEffort(_)
        | Request::SetMode(_)
        | Request::SetZdr(_)
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
        | Request::GitDiff { .. } => true,
    }
}

fn request_needs_reply_ui_context(req: &Request) -> bool {
    matches!(
        req,
        Request::GetMcpPrompt { .. } | Request::RunPluginCommand { .. }
    )
}

fn reply_scope(
    req: &Request,
    session_context: SessionContext,
    client_context: ClientEventContext,
    ui_context: crate::state::UiContext,
) -> ReplyScope {
    if request_needs_session(req) {
        return ReplyScope::Session {
            context: session_context,
            client_context,
        };
    }
    if request_needs_reply_ui_context(req) {
        return ReplyScope::Ui {
            context: ui_context,
        };
    }
    ReplyScope::Global {
        context: ui_context,
    }
}

pub(super) fn session_reply_is_current(
    state: &AppState,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> bool {
    state.context_is_current(context) && state.event_context_is_current(client_context)
}

pub(super) fn deferred_reply_is_current(
    state: &AppState,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> bool {
    state.context_is_current(context)
        && (client_context.session.is_none() || state.event_context_is_current(client_context))
}

pub(super) fn reply_scope_is_current(state: &AppState, scope: &ReplyScope) -> bool {
    match scope {
        ReplyScope::Global { context } | ReplyScope::Ui { context } => {
            state.ui_context_is_current(context)
        }
        ReplyScope::Session {
            context,
            client_context,
        } => session_reply_is_current(state, context, client_context),
    }
}

/// Fold one daemon reply into state. List replies replace their section
/// wholesale (§8.2) and — once state moved — rewrite their cache file.
pub(super) fn handle_reply(
    res: Result<Reply, pacode_client::ClientError>,
    state: &mut AppState,
    context: Option<&SessionContext>,
) -> Vec<Action> {
    use crate::cache::ListKind;
    match res {
        Ok(Reply::Error { message }) => {
            let now = now_ms();
            state.transcript.push_cell(Cell {
                id: now,
                kind: CellKind::Item(TranscriptKind::Notice {
                    level: ToastLevel::Error,
                    text: message,
                }),
                version: 0,
                ts_ms: now,
                stats: None,
                perm_explain: None,
            });
            state.dirty = true;
            vec![]
        }
        Ok(Reply::Analyze { report }) => {
            // Drawn from this copy, so scrolling never re-asks and a client that
            // reconnects mid-read keeps what it was reading.
            state.analyze_report = Some(*report);
            state.open_analyze();
            state.dirty = true;
            vec![]
        }
        Ok(Reply::Models { models }) => {
            state.models = models;
            state.dirty = true;
            crate::cache::write_after_reply(state, ListKind::Models);
            vec![]
        }
        Ok(Reply::AuthStatus { providers }) => {
            state.auth_providers = providers;
            state.dirty = true;
            vec![]
        }
        Ok(Reply::Sessions { sessions }) => {
            state.sessions = sessions;
            state.dirty = true;
            crate::cache::write_after_reply(state, ListKind::Sessions);
            vec![]
        }
        Ok(Reply::McpServers { servers }) => {
            state.mcp_servers = servers;
            // A server removal must drop its prompt commands — rebuild, not
            // merge (§8.2 wholesale replacement).
            crate::commands::rebuild_dynamic_commands(&state.plugins, &state.mcp_servers);
            state.dirty = true;
            crate::cache::write_after_reply(state, ListKind::McpServers);
            vec![]
        }
        Ok(Reply::McpPrompt { text }) => {
            state.input.insert_str(&text);
            state.dirty = true;
            vec![]
        }
        // The reply's `stale` flag belonged to the legacy overlay; the menu
        // reads the `offline copy` badge from `state.lists` instead (§8.2).
        Ok(Reply::MarketplacePlugins {
            plugins: listing, ..
        }) => {
            state.marketplace = listing;
            state.dirty = true;
            crate::cache::write_after_reply(state, ListKind::Marketplace);
            vec![]
        }
        Ok(Reply::GitLog {
            commits,
            has_more,
            status,
        }) => {
            let first_page = !state.git.loading_more;
            state.git.apply_log(commits, has_more, status);
            state.dirty = true;
            if first_page {
                crate::cache::write_after_reply(state, ListKind::GitLog);
            }
            vec![]
        }
        Ok(Reply::GitShow { hash, stat }) => {
            state.git.apply_show(hash, stat);
            state.dirty = true;
            vec![]
        }
        Ok(Reply::GitDiff {
            hash,
            patch,
            truncated,
        }) => {
            state.git.apply_diff(hash, patch, truncated);
            state.dirty = true;
            vec![]
        }
        Ok(Reply::Plugins { plugins }) => {
            crate::commands::rebuild_dynamic_commands(&plugins, &state.mcp_servers);
            state.plugins = plugins;
            state.dirty = true;
            crate::cache::write_after_reply(state, ListKind::Plugins);
            vec![]
        }
        Ok(Reply::PluginCommand(outcome)) => match outcome {
            PluginCommandOutcome::InsertText { text } => {
                state.input.insert_str(&text);
                state.dirty = true;
                vec![]
            }
            PluginCommandOutcome::SendPrompt { text } => {
                vec![Action::Send(Request::UserMessage {
                    text,
                    images: Vec::new(),
                    message_id: None,
                })]
            }
            PluginCommandOutcome::Nothing => vec![],
        },
        Ok(Reply::Attached(snapshot)) | Ok(Reply::Snapshot(snapshot)) => context
            .map(|context| apply_scoped_snapshot(state, snapshot, context))
            .unwrap_or_default(),
        Ok(_) => vec![],
        Err(err) => {
            let now = now_ms();
            state.transcript.push_cell(Cell {
                id: now,
                kind: CellKind::Item(TranscriptKind::Notice {
                    level: ToastLevel::Error,
                    text: err.to_string(),
                }),
                version: 0,
                ts_ms: now,
                stats: None,
                perm_explain: None,
            });
            state.dirty = true;
            vec![]
        }
    }
}
