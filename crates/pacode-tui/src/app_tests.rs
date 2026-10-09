use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_client::ClientError;
use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{AgentKind, AgentStatus, Mode, Plan, UsageTotals};
use pacode_types::{
    AgentInfo, Attach, Config, PluginCommandOutcome, Request, SessionId, SessionMeta,
    SessionSnapshot, TranscriptItem,
};

use super::*;
use crate::cache::ListKind;
use crate::state::files::PreviewKey;

fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: None,
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::Medium,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

fn snapshot(id: &str) -> SessionSnapshot {
    SessionSnapshot {
        meta: meta(id),
        agents: Vec::new(),
        plan: Plan::default(),
        tasks: Vec::new(),
        usage: UsageTotals::default(),
        transcript: Vec::new(),
        has_more_history: false,
        pending_permissions: Vec::new(),
        pending_questions: Vec::new(),
        turn_active: false,
        active_turn: None,
        terminal_turn: None,
        terminal_error: None,
        cron_jobs: Vec::new(),
        monitors: Vec::new(),
        seq: 1,
    }
}

fn main_agent(status: AgentStatus) -> AgentInfo {
    AgentInfo {
        id: pacode_types::AgentId::main(),
        name: "main".into(),
        kind: AgentKind::Main,
        status,
        activity: None,
        started_at_ms: 1,
        finished_at_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        model: ModelRoute::new("prov", "model"),
        effort: Effort::Medium,
        parent: None,
        summary: None,
        error: None,
    }
}

#[test]
fn frame_clock_applies_live_ups_without_an_idle_timer() {
    let mut clock = FrameClock::new(pacode_types::Ups::Fixed(10));
    assert_eq!(clock.interval, Some(Duration::from_millis(100)));
    assert!(clock.set_ups(pacode_types::Ups::Fixed(30)));
    assert_eq!(clock.interval, Some(Duration::from_millis(33)));
    assert!(clock.set_ups(pacode_types::Ups::Auto));
    assert_eq!(clock.interval, None);
    assert!(clock.accept_probe(clock.probe_generation, Some(60)));
    assert_eq!(clock.interval, Some(Duration::from_millis(16)));
    assert!(!clock.accept_probe(clock.probe_generation.wrapping_sub(1), Some(10)));
    assert!(clock.set_ups(pacode_types::Ups::Dynamic));
    assert_eq!(clock.interval, None);
}

#[test]
fn late_attach_reply_cannot_replace_the_current_slot() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let old_attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let old_context = state.begin_attach(old_attach.clone());
    let old_token = state.pending_attach_token().unwrap();
    state.leave_active_slot();
    state.active_slot = 1;
    let new_attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let new_context = state.begin_attach(new_attach.clone());
    let new_token = state.pending_attach_token().unwrap();

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context: old_context,
            token: old_token,
            request_client_context: ClientEventContext::unscoped(),
            client_context: ClientEventContext::unscoped(),
            attach: old_attach,
            result: Ok(snapshot("ses_old")),
        },
        &mut state,
    );
    assert!(actions.is_empty());
    assert!(state.meta.is_none());
    assert_eq!(
        state.pending_attach.as_ref().map(|p| p.context.clone()),
        Some(new_context.clone())
    );

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context: new_context,
            token: new_token,
            request_client_context: ClientEventContext::unscoped(),
            client_context: ClientEventContext::unscoped(),
            attach: new_attach,
            result: Ok(snapshot("ses_new")),
        },
        &mut state,
    );
    assert!(!actions.is_empty());
    assert_eq!(state.meta.as_ref().map(|m| m.id.as_str()), Some("ses_new"));
}

#[test]
fn stale_file_reply_does_not_repopulate_reset_state() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    let context = state.session_context();
    let key = PreviewKey {
        raw: "src/main.rs".into(),
    };
    state.files.preview_in_flight = Some(key.clone());
    state.leave_active_slot();

    let actions = handle_bg_response(
        BgResponse::FilePreviewReady {
            context,
            client_context: ClientEventContext::unscoped(),
            key,
            resolved: PathBuf::from("/tmp/src/main.rs"),
            text: None,
        },
        &mut state,
    );
    assert!(actions.is_empty());
    assert!(state.files.cached_preview.is_none());
    assert!(state.files.preview_in_flight.is_none());
}

#[test]
fn failed_user_message_reply_releases_gate_and_enables_manual_retry() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    let context = state.session_context();
    let ticket = state.session.begin_user_message(
        context,
        Request::UserMessage {
            text: "retry me".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    let request = ticket.request.clone();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 0,
        session: Some(SessionId::new("ses_test")),
    };
    state.event_connection_epoch = 1;
    state.event_session_epoch = 0;
    state.awaiting_snapshot = false;

    let actions = handle_bg_response(
        BgResponse::UserMessageDone {
            ticket,
            client_context,
            result: Err(ClientError::Disconnected),
        },
        &mut state,
    );
    assert!(actions.is_empty());
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(state.has_retry_request());
    assert_eq!(
        state.session.retry_requests.front(),
        Some(&crate::state::PromptBatch::single(request.clone()))
    );
    assert!(!state.transcript.cells.is_empty());
}

#[test]
fn failed_user_message_waits_for_explicit_enter_before_retry() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let ticket = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "retry me".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    state.input.queue_push("later prompt".into());

    let actions = handle_bg_response(
        BgResponse::UserMessageDone {
            ticket,
            client_context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            result: Err(ClientError::Disconnected),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(state.drain_prompt_queue().is_none());
    let actions = crate::keys::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        Instant::now(),
    );
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        Action::Send(Request::UserMessage { text, .. }) if text == "retry me"
    ));
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("later prompt")
    );
}

#[test]
fn full_prompt_queue_restores_deferred_user_text() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    for index in 0..crate::state::input::PROMPT_QUEUE_CAP {
        state.input.queue_push(format!("queued {index}"));
    }

    queue_user_message_request(
        &mut state,
        Request::UserMessage {
            text: "must not disappear".into(),
            images: Vec::new(),
            message_id: None,
        },
    );

    assert_eq!(state.input.text, "must not disappear");
    assert_eq!(state.input.cursor, "must not disappear".chars().count());
    assert_eq!(
        state.input.prompt_queue.len(),
        crate::state::input::PROMPT_QUEUE_CAP
    );
    assert!(
        state
            .toasts
            .iter()
            .any(|toast| toast.title == "Prompt queue full")
    );
}

#[test]
fn stale_bash_completion_does_not_clear_a_new_run() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = state.session_context();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    let (first_cancel, _first_receiver) = tokio::sync::oneshot::channel();
    let first_id = state.start_bash_run(first_cancel);
    let (second_cancel, _second_receiver) = tokio::sync::oneshot::channel();
    let second_id = state.start_bash_run(second_cancel);

    assert!(
        handle_bg_response(
            BgResponse::BashCompleted {
                run_id: first_id,
                context: context.clone(),
                client_context: client_context.clone(),
                result: Ok(crate::bash::BashOutput {
                    command: "old".into(),
                    output: "old".into(),
                    exit_code: Some(0),
                    truncated: false,
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(state.is_running_bash());

    assert!(
        handle_bg_response(
            BgResponse::BashCompleted {
                run_id: second_id,
                context,
                client_context,
                result: Ok(crate::bash::BashOutput {
                    command: "new".into(),
                    output: "new".into(),
                    exit_code: Some(0),
                    truncated: false,
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(!state.is_running_bash());
    assert_eq!(state.transcript.cells.len(), 1);
    assert!(state.transcript.cells.iter().any(|cell| {
        matches!(
            &cell.kind,
            CellKind::Item(TranscriptKind::BashCommand { command, .. }) if command == "new"
        )
    }));
}

#[test]
fn pending_new_attach_rejects_a_stale_reconnect_snapshot() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();

    assert!(!state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 0,
                session: Some(SessionId::new("ses_old")),
            },
            snapshot: snapshot("ses_old"),
        },
        std::time::Instant::now(),
    ));
    assert!(state.meta.is_none());
    assert!(state.pending_attach.is_some());

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context: ClientEventContext::unscoped(),
            client_context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_new")),
            },
            attach,
            result: Ok(snapshot("ses_new")),
        },
        &mut state,
    );
    assert!(!actions.is_empty());
    assert_eq!(state.meta.as_ref().map(|m| m.id.as_str()), Some("ses_new"));
}

#[test]
fn global_reply_survives_snapshot_wait_and_clears_list_health() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let context = state.ui_context();
    state.awaiting_snapshot = true;
    state.lists.mark_sent(ListKind::Models);

    let actions = handle_bg_response(
        BgResponse::Reply {
            scope: ReplyScope::Global { context },
            kind: Some(ListKind::Models),
            global_refresh: false,
            res: Ok(Reply::Error {
                message: "global failure".into(),
            }),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(!state.lists.models.in_flight);
}

#[test]
fn reconnect_cleanup_clears_all_list_loading_markers() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    for kind in [
        ListKind::Models,
        ListKind::Plugins,
        ListKind::Marketplace,
        ListKind::McpServers,
        ListKind::Sessions,
        ListKind::GitLog,
    ] {
        state.lists.mark_sent(kind);
    }

    clear_list_loading(&mut state);

    assert!(!state.lists.models.in_flight);
    assert!(!state.lists.plugins.in_flight);
    assert!(!state.lists.marketplace.in_flight);
    assert!(!state.lists.mcp.in_flight);
    assert!(!state.lists.sessions.in_flight);
    assert!(!state.lists.git.in_flight);
}

#[test]
fn ui_reply_survives_snapshot_wait() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let ui_context = state.ui_context();
    state.awaiting_snapshot = true;

    let actions = handle_bg_response(
        BgResponse::Reply {
            scope: ReplyScope::Ui {
                context: ui_context,
            },
            kind: None,
            global_refresh: false,
            res: Ok(Reply::McpPrompt {
                text: "after reconnect".into(),
            }),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert_eq!(state.input.text, "after reconnect");
}

#[test]
fn pre_attach_mcp_and_plugin_replies_use_ui_context() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();
    let ui_context = state.ui_context();
    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context: ClientEventContext::unscoped(),
            client_context: ClientEventContext::unscoped(),
            attach,
            result: Ok(snapshot("ses_test")),
        },
        &mut state,
    );
    assert!(!actions.is_empty());

    assert!(
        handle_bg_response(
            BgResponse::Reply {
                scope: ReplyScope::Ui {
                    context: ui_context
                },
                kind: None,
                global_refresh: false,
                res: Ok(Reply::McpPrompt { text: "mcp".into() }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(
        handle_bg_response(
            BgResponse::Reply {
                scope: ReplyScope::Ui {
                    context: ui_context
                },
                kind: None,
                global_refresh: false,
                res: Ok(Reply::PluginCommand(PluginCommandOutcome::InsertText {
                    text: " plugin".into(),
                })),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(state.input.text, "mcp plugin");
}

#[test]
fn prompt_ack_waits_for_the_actual_turn_boundary() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.input.queue_push("queued".into());
    let ticket = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "first".into(),
            images: Vec::new(),
            message_id: None,
        },
    );

    let actions = handle_bg_response(
        BgResponse::UserMessageDone {
            ticket,
            client_context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            result: Ok(Reply::Ok),
        },
        &mut state,
    );
    assert!(actions.is_empty());
    assert!(state.turn_boundary_pending);
    assert_eq!(state.input.prompt_queue.len(), 1);

    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: context.clone(),
            seq: 1,
            event: pacode_types::Event::TurnStarted {
                agent: pacode_types::AgentId::main(),
                turn: pacode_types::TurnId::new("turn_1"),
            },
        },
        std::time::Instant::now(),
    );
    assert!(state.turn_active);
    assert!(state.drain_prompt_queue().is_none());

    state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context,
            seq: 2,
            event: pacode_types::Event::TurnEnded {
                agent: pacode_types::AgentId::main(),
                turn: pacode_types::TurnId::new("turn_1"),
                usage: None,
                stop: pacode_types::TurnStop::Completed,
            },
        },
        std::time::Instant::now(),
    );
    assert_eq!(
        state.drain_prompt_queue(),
        Some(Request::UserMessage {
            text: "queued".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
}

#[test]
fn ordered_sequence_keeps_the_ticket_message_id() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let ticket = state
        .session
        .begin_user_message_sequence(
            state.session_context(),
            PromptBatch {
                requests: vec![
                    Request::DetachTurn,
                    Request::UserMessage {
                        text: "detach then prompt".into(),
                        images: Vec::new(),
                        message_id: None,
                    },
                ],
            },
        )
        .unwrap();

    assert_eq!(ticket.sequence.requests[0], Request::DetachTurn);
    assert_eq!(ticket.sequence.requests[1], ticket.request);
    assert!(matches!(
        ticket.request,
        Request::UserMessage {
            message_id: Some(_),
            ..
        }
    ));
}

#[test]
fn retry_request_waits_for_explicit_action_before_queued_prompt() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.awaiting_snapshot = false;
    let request = Request::UserMessage {
        text: "retry".into(),
        images: Vec::new(),
        message_id: None,
    };
    let ticket = state
        .session
        .begin_user_message(state.session_context(), request);
    let request = ticket.request.clone();
    assert_eq!(
        state.session.finish_user_message(ticket.id, true),
        Some(request.clone())
    );
    state.input.queue_push("queued".into());

    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(
        state.take_retry_batch().map(|batch| batch.requests),
        Some(vec![request])
    );
    assert_eq!(
        state.drain_prompt_queue(),
        Some(Request::UserMessage {
            text: "queued".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
}

#[test]
fn stale_session_reply_does_not_apply_after_session_switch() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let old_client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 0,
        session: Some(SessionId::new("ses_old")),
    };
    state.models = vec![pacode_types::ModelInfo {
        route: ModelRoute::new("prov", "model"),
        display_name: "old".into(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }];

    state.leave_active_slot();
    state.meta = Some(meta("ses_new"));
    state.event_session_epoch = 2;
    state.awaiting_snapshot = false;
    state.lists.models.in_flight = true;
    let current_context = state.session_context();

    let actions = handle_bg_response(
        BgResponse::Reply {
            scope: ReplyScope::Session {
                context: current_context,
                client_context: old_client_context,
            },
            kind: Some(ListKind::Models),
            global_refresh: false,
            res: Ok(Reply::Models { models: Vec::new() }),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert_eq!(state.models.len(), 1);
    assert_eq!(state.models[0].display_name, "old");
    assert!(state.lists.models.in_flight);
}

#[test]
fn transient_startup_disconnect_keeps_attach_and_initial_prompt() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.initial_prompt = Some("queued startup prompt".into());
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();
    state.attach_in_flight = true;
    let disconnected = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: None,
    };

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context: disconnected.clone(),
            client_context: disconnected.clone(),
            attach,
            result: Err(ClientError::Disconnected),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(!state.quit);
    assert!(state.pending_attach.is_some());
    assert!(!state.attach_in_flight);
    assert_eq!(
        state.initial_prompt.as_deref(),
        Some("queued startup prompt")
    );

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: disconnected.clone(),
            reason: "connection lost".into(),
        },
        std::time::Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: disconnected,
            attempt: 1,
        },
        std::time::Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        std::time::Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            snapshot: snapshot("ses_test"),
        },
        std::time::Instant::now(),
    ));
    assert_eq!(
        state.meta.as_ref().map(|meta| meta.id.as_str()),
        Some("ses_test")
    );
    assert!(state.pending_attach.is_none());
    assert_eq!(
        state.initial_prompt.as_deref(),
        Some("queued startup prompt")
    );
}

#[test]
fn reconnect_snapshot_flushes_startup_prompt_once() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.initial_prompt = Some("startup prompt".into());
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    state.begin_attach(attach);
    state.set_pending_attach_client_epoch(1);
    state.attach_in_flight = true;
    let reconnect_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: reconnect_context.clone(),
            reason: "connection lost".into(),
        },
        std::time::Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: reconnect_context,
            attempt: 1,
        },
        std::time::Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        std::time::Instant::now(),
    ));
    let had_pending_attach = state.pending_attach.is_some();
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_test")),
            },
            snapshot: snapshot("ses_test"),
        },
        std::time::Instant::now(),
    ));

    let actions = snapshot_actions_after_apply(&mut state, had_pending_attach, true, true);
    assert_eq!(
        actions
            .iter()
            .filter(|action| matches!(action, Action::Send(Request::UserMessage { .. })))
            .count(),
        1
    );
    assert!(state.initial_prompt.is_none());
    assert!(
        snapshot_actions_after_apply(&mut state, false, true, true)
            .iter()
            .all(|action| !matches!(action, Action::Send(Request::UserMessage { .. })))
    );
}

#[test]
fn attach_flush_precedes_startup_prompt_and_startup_prompt_precedes_user_queue() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.initial_prompt = Some("startup prompt".into());
    state.input.queue_push("user prompt".into());
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();
    state.pending_requests.push_back(Request::Ping);
    state.attach_in_flight = true;
    let mut attached_snapshot = snapshot("ses_test");
    attached_snapshot.turn_active = true;
    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context: ClientEventContext::unscoped(),
            client_context: ClientEventContext::unscoped(),
            attach,
            result: Ok(attached_snapshot),
        },
        &mut state,
    );

    assert!(matches!(actions.first(), Some(Action::Send(Request::Ping))));
    assert!(state.initial_prompt.is_some());
    assert!(
        !actions
            .iter()
            .any(|action| { matches!(action, Action::Send(Request::UserMessage { .. })) })
    );

    state.turn_active = false;
    assert_eq!(
        state.drain_prompt_queue(),
        Some(Request::UserMessage {
            text: "startup prompt".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
    assert_eq!(
        state.input.prompt_queue.front().map(String::as_str),
        Some("user prompt")
    );
}

#[test]
fn global_resync_orders_epoch_and_coalesces_burst() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let old_context = state.ui_context();
    let marker_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    let now = std::time::Instant::now();

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::ResyncNeeded {
            context: marker_context.clone(),
        },
        now,
    ));
    assert_eq!(
        take_resync_action(&mut state, now),
        Some(Action::Send(Request::GetSnapshot))
    );
    assert_ne!(
        state.ui_context().deferred_epoch,
        old_context.deferred_epoch
    );
    let first_batch = take_global_resync_actions(&mut state, true);
    assert_eq!(first_batch.len(), 5);
    assert!(state.global_refresh_in_flight());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::ResyncNeeded {
            context: marker_context,
        },
        now,
    ));
    assert!(take_resync_action(&mut state, now).is_none());
    assert!(take_global_resync_actions(&mut state, true).is_empty());

    let stale = handle_bg_response(
        BgResponse::Reply {
            scope: ReplyScope::Global {
                context: old_context,
            },
            kind: Some(ListKind::Models),
            global_refresh: true,
            res: Ok(Reply::Models { models: Vec::new() }),
        },
        &mut state,
    );
    assert!(stale.is_empty());
    assert!(state.global_refresh_in_flight());

    let current_context = state.ui_context();
    for (kind, reply) in [
        (
            Some(ListKind::Models),
            Reply::Error {
                message: "models".into(),
            },
        ),
        (
            Some(ListKind::Plugins),
            Reply::Error {
                message: "plugins".into(),
            },
        ),
        (
            Some(ListKind::McpServers),
            Reply::Error {
                message: "mcp".into(),
            },
        ),
        (
            Some(ListKind::Sessions),
            Reply::Error {
                message: "sessions".into(),
            },
        ),
        (
            None,
            Reply::Error {
                message: "auth".into(),
            },
        ),
    ] {
        assert!(
            handle_bg_response(
                BgResponse::Reply {
                    scope: ReplyScope::Global {
                        context: current_context,
                    },
                    kind,
                    global_refresh: true,
                    res: Ok(reply),
                },
                &mut state,
            )
            .is_empty()
        );
    }
    assert!(!state.global_refresh_in_flight());
    assert!(take_global_resync_actions(&mut state, true).is_empty());
}

#[test]
fn reconnect_snapshot_requests_global_refresh_without_marker() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        std::time::Instant::now(),
    ));
    assert!(take_global_resync_actions(&mut state, false).is_empty());
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context,
            snapshot: snapshot("ses_test"),
        },
        std::time::Instant::now(),
    ));
    assert!(state.take_reconnect_snapshot_boundary());
    assert_eq!(take_global_resync_actions(&mut state, true).len(), 5);
    assert!(take_resync_action(&mut state, std::time::Instant::now()).is_none());
}

#[test]
fn sequence_failure_keeps_the_user_prompt_retryable() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = state.session_context();
    let ticket = state.session.begin_user_message(
        context,
        Request::UserMessage {
            text: "detach then prompt".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    let request = ticket.request.clone();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    let actions = handle_bg_response(
        BgResponse::UserMessageStepDone {
            ticket,
            client_context,
            request,
            result: Err(ClientError::Timeout("user_message")),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(state.has_retry_request());
    assert!(state.input.prompt_queue.is_empty());
}

#[test]
fn deferred_sequence_drops_detach_on_reconnect_and_keeps_user_suffix() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let user_request = Request::UserMessage {
        text: "deferred prompt".into(),
        images: Vec::new(),
        message_id: None,
    };
    queue_sequence(
        &mut state,
        vec![Request::DetachTurn, user_request.clone()],
        "deferred prompt".into(),
    );

    let old_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: old_context,
            attempt: 1,
        },
        Instant::now(),
    ));
    assert_eq!(
        state
            .session
            .pending_sequences
            .front()
            .map(|batch| &batch.requests),
        Some(&vec![user_request.clone()])
    );
    assert!(state.drain_pending_sequence().is_none());

    let new_context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: new_context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: new_context,
            snapshot: snapshot("ses_test"),
        },
        Instant::now(),
    ));
    assert_eq!(
        state.drain_pending_sequence().map(|batch| batch.requests),
        Some(vec![user_request])
    );
}

#[test]
fn partial_sequence_retry_repeats_only_unconfirmed_suffix() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.turn_active = true;
    let ticket = state
        .session
        .begin_user_message_sequence(
            state.session_context(),
            PromptBatch {
                requests: vec![
                    Request::DetachTurn,
                    Request::UserMessage {
                        text: "detach then prompt".into(),
                        images: Vec::new(),
                        message_id: None,
                    },
                ],
            },
        )
        .unwrap();
    let id = ticket.id;
    let user_request = ticket.request.clone();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert_eq!(
        handle_bg_response(
            BgResponse::UserMessageStepDone {
                ticket,
                client_context: client_context.clone(),
                request: Request::DetachTurn,
                result: Ok(Reply::TurnDetached {
                    agent: pacode_types::AgentId::main(),
                }),
            },
            &mut state,
        ),
        vec![Action::ContinuePromptBatch { id }]
    );
    let ticket = state.session.user_message_ticket(id).unwrap();
    assert_eq!(ticket.sequence.requests, vec![user_request.clone()]);

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 1,
            event: pacode_types::Event::AgentUpdated(main_agent(AgentStatus::Idle)),
        },
        Instant::now(),
    ));
    assert!(!state.turn_active);
    assert!(
        handle_bg_response(
            BgResponse::UserMessageStepDone {
                ticket,
                client_context,
                request: user_request.clone(),
                result: Err(ClientError::Disconnected),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(
        crate::keys::handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Instant::now(),
        ),
        vec![Action::Send(user_request)]
    );
}

#[test]
fn disconnect_before_progress_retries_suffix_without_detaching_new_turn() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.turn_active = true;
    let ticket = state
        .session
        .begin_user_message_sequence(
            state.session_context(),
            PromptBatch {
                requests: vec![
                    Request::DetachTurn,
                    Request::UserMessage {
                        text: "survives disconnect".into(),
                        images: Vec::new(),
                        message_id: None,
                    },
                ],
            },
        )
        .unwrap();
    let user_request = ticket.request.clone();
    let old_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: old_context.clone(),
            reason: "connection lost".into(),
        },
        Instant::now(),
    ));
    assert_eq!(state.user_msgs_in_flight, 0);
    assert_eq!(
        state
            .session
            .retry_requests
            .front()
            .map(|batch| &batch.requests),
        Some(&vec![user_request.clone()])
    );

    let new_context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: old_context,
            attempt: 1,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: new_context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    let mut active_snapshot = snapshot("ses_test");
    active_snapshot.turn_active = true;
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: new_context.clone(),
            snapshot: active_snapshot,
        },
        Instant::now(),
    ));
    assert!(state.take_retry_batch().is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: new_context,
            seq: 2,
            event: pacode_types::Event::AgentUpdated(main_agent(AgentStatus::Idle)),
        },
        Instant::now(),
    ));
    assert!(!state.turn_active);
    assert_eq!(
        crate::keys::handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Instant::now(),
        ),
        vec![Action::Send(user_request)]
    );
}

#[test]
fn main_agent_updates_drive_turn_active_and_retry_gate() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let ticket = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "wait for idle".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    let request = ticket.request.clone();
    state.session.finish_user_message(ticket.id, true);
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: context.clone(),
            seq: 1,
            event: pacode_types::Event::AgentUpdated(main_agent(AgentStatus::Thinking)),
        },
        Instant::now(),
    ));
    assert!(state.turn_active);
    assert!(state.take_retry_batch().is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context,
            seq: 2,
            event: pacode_types::Event::AgentUpdated(main_agent(AgentStatus::Idle)),
        },
        Instant::now(),
    ));
    assert!(!state.turn_active);
    assert_eq!(
        crate::keys::handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Instant::now(),
        ),
        vec![Action::Send(request)]
    );
}

#[test]
fn sequence_success_checkpoints_detach_before_user_message() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let ticket = state
        .session
        .begin_user_message_sequence(
            state.session_context(),
            PromptBatch {
                requests: vec![
                    Request::DetachTurn,
                    Request::UserMessage {
                        text: "detach then prompt".into(),
                        images: Vec::new(),
                        message_id: None,
                    },
                ],
            },
        )
        .unwrap();
    let id = ticket.id;
    let user_request = ticket.request.clone();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert_eq!(
        handle_bg_response(
            BgResponse::UserMessageStepDone {
                ticket,
                client_context: client_context.clone(),
                request: Request::DetachTurn,
                result: Ok(Reply::TurnDetached {
                    agent: pacode_types::AgentId::main(),
                }),
            },
            &mut state,
        ),
        vec![Action::ContinuePromptBatch { id }]
    );
    let ticket = state.session.user_message_ticket(id).unwrap();
    assert!(
        handle_bg_response(
            BgResponse::UserMessageStepDone {
                ticket,
                client_context,
                request: user_request,
                result: Ok(Reply::Ok),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(!state.has_retry_request());
}

#[test]
fn stale_deferred_reply_does_not_clear_a_new_preview_marker() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    let context = state.session_context();
    let key = PreviewKey {
        raw: "src/main.rs".into(),
    };
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_old")),
    };
    state.files.preview_in_flight = Some(key.clone());
    state.leave_active_slot();
    state.meta = Some(meta("ses_new"));
    state.files.preview_in_flight = Some(key.clone());

    let actions = handle_bg_response(
        BgResponse::FilePreviewReady {
            context,
            client_context,
            key,
            resolved: PathBuf::from("/tmp/src/main.rs"),
            text: None,
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(state.files.preview_in_flight.is_some());
    assert!(state.files.cached_preview.is_none());
}

#[test]
fn connected_before_snapshot_keeps_prompt_queue_gated() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = true;
    state.input.queue_push("queued".into());
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: client_context.clone(),
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        std::time::Instant::now(),
    ));
    assert!(matches!(
        state.connection,
        crate::state::Connection::Connected
    ));
    assert!(state.awaiting_snapshot);
    assert!(!prompt_gate_open(&state));
    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(state.input.prompt_queue.len(), 1);

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: client_context,
            snapshot: snapshot("ses_test"),
        },
        std::time::Instant::now(),
    ));
    assert!(!state.awaiting_snapshot);
    assert!(prompt_gate_open(&state));
    assert_eq!(
        state.drain_prompt_queue(),
        Some(Request::UserMessage {
            text: "queued".into(),
            images: Vec::new(),
            message_id: None,
        })
    );
}

#[test]
fn resync_marker_dispatches_get_snapshot_action() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::ResyncNeeded { context },
        std::time::Instant::now(),
    ));
    assert_eq!(
        take_resync_action(&mut state, std::time::Instant::now()),
        Some(Action::Send(Request::GetSnapshot))
    );
}

#[test]
fn resync_buffers_events_and_replays_them_once() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.last_event_seq = Some(1);
    state.resync_want = true;
    state.lists.mark_sent(ListKind::Models);
    state.files.preview_in_flight = Some(PreviewKey {
        raw: "src/main.rs".into(),
    });
    let old_ui_context = state.ui_context();
    let now = std::time::Instant::now();

    assert_eq!(
        take_resync_action(&mut state, now),
        Some(Action::Send(Request::GetSnapshot))
    );
    assert!(state.awaiting_snapshot);
    assert!(state.resync_in_flight);
    assert!(!state.lists.models.in_flight);
    assert!(state.files.preview_in_flight.is_none());
    assert!(!state.ui_context_is_current(&old_ui_context));
    assert!(
        handle_bg_response(
            BgResponse::Reply {
                scope: ReplyScope::Global {
                    context: old_ui_context,
                },
                kind: None,
                global_refresh: false,
                res: Ok(Reply::McpPrompt {
                    text: "stale".into(),
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(state.input.text.is_empty());

    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(!state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 3,
            event: pacode_types::Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "buffered".into(),
                detail: None,
            },
        },
        now,
    ));
    assert_eq!(state.session.buffered_event_count(), 1);
    assert!(state.toasts.is_empty());
    let context = state.session_context();
    let mut resync_snapshot = snapshot("ses_test");
    resync_snapshot.seq = 2;

    assert!(
        handle_bg_response(
            BgResponse::ResyncDone {
                context: context.clone(),
                client_context: client_context.clone(),
                result: Ok(Reply::Snapshot(resync_snapshot.clone())),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(!state.awaiting_snapshot);
    assert!(!state.resync_in_flight);
    assert_eq!(state.session.buffered_event_count(), 0);
    assert_eq!(state.last_event_seq, Some(3));
    assert_eq!(state.toasts.len(), 1);
    assert_eq!(state.toasts[0].title, "buffered");

    assert!(
        handle_bg_response(
            BgResponse::ResyncDone {
                context,
                client_context,
                result: Ok(Reply::Snapshot(resync_snapshot)),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(state.last_event_seq, Some(3));
    assert_eq!(state.toasts.len(), 1);
}

#[test]
fn failed_resync_releases_prompt_gate_and_replays_buffered_events() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.last_event_seq = Some(1);
    state.resync_want = true;
    state.input.queue_push("queued".into());
    let now = std::time::Instant::now();
    assert_eq!(
        take_resync_action(&mut state, now),
        Some(Action::Send(Request::GetSnapshot))
    );
    let context = state.session_context();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    assert!(!state.apply_client_event(
        pacode_client::ClientEvent::Event {
            context: client_context.clone(),
            seq: 2,
            event: pacode_types::Event::Toast {
                level: pacode_types::ToastLevel::Info,
                title: "buffered".into(),
                detail: None,
            },
        },
        now,
    ));

    assert!(
        handle_bg_response(
            BgResponse::ResyncDone {
                context,
                client_context,
                result: Err(ClientError::Timeout("get_snapshot")),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(state.input.prompt_queue.len(), 1);
    assert!(!state.awaiting_snapshot);
    assert!(!state.resync_in_flight);
    assert!(state.resync_want);
    assert_eq!(state.session.buffered_event_count(), 0);
    assert_eq!(state.last_event_seq, Some(2));
    assert_eq!(state.toasts.len(), 1);
}

#[test]
fn resync_invalidates_old_prompt_reply_without_stranding_retry() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let request = Request::UserMessage {
        text: "retry".into(),
        images: Vec::new(),
        message_id: None,
    };
    let ticket = state
        .session
        .begin_user_message(state.session_context(), request);
    let request = ticket.request.clone();
    let old_client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    state.resync_want = true;
    assert_eq!(
        take_resync_action(&mut state, std::time::Instant::now()),
        Some(Action::Send(Request::GetSnapshot))
    );
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(state.has_retry_request());

    assert!(
        handle_bg_response(
            BgResponse::UserMessageDone {
                ticket,
                client_context: old_client_context,
                result: Ok(Reply::Ok),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(state.user_msgs_in_flight, 0);

    let context = state.session_context();
    assert!(
        handle_bg_response(
            BgResponse::ResyncDone {
                context,
                client_context: ClientEventContext {
                    connection_epoch: 1,
                    session_epoch: 1,
                    session: Some(SessionId::new("ses_test")),
                },
                result: Ok(Reply::Snapshot(snapshot("ses_test"))),
            },
            &mut state,
        )
        .is_empty()
    );
    assert_eq!(
        state.take_retry_batch().map(|batch| batch.requests),
        Some(vec![request])
    );
}

#[test]
fn timeout_attach_retry_reuses_pending_attempt() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.connection = crate::state::Connection::Connecting;
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();
    state.set_pending_attach_client_epoch(1);
    state.attach_in_flight = true;
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: None,
    };

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context: context.clone(),
            token,
            request_client_context: client_context.clone(),
            client_context: client_context.clone(),
            attach: attach.clone(),
            result: Err(ClientError::Timeout("attach")),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(!state.quit);
    assert_eq!(state.pending_attach_token(), Some(token));
    assert_eq!(
        state.pending_attach.as_ref().map(|pending| &pending.attach),
        Some(&attach)
    );
    assert!(state.take_pending_attach_retry().is_none());
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: client_context,
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        std::time::Instant::now(),
    ));
    let retry = state.take_pending_attach_retry().unwrap();
    assert_eq!(retry.context, context);
    assert_eq!(retry.token, token);
    assert_eq!(retry.attach, attach);
    assert_eq!(retry.client_session_epoch(), 1);
    assert!(state.attach_in_flight);
}

#[test]
fn non_transient_switch_attach_failure_clears_pending_state() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.leave_active_slot();
    let attach = Attach::Resume {
        session: SessionId::new("ses_missing"),
    };
    let context = state.begin_attach(attach.clone());
    let token = state.pending_attach_token().unwrap();
    state.set_pending_attach_client_epoch(2);
    state.attach_in_flight = true;
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 2,
        session: None,
    };

    let actions = handle_bg_response(
        BgResponse::AttachFinished {
            context,
            token,
            request_client_context: client_context.clone(),
            client_context,
            attach,
            result: Err(ClientError::Daemon("session not found".into())),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    assert!(state.pending_attach.is_none());
    assert!(!state.attach_in_flight);
    assert!(!state.awaiting_snapshot);
    assert!(matches!(
        state.connection,
        crate::state::Connection::Disconnected { .. }
    ));
    assert!(!state.quit);
}

#[test]
fn attach_failed_startup_clears_barriers_and_fails_fast() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let attach = Attach::New {
        cwd: PathBuf::from("/tmp"),
        model: None,
        effort: None,
        mode: None,
    };
    state.begin_attach(attach.clone());
    state.set_pending_attach_client_epoch(1);
    state.attach_in_flight = true;
    state.resync_want = true;
    state.resync_in_flight = true;
    state.resync_sent_at = Some(std::time::Instant::now());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::AttachFailed {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: None,
            },
            attach: Attach::Resume {
                session: SessionId::new("ses_new"),
            },
            reason: "session not found".into(),
        },
        std::time::Instant::now(),
    ));
    assert!(state.pending_attach.is_none());
    assert!(!state.attach_in_flight);
    assert!(!state.resync_want);
    assert!(!state.resync_in_flight);
    assert!(state.resync_sent_at.is_none());
    assert!(!state.awaiting_snapshot);
    assert!(state.quit);
    assert!(matches!(
        state.attach_error,
        Some(ClientError::Daemon(message)) if message == "session not found"
    ));
}

#[test]
fn attach_failed_active_reconnect_is_terminal_without_pending_attach() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.resync_want = true;
    state.resync_in_flight = true;

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::AttachFailed {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: None,
            },
            attach: Attach::Resume {
                session: SessionId::new("ses_old"),
            },
            reason: "session not found".into(),
        },
        std::time::Instant::now(),
    ));
    assert!(state.pending_attach.is_none());
    assert!(!state.resync_want);
    assert!(!state.resync_in_flight);
    assert!(state.quit);
    assert!(matches!(
        state.attach_error,
        Some(ClientError::Daemon(message)) if message == "session not found"
    ));
}

#[test]
fn attach_failed_active_context_clears_pending_without_retry() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.leave_active_slot();
    let attach = Attach::Resume {
        session: SessionId::new("ses_missing"),
    };
    state.begin_attach(attach.clone());
    state.set_pending_attach_client_epoch(2);
    state.attach_in_flight = true;
    state.resync_want = true;
    state.resync_in_flight = true;

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::AttachFailed {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 2,
                session: None,
            },
            attach,
            reason: "session not found".into(),
        },
        std::time::Instant::now(),
    ));
    assert!(state.pending_attach.is_none());
    assert!(!state.attach_in_flight);
    assert!(!state.resync_want);
    assert!(!state.resync_in_flight);
    assert!(state.quit);
}

#[test]
fn an_oversized_paste_is_cut_at_the_prompt_cap_and_reported() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let cap = crate::paste::PASTE_MAX_CHARS;
    let huge = "x".repeat(cap + 4096);

    let bounded = bound_paste(&mut state, huge, Instant::now());
    assert_eq!(bounded.chars().count(), cap);
    assert!(
        state
            .toasts
            .iter()
            .any(|toast| toast.title.contains("truncated"))
    );

    let small = "short paste".to_string();
    let kept = bound_paste(&mut state, small, Instant::now());
    assert_eq!(kept, "short paste");
}

#[test]
fn a_request_the_client_cannot_bind_waits_for_the_next_attach() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.resync_want = true;
    state.resync_in_flight = true;
    state.awaiting_snapshot = true;

    defer_unbound_request(&mut state, Request::Compact);
    assert_eq!(state.pending_requests.len(), 1);
    assert!(state.pending_requests.contains(&Request::Compact));

    defer_unbound_request(
        &mut state,
        Request::UserMessage {
            text: "typed while detached".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    assert_eq!(state.pending_requests.len(), 1);
    assert_eq!(state.input.prompt_queue.len(), 1);

    defer_unbound_request(&mut state, Request::GetSnapshot);
    assert!(!state.resync_in_flight);
    assert!(!state.awaiting_snapshot);
}

#[test]
fn the_pending_queue_keeps_one_copy_of_a_request() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    queue_pending_request(&mut state, Request::Interrupt);
    queue_pending_request(&mut state, Request::Interrupt);
    queue_pending_request(&mut state, Request::InterruptAll);
    assert_eq!(state.pending_requests.len(), 2);
}

#[test]
fn one_batch_never_asks_the_daemon_the_same_thing_twice() {
    let batch = dedup_actions(vec![
        Action::Send(Request::ListModels),
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListModels),
        Action::GitLog { after: None },
        Action::Send(Request::ListSessions { limit: 10 }),
        Action::Send(Request::ListSessions { limit: 50 }),
    ]);

    assert_eq!(
        batch,
        vec![
            Action::Send(Request::ListModels),
            Action::Send(Request::ListPlugins),
            Action::GitLog { after: None },
            Action::Send(Request::ListSessions { limit: 10 }),
            Action::Send(Request::ListSessions { limit: 50 }),
        ]
    );
}

#[test]
fn a_global_resync_skips_a_list_the_pending_queue_already_carries() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    queue_pending_request(&mut state, Request::ListModels);

    let batch = take_global_resync_actions(&mut state, true);

    assert_eq!(batch.len(), 4);
    assert!(
        batch
            .iter()
            .all(|action| !matches!(action, Action::Send(Request::ListModels)))
    );
    assert!(state.global_refresh_in_flight());
}

#[test]
fn one_panel_load_per_target_and_only_the_newest_reply_applies() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = state.session_context();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    let agent = PanelTarget::Agent(AgentId::main());
    state.panel.target = Some(agent.clone());

    let first = begin_panel_load(&context, &agent).expect("first load flies");
    assert_eq!(begin_panel_load(&context, &agent), None);

    let other = PanelTarget::Agent(AgentId::new("agt_other"));
    let second = begin_panel_load(&context, &other).expect("another target flies");
    let switched = SessionContext {
        generation: context.generation + 1,
        ..context.clone()
    };
    let third = begin_panel_load(&switched, &agent).expect("a new session loads again");

    let page = |seq: u64| {
        vec![TranscriptItem {
            seq,
            agent: AgentId::main(),
            ts_ms: seq * 1_000,
            kind: TranscriptKind::User {
                text: format!("line {seq}"),
            },
        }]
    };

    assert!(
        handle_bg_response(
            BgResponse::LoadPanelReply {
                context: context.clone(),
                client_context: client_context.clone(),
                target: agent.clone(),
                token: first,
                result: Ok(Reply::History {
                    agent: AgentId::main(),
                    items: page(1),
                    has_more: false,
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(
        handle_bg_response(
            BgResponse::LoadPanelReply {
                context: context.clone(),
                client_context: client_context.clone(),
                target: other,
                token: second,
                result: Ok(Reply::History {
                    agent: AgentId::main(),
                    items: page(2),
                    has_more: false,
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(
        handle_bg_response(
            BgResponse::LoadPanelReply {
                context: switched,
                client_context,
                target: agent.clone(),
                token: third,
                result: Ok(Reply::History {
                    agent: AgentId::main(),
                    items: page(3),
                    has_more: false,
                }),
            },
            &mut state,
        )
        .is_empty()
    );
    assert!(state.panel.agent_transcript.cells.is_empty());
    assert!(begin_panel_load(&context, &agent).is_some());
}

#[test]
fn a_history_page_that_no_longer_answers_the_oldest_row_releases_the_flag() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = state.session_context();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    state.transcript.loading_history = true;

    assert!(
        handle_bg_response(
            BgResponse::LoadHistoryReply {
                context: context.clone(),
                client_context: client_context.clone(),
                before_seq: Some(7),
                result: Ok(Reply::History {
                    agent: AgentId::main(),
                    items: Vec::new(),
                    has_more: true,
                }),
            },
            &mut state,
        )
        .is_empty()
    );

    assert!(!state.transcript.loading_history);
}

#[test]
fn a_worktree_diff_that_landed_after_the_panel_moved_releases_its_marker() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let context = state.session_context();
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    state.files.diff_in_flight = Some("src/main.rs".into());
    state.panel.target = Some(PanelTarget::CommitDiff {
        hash: "abc123".into(),
    });

    assert!(
        handle_bg_response(
            BgResponse::FileDiffReady {
                context,
                client_context,
                path: "src/main.rs".into(),
                diff: pacode_vcs::FileDiff {
                    patch: Some("--- a\n+++ b".into()),
                    status: Some(pacode_vcs::FileStatus::Modified),
                    truncated: false,
                    note: None,
                },
            },
            &mut state,
        )
        .is_empty()
    );

    assert!(state.files.diff_in_flight.is_none());
    assert!(state.panel.diff.is_none());
}

#[test]
fn an_inline_thumbnail_over_the_cap_falls_back_to_the_placeholder() {
    let path = std::env::temp_dir().join(format!(
        "pacode_thumb_{}_{}.png",
        std::process::id(),
        IMAGE_THUMB_MAX_BYTES
    ));
    let file = std::fs::File::create(&path).expect("temp file");
    file.set_len(IMAGE_THUMB_MAX_BYTES + 1).expect("sized file");
    drop(file);

    assert!(render_thumbnail(&path, 20, 6).is_none());

    let missing = std::env::temp_dir().join("pacode_thumb_absent_file.png");
    let _ = std::fs::remove_file(&missing);
    assert!(render_thumbnail(&missing, 20, 6).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_refused_clear_keeps_the_transcript_and_says_why() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let client_context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_test")),
    };
    state.transcript.reset(
        vec![TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::User {
                text: "before the clear".into(),
            },
        }],
        false,
    );
    assert_eq!(
        crate::commands::execute(&mut state, "/clear"),
        vec![Action::Send(Request::ClearContext)]
    );

    let actions = handle_bg_response(
        BgResponse::Reply {
            scope: ReplyScope::Session {
                context: state.session_context(),
                client_context,
            },
            kind: None,
            global_refresh: false,
            res: Ok(Reply::Error {
                message: "store: injected store failure".into(),
            }),
        },
        &mut state,
    );

    assert!(actions.is_empty());
    let kinds: Vec<_> = state.transcript.cells.iter().map(|c| &c.kind).collect();
    assert!(matches!(
        kinds.first(),
        Some(CellKind::Item(TranscriptKind::User { text })) if text == "before the clear"
    ));
    assert!(matches!(
        kinds.last(),
        Some(CellKind::Item(TranscriptKind::Notice { level: ToastLevel::Error, text }))
            if text.contains("injected store failure")
    ));
}
