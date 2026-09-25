use std::time::Instant;

use pacode_client::ClientEventContext;

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{Mode, Plan, UsageTotals};
use pacode_types::{Attach, Config, Event, Request, SessionId, SessionMeta, SessionSnapshot};

use super::*;
use crate::state::{AppState, PanelTarget};

fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: None,
        cwd: "/tmp".into(),
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

#[test]
fn leaving_slot_replaces_all_session_ui_state() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_old"));
    state.input.text = "draft".into();
    state.input.queue_push("queued".into());
    state
        .files
        .observe_tool_item("write", &serde_json::json!({"path": "src/x.rs"}), 1);
    state.files.preview_want = Some(crate::state::files::PreviewKey {
        raw: "src/x.rs".into(),
    });
    state.panel.target = Some(PanelTarget::McpServer("server".into()));
    state.panel.diff = Some(PanelDiff {
        path: "src/x.rs".into(),
        patch: None,
        status: None,
        truncated: false,
        note: None,
        rendered: None,
    });
    state.focus = crate::state::Focus::Overlay(crate::state::Overlay::QuestionPicker);
    state.selection.active = true;
    state.selection.dragging = true;
    state.turn_active = true;
    state.turn_started_at = Some(Instant::now());
    state.anim_frame = 42;
    state.phase = Some((Phase::Thinking, Instant::now()));
    state.phase_elapsed_ms = 100;
    state.started_at = Instant::now();
    state.second_parity = true;
    state.last_focus_key = 9;
    state.overlay_since = Some(Instant::now());
    state.pending_copy = Some(crate::state::PendingCopy {
        text: "copy".into(),
        auto: false,
    });
    state.pending_requests.push_back(Request::Ping);
    state.resync_want = true;
    state.resync_in_flight = true;
    state.resync_sent_at = Some(Instant::now());
    state.last_event_seq = Some(9);
    state.image_strip.want.push((1, "/tmp/x.png".into(), 20, 6));
    let generation = state.session.generation;

    state.leave_active_slot();

    assert_eq!(state.session.generation, generation + 1);
    assert!(state.meta.is_none());
    assert!(state.input.text.is_empty());
    assert!(state.input.prompt_queue.is_empty());
    assert!(state.files.rows.is_empty());
    assert!(state.files.preview_want.is_none());
    assert!(state.panel.target.is_none());
    assert!(state.panel.diff.is_none());
    assert_eq!(state.focus, crate::state::Focus::Normal);
    assert!(!state.selection.active);
    assert!(!state.turn_active);
    assert!(state.turn_started_at.is_none());
    assert_eq!(state.anim_frame, 0);
    assert!(state.phase.is_none());
    assert_eq!(state.phase_elapsed_ms, 0);
    assert!(state.pending_copy.is_none());
    assert!(state.pending_requests.is_empty());
    assert!(!state.resync_want);
    assert!(!state.resync_in_flight);
    assert!(state.resync_sent_at.is_none());
    assert!(state.image_strip.want.is_empty());
    assert!(state.questions.is_empty());
}

#[test]
fn attach_context_rejects_a_snapshot_for_another_slot_or_session() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let target = SessionId::new("ses_target");
    let attach = Attach::Resume {
        session: target.clone(),
    };
    let context = state.begin_attach(attach.clone());
    let client_context = ClientEventContext {
        connection_epoch: 0,
        session_epoch: 0,
        session: Some(target.clone()),
    };

    assert!(state.accepts_attach(&context, &client_context, &attach, &snapshot("ses_target")));
    assert!(!state.accepts_attach(&context, &client_context, &attach, &snapshot("ses_other")));
    state.active_slot = 1;
    assert!(!state.accepts_attach(&context, &client_context, &attach, &snapshot("ses_target")));
}

#[test]
fn failed_user_message_releases_gate_and_keeps_explicit_retry() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let context = state.session_context();
    let request = Request::UserMessage {
        text: "keep me".into(),
        images: Vec::new(),
        message_id: None,
    };
    let ticket = state.session.begin_user_message(context, request);
    let request = ticket.request.clone();
    let Request::UserMessage {
        message_id: Some(message_id),
        ..
    } = &request
    else {
        panic!("begin_user_message must assign a client message id");
    };
    assert!(message_id.starts_with("tui-"));

    assert_eq!(state.user_msgs_in_flight, 1);
    assert_eq!(
        state.session.finish_user_message(ticket.id, true),
        Some(request.clone())
    );
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(state.has_retry_request());
    assert_eq!(
        state.session.take_retry_batch(),
        Some(PromptBatch::single(request))
    );
}

#[test]
fn begin_user_message_preserves_an_existing_client_message_id() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let first = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "retry".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    let second = state
        .session
        .begin_user_message(state.session_context(), first.request.clone());

    let Request::UserMessage {
        message_id: Some(first_id),
        ..
    } = &first.request
    else {
        panic!("first ticket must have a client message id");
    };
    let Request::UserMessage {
        message_id: Some(second_id),
        ..
    } = &second.request
    else {
        panic!("retry ticket must keep a client message id");
    };
    assert_eq!(first_id, second_id);
}

#[test]
fn user_message_ticket_ids_remain_unique_after_session_reset() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let request = Request::UserMessage {
        text: "first".into(),
        images: Vec::new(),
        message_id: None,
    };
    let first = state
        .session
        .begin_user_message(state.session_context(), request.clone());
    state.session.reset(300);
    let second = state
        .session
        .begin_user_message(state.session_context(), request);
    assert_ne!(first.id, second.id);
    let Request::UserMessage {
        message_id: Some(first_message_id),
        ..
    } = &first.request
    else {
        panic!("first ticket must have a client message id");
    };
    let Request::UserMessage {
        message_id: Some(second_message_id),
        ..
    } = &second.request
    else {
        panic!("second ticket must have a client message id");
    };
    assert_ne!(first_message_id, second_message_id);
    assert!(state.session.finish_user_message(first.id, false).is_none());
    assert!(
        state
            .session
            .finish_user_message(second.id, false)
            .is_some()
    );
}

#[test]
fn reconnect_snapshot_can_complete_a_pending_attach() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let target = SessionId::new("ses_target");
    let attach = Attach::Resume {
        session: target.clone(),
    };
    state.begin_attach(attach);
    let context = ClientEventContext {
        connection_epoch: 2,
        session_epoch: 1,
        session: Some(target),
    };
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context,
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(state.pending_attach.is_none());
    assert!(!state.attach_in_flight);
    assert_eq!(
        state.meta.as_ref().map(|meta| &meta.id),
        Some(&SessionId::new("ses_target"))
    );
}

#[test]
fn disconnect_keeps_pending_attach_for_reconnect_snapshot() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let target = SessionId::new("ses_target");
    let attach = Attach::Resume {
        session: target.clone(),
    };
    state.begin_attach(attach);
    let old_token = state.pending_attach_token().unwrap();
    state.attach_in_flight = true;
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(target.clone()),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: context.clone(),
            reason: "connection lost".into(),
        },
        Instant::now(),
    ));
    assert!(state.pending_attach.is_some());
    assert_ne!(state.pending_attach_token(), Some(old_token));
    assert!(
        state
            .pending_attach
            .as_ref()
            .is_some_and(|pending| pending.retry_requested)
    );
    assert!(!state.attach_in_flight);
    assert!(!state.quit);
    assert!(state.attach_error.is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: context.clone(),
            attempt: 1,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(target),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert_eq!(
        state.meta.as_ref().map(|meta| &meta.id),
        Some(&SessionId::new("ses_target"))
    );
    assert!(state.pending_attach.is_none());
}

#[test]
fn idle_snapshot_keeps_boundary_until_in_flight_message_finishes() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let target = SessionId::new("ses_target");
    state.meta = Some(meta("ses_target"));
    state.connection = crate::state::Connection::Connected;
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
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(target),
    };

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context,
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(state.turn_boundary_pending);
    assert!(state.drain_prompt_queue().is_none());

    state.session.finish_user_message(ticket.id, false);
    assert!(state.turn_boundary_pending);
    assert!(state.resync_want);
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(!state.turn_boundary_pending);
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
fn turn_end_before_user_message_ack_releases_the_prompt_gate() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_test"));
    state.connection = crate::state::Connection::Connected;
    let ticket = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "already ended".into(),
            images: Vec::new(),
            message_id: None,
        },
    );

    state.session.observe_turn_ended();
    state.session.finish_user_message(ticket.id, false);

    assert!(!state.turn_boundary_pending);
    assert!(!state.resync_want);
}

#[test]
fn retry_waits_for_in_flight_message_and_boundary() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_target"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    let retry = Request::UserMessage {
        text: "retry".into(),
        images: Vec::new(),
        message_id: None,
    };
    let first = state
        .session
        .begin_user_message(state.session_context(), retry);
    let retry = first.request.clone();
    let second = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "second".into(),
            images: Vec::new(),
            message_id: None,
        },
    );

    state.session.finish_user_message(first.id, true);
    assert_eq!(state.user_msgs_in_flight, 1);
    assert!(state.turn_boundary_pending);
    assert!(state.session.take_retry_batch().is_none());
    assert!(state.drain_prompt_queue().is_none());

    state.session.finish_user_message(second.id, false);
    assert_eq!(state.user_msgs_in_flight, 0);
    assert!(state.turn_boundary_pending);
    assert!(state.session.take_retry_batch().is_none());
    assert!(state.drain_prompt_queue().is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(
        state.session.take_retry_batch(),
        Some(PromptBatch::single(retry))
    );
}

#[test]
fn buffered_deltas_coalesce_without_losing_the_sequence_range() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_target")),
    };
    let agent = pacode_types::AgentId::main();
    assert!(state.session.buffer_event(
        context.clone(),
        1,
        Event::TextDelta {
            agent: agent.clone(),
            item_seq: 7,
            text: "a".into(),
        },
    ));
    assert!(state.session.buffer_event(
        context,
        2,
        Event::TextDelta {
            agent,
            item_seq: 7,
            text: "b".into(),
        },
    ));
    assert_eq!(state.session.buffered_event_count(), 1);
    let (events, overflowed) = state.session.take_buffered_events();
    assert!(!overflowed);
    assert_eq!(events[0].first_seq, 1);
    assert_eq!(events[0].seq, 2);
    assert!(matches!(
        events[0].event.as_ref(),
        Some(Event::TextDelta { text, .. }) if text == "ab"
    ));
}

#[test]
fn a_single_oversized_buffered_event_is_rejected_and_marks_overflow() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    let context = ClientEventContext {
        connection_epoch: 1,
        session_epoch: 1,
        session: Some(SessionId::new("ses_target")),
    };
    let event = Event::TextDelta {
        agent: pacode_types::AgentId::main(),
        item_seq: 1,
        text: "x".repeat(4 * 1024 * 1024),
    };
    assert!(!state.session.buffer_event(context, 1, event));
    assert_eq!(state.session.buffered_event_count(), 0);
    assert_eq!(state.session.buffered_event_bytes(), 0);
    let (_, overflowed) = state.session.take_buffered_events();
    assert!(overflowed);
}

#[test]
fn attach_state_self_heals_when_the_generation_is_gone() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.attach_in_flight = true;
    assert!(state.take_pending_attach_retry().is_none());
    assert!(!state.attach_in_flight);
    assert!(!state.request_attach_retry());
    assert!(!state.attach_in_flight);
}

#[test]
fn generation_counters_skip_the_reserved_zero_value() {
    let mut state = SessionUiState {
        deferred_epoch: u64::MAX,
        next_attach_token: u64::MAX,
        turn_generation: u64::MAX,
        ..SessionUiState::default()
    };
    state.advance_deferred_epoch();
    assert_eq!(state.deferred_epoch, 1);
    assert_eq!(state.allocate_attach_token(), 1);
    state.observe_turn_started_for(None);
    assert_eq!(state.turn_generation, 1);
}

#[test]
fn turn_fences_do_not_release_a_prompt_before_its_own_turn_ends() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_target"));
    let ticket = state.session.begin_user_message(
        state.session_context(),
        Request::UserMessage {
            text: "fenced".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    let turn = pacode_types::TurnId::new("turn_new");
    state.session.observe_turn_started_for(Some(&turn));
    assert!(
        state
            .session
            .finish_user_message(ticket.id, false)
            .is_some()
    );
    assert!(state.turn_boundary_pending);
    assert_eq!(state.session.observe_turn_end(&turn), TurnEndMatch::Current);
    assert!(!state.turn_boundary_pending);
}

#[test]
fn reconnect_invalidates_deferred_context_and_clears_markers() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_target"));
    state.connection = crate::state::Connection::Connected;
    state.event_connection_epoch = 1;
    state.event_session_epoch = 1;
    state.awaiting_snapshot = false;
    state.transcript.loading_history = true;
    state.input.at_comp.in_flight = Some(crate::state::input::AtKey {
        query: "src".into(),
        cursor_byte: 0,
        cwd: "/tmp".into(),
    });
    state.input.bash_comp.in_flight = Some(crate::state::input::BashKey {
        text: "git".into(),
        cursor: 3,
        cwd: "/tmp".into(),
        history_len: 0,
    });
    state.files.preview_in_flight = Some(crate::state::files::PreviewKey {
        raw: "src/main.rs".into(),
    });
    state.files.statused_at = Some(1);
    state.files.statuses_in_flight = true;
    state.files.diff_in_flight = Some("src/main.rs".into());
    state.rail.task_tail_in_flight = Some(crate::state::rail::TaskTailKey {
        task: pacode_types::TaskId::new("task_1"),
        output_bytes: 10,
    });
    state.image_strip.in_flight.insert((1, 20, 6));
    state.git.loading = true;
    state.git.loading_more = true;
    state.git.log_after = Some("abc".into());
    state.git.show_in_flight = Some("abc".into());
    state.resync_in_flight = true;
    let (cancel, _receiver) = tokio::sync::oneshot::channel();
    state.running_bash = Some(cancel);
    let old_session = state.session_context();
    let old_ui = state.ui_context();

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            reason: "connection lost".into(),
        },
        Instant::now(),
    ));
    assert!(!state.context_is_current(&old_session));
    assert!(!state.ui_context_is_current(&old_ui));
    assert!(!state.transcript.loading_history);
    assert!(state.input.at_comp.in_flight.is_none());
    assert!(state.input.bash_comp.in_flight.is_none());
    assert!(state.files.preview_in_flight.is_none());
    assert!(state.files.statused_at.is_none());
    assert!(!state.files.statuses_in_flight);
    assert!(state.files.diff_in_flight.is_none());
    assert!(state.rail.task_tail_in_flight.is_none());
    assert!(state.image_strip.in_flight.is_empty());
    assert!(!state.git.loading);
    assert!(!state.git.loading_more);
    assert!(state.git.log_after.is_none());
    assert!(state.git.show_in_flight.is_none());
    assert!(!state.resync_in_flight);
    assert!(state.running_bash.is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Reconnecting {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            attempt: 1,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(!state.context_is_current(&old_session));
    assert!(!state.ui_context_is_current(&old_ui));
}

#[test]
fn reconnect_holds_retry_until_snapshot_confirms_idle() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_target"));
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

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Disconnected {
            context: ClientEventContext {
                connection_epoch: 1,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            reason: "connection lost".into(),
        },
        Instant::now(),
    ));
    assert!(state.has_retry_request());
    assert!(state.turn_boundary_pending);
    assert!(state.session.take_retry_batch().is_none());
    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(state.session.finish_user_message(ticket.id, false), None);

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Connected {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            daemon_version: "test".into(),
            pid: 1,
            version_mismatch: false,
        },
        Instant::now(),
    ));
    assert!(state.session.take_retry_batch().is_none());
    assert!(state.drain_prompt_queue().is_none());

    assert!(state.apply_client_event(
        pacode_client::ClientEvent::Snapshot {
            context: ClientEventContext {
                connection_epoch: 2,
                session_epoch: 1,
                session: Some(SessionId::new("ses_target")),
            },
            snapshot: snapshot("ses_target"),
        },
        Instant::now(),
    ));
    assert!(state.drain_prompt_queue().is_none());
    assert_eq!(
        state.session.take_retry_batch(),
        Some(PromptBatch::single(request))
    );
}

#[test]
fn every_fence_counter_survives_a_session_switch() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_one"));
    let context = state.session_context();
    state.session.begin_user_message(
        context,
        Request::UserMessage {
            text: "queued".into(),
            images: Vec::new(),
            message_id: None,
        },
    );
    state
        .session
        .observe_turn_started_for(Some(&pacode_types::TurnId::new("turn_1")));
    state
        .session
        .observe_turn_end(&pacode_types::TurnId::new("turn_1"));
    state.session.allocate_attach_token();

    let generation = state.session.generation;
    let deferred_epoch = state.session.deferred_epoch;
    let turn_generation = state.session.turn_generation;
    let message_id = state.session.next_user_message_id;
    let attach_token = state.session.next_attach_token;

    state.leave_active_slot();

    assert_eq!(state.session.generation, generation + 1);
    assert_eq!(state.session.deferred_epoch, deferred_epoch);
    assert_eq!(
        state.session.turn_generation, turn_generation,
        "the turn fence must not restart, or a turn cycle that already ended can release a fresh prompt"
    );
    assert_eq!(state.session.next_user_message_id, message_id);
    assert_eq!(state.session.next_attach_token, attach_token);
}

#[test]
fn a_reply_from_a_previous_generation_never_touches_the_new_session() {
    let mut state = AppState::new(Config::default(), "test".into(), 100, 30);
    state.meta = Some(meta("ses_one"));
    let stale_context = state.session_context();
    let stale = state
        .session
        .begin_user_message(
            stale_context.clone(),
            Request::UserMessage {
                text: "stale".into(),
                images: Vec::new(),
                message_id: None,
            },
        )
        .id;

    state.session.reset(300);
    state.session.turn_boundary_pending = true;
    state.session.retry_requests.clear();
    // The in-flight list is rebuilt by the switch; a reply from the session
    // that was left behind still carries its old ticket.
    state
        .session
        .in_flight_user_messages
        .push_back(UserMessageInFlight {
            id: stale,
            context: stale_context,
            request: Request::DetachTurn,
            sequence: PromptBatch::single(Request::DetachTurn),
            turn_generation: 0,
        });
    state.session.user_msgs_in_flight = 1;

    assert!(
        state.session.finish_user_message(stale, true).is_none(),
        "a stale ticket must not re-queue its batch into the new session"
    );
    assert!(state.session.retry_requests.is_empty());
    assert!(
        state
            .session
            .complete_user_message_step(stale, &Request::DetachTurn)
            .is_none(),
        "a stale ticket must not advance a prompt batch"
    );
    assert!(
        state.session.in_flight_user_messages.is_empty(),
        "a stale ticket must not stay in flight, holding the prompt gate"
    );
    assert_eq!(state.session.user_msgs_in_flight, 0);
    assert!(state.session.turn_boundary_pending);

    let live = state
        .session
        .begin_user_message(
            state.session_context(),
            Request::UserMessage {
                text: "live".into(),
                images: Vec::new(),
                message_id: None,
            },
        )
        .id;
    assert!(state.session.finish_user_message(live, false).is_some());
}
