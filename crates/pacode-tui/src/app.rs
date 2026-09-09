//! Event loop and redraw scheduling.
//!
//! One `tokio::select!` over: client events (`ClientEvent`), terminal events
//! (`crossterm::event::EventStream`), the stream tick (paced by `config.ui.ups`,
//! only while `state.needs_stream_tick()`), the second tick (1 s, only while
//! `state.needs_second_tick()`), and toast expiry (armed only while toasts exist).
//! A frame is drawn when `state.dirty` and the frame interval has passed since the
//! last frame (otherwise a deferred draw is scheduled). Nothing ticks in the idle state.
//!
//! Startup: connect (spawning the daemon), attach, apply the snapshot, send
//! `initial_prompt` if any, then loop. On `quit`: leave the alt screen and close the
//! client (the daemon and its sessions keep running).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

use pacode_client::{Client, ClientEvent};
use pacode_types::state::ToastLevel;
use pacode_types::time::now_ms;
use pacode_types::{AgentId, PluginCommandOutcome, Reply, Request, TranscriptKind};

use crate::keys::{Action, handle_key, handle_mouse};
use crate::layout::ScreenLayout;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, PanelTarget, TOAST_TTL_SECS};
use crate::terminal::TerminalGuard;
use crate::ui;
use crate::{TuiError, TuiOptions};

enum BgResponse {
    Reply(Result<Reply, pacode_client::ClientError>),
    LoadPanelReply {
        target: PanelTarget,
        result: Result<Reply, pacode_client::ClientError>,
    },
    LoadHistoryReply {
        result: Result<Reply, pacode_client::ClientError>,
    },
}

pub async fn run(opts: TuiOptions) -> Result<pacode_types::SessionId, TuiError> {
    let mut term_guard = TerminalGuard::enter(opts.config.ui.mouse)?;

    let client_paths = opts.client.paths.clone();
    let (client, mut events) = Client::connect(opts.client).await?;
    let snapshot = client.attach(opts.attach.clone()).await?;
    let session_id = snapshot.meta.id.clone();
    let client = Arc::new(client);

    let size = term_guard.terminal.size()?;
    let mut state = AppState::new(
        opts.config.clone(),
        opts.app_version.clone(),
        size.width,
        size.height,
    );
    state.paths = client_paths;
    state.cwd = opts.cwd.clone();
    state.slots[0] = Some(crate::state::SessionSlot {
        id: snapshot.meta.id.clone(),
        title: snapshot.meta.title(),
        turns: snapshot.usage.turns,
        context_tokens: snapshot.usage.context_tokens,
        agents_count: snapshot.agents.len(),
        tasks_count: snapshot.tasks.len(),
    });
    state.apply_client_event(ClientEvent::Snapshot(snapshot), Instant::now());

    let font_outcome = term_guard.apply_font(&opts.config.font);
    if !font_outcome.unsupported.is_empty() {
        let items = font_outcome.unsupported.join(", ");
        state.push_toast(
            ToastLevel::Info,
            format!("font: this terminal has no {items} control"),
            None,
            Instant::now(),
        );
    }
    for warning in std::mem::take(&mut state.keymap_warnings) {
        state.push_toast(ToastLevel::Warn, warning, None, Instant::now());
    }

    if let Some(prompt) = opts.initial_prompt {
        let _ = client.ok(Request::UserMessage { text: prompt }).await;
    }

    let mut term_events = EventStream::new();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<BgResponse>();

    // On start (after attach), send ListPlugins and register commands
    {
        let cl = Arc::clone(&client);
        let tx = bg_tx.clone();
        tokio::spawn(async move {
            let res = cl.request(Request::ListPlugins).await;
            let _ = tx.send(BgResponse::Reply(res));
        });
    }

    let detected_hz = match opts.config.ui.ups {
        pacode_types::Ups::Auto => pacode_config::display::detect_refresh_hz(),
        pacode_types::Ups::Fixed(_) | pacode_types::Ups::Dynamic => None,
    };
    let frame_interval = opts.config.ui.ups.frame_interval(detected_hz);

    let mut last_draw = Instant::now() - frame_interval.unwrap_or(Duration::from_millis(33));
    let mut last_layout = ScreenLayout::default();
    let mut active_escape: Option<(ratatui::layout::Rect, std::path::PathBuf)> = None;

    loop {
        let is_files_overlay = matches!(
            state.focus,
            crate::state::Focus::Overlay(crate::state::Overlay::Files { .. })
        );
        if !is_files_overlay {
            if state.files.cached_preview.is_some() {
                state.files.clear_preview();
            }
            if let Some((area, _path)) = active_escape.take() {
                let _ = term_guard.clear_image_area(area);
            }
        }

        // Redraw if dirty and frame interval elapsed
        let now = Instant::now();
        let can_draw = match frame_interval {
            Some(interval) => now.saturating_duration_since(last_draw) >= interval,
            None => true,
        };
        if state.dirty && can_draw {
            term_guard.terminal.draw(|f| {
                last_layout = ui::draw(f, &mut state);
                crate::font::apply_weight_to_frame(f, &state.config.font);
            })?;
            state.dirty = false;
            last_draw = Instant::now();

            if is_files_overlay {
                if let Some((area, esc, path)) = state.files.pending_escape.take() {
                    if let Some((old_area, ref old_path)) = active_escape
                        && (*old_path != path || old_area != area)
                    {
                        let _ = term_guard.clear_image_area(old_area);
                    }
                    let _ = term_guard.draw_image_escape(area, &esc);
                    active_escape = Some((area, path));
                } else if let Some((old_area, _)) = active_escape.take() {
                    let _ = term_guard.clear_image_area(old_area);
                }
            } else if let Some((old_area, _)) = active_escape.take() {
                let _ = term_guard.clear_image_area(old_area);
            }
        }

        if state.quit {
            break;
        }

        // Compute timers dynamically
        let needs_stream = state.needs_stream_tick();
        let stream_tick_dur = frame_interval.unwrap_or(Duration::from_millis(33));
        let stream_sleep = async {
            if needs_stream {
                tokio::time::sleep(stream_tick_dur).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let needs_second = state.needs_second_tick();
        let second_sleep = async {
            if needs_second {
                tokio::time::sleep(Duration::from_secs(1)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let anim_sleep = async {
            if state.turn_active || state.transcript.has_backlog() {
                let interval_ms = crate::ui::anim::pacman_interval_ms(
                    state.transcript.stream_backlog_chars(),
                    state.transcript.has_pending_final(),
                );
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            } else if state.needs_anim_tick() {
                tokio::time::sleep(Duration::from_millis(125)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let ctrl_c_expiry_sleep = async {
            if let Some(t) = state.ctrl_c_at {
                let dur = Duration::from_secs(2);
                let elapsed = Instant::now().saturating_duration_since(t);
                if elapsed < dur {
                    tokio::time::sleep(dur - elapsed).await;
                }
            } else {
                std::future::pending::<()>().await;
            }
        };

        let toast_expiry_sleep = async {
            if let Some(oldest) = state.toasts.front() {
                let dur = Duration::from_secs(TOAST_TTL_SECS);
                let elapsed = Instant::now().saturating_duration_since(oldest.shown_at);
                let remaining = dur.saturating_sub(elapsed);
                tokio::time::sleep(remaining).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let deferred_draw_sleep = async {
            if state.dirty {
                if let Some(interval) = frame_interval {
                    let elapsed = Instant::now().saturating_duration_since(last_draw);
                    let remaining = interval.saturating_sub(elapsed);
                    tokio::time::sleep(remaining).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            client_ev = events.recv() => {
                match client_ev {
                    Some(ev) => {
                        state.apply_client_event(ev, Instant::now());
                    }
                    None => {
                        break;
                    }
                }
            }
            term_ev = term_events.next() => {
                if let Some(Ok(ev)) = term_ev {
                    let actions = match ev {
                        Event::Key(key) => handle_key(&mut state, key, Instant::now()),
                        Event::Mouse(mouse) => handle_mouse(&mut state, mouse, &last_layout),
                        Event::Resize(w, h) => {
                            state.cols = w;
                            state.rows = h;
                            state.dirty = true;
                            vec![]
                        }
                        _ => vec![],
                    };
                    for act in actions {
                        dispatch_action(act, &mut state, &client, &bg_tx);
                    }
                }
            }
            bg_res = bg_rx.recv() => {
                if let Some(res) = bg_res {
                    let actions = handle_bg_response(res, &mut state);
                    for act in actions {
                        dispatch_action(act, &mut state, &client, &bg_tx);
                    }
                }
            }
            _ = stream_sleep => {
                state.tick_stream(Instant::now());
            }
            _ = second_sleep => {
                let now = Instant::now();
                if state.rail.update_idle(state.turn_active, now) {
                    state.dirty = true;
                }
            }
            _ = anim_sleep => {
                state.anim_frame = state.anim_frame.wrapping_add(1);
                state.dirty = true;
            }
            _ = ctrl_c_expiry_sleep => {
                state.ctrl_c_at = None;
                state.dirty = true;
            }
            _ = toast_expiry_sleep => {
                state.expire_toasts(Instant::now());
            }
            _ = deferred_draw_sleep => {
                // Loop iteration triggers draw
            }
        }
    }

    let attached_session = state
        .meta
        .as_ref()
        .map(|m| m.id.clone())
        .unwrap_or(session_id);
    if let Some((area, _)) = active_escape.take() {
        let _ = term_guard.clear_image_area(area);
    }
    drop(events);
    drop(bg_rx);
    if let Ok(c) = Arc::try_unwrap(client) {
        c.close().await;
    }
    Ok(attached_session)
}

fn dispatch_action(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    match act {
        Action::Quit => {
            state.quit = true;
        }
        Action::Send(req) => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(req).await;
                let _ = tx.send(BgResponse::Reply(res));
            });
        }
        Action::LoadPanel => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            if let Some(ref target) = state.panel.target {
                let target = target.clone();
                tokio::spawn(async move {
                    let res = match &target {
                        PanelTarget::Agent(id) => {
                            cl.request(Request::GetHistory {
                                agent: id.clone(),
                                before_seq: None,
                                limit: 300,
                            })
                            .await
                        }
                        PanelTarget::Task(id) => {
                            cl.request(Request::GetTaskOutput {
                                task: id.clone(),
                                tail_lines: 300,
                            })
                            .await
                        }
                    };
                    let _ = tx.send(BgResponse::LoadPanelReply {
                        target,
                        result: res,
                    });
                });
            }
        }
        Action::LoadHistory => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            let first_cell_id = state.transcript.oldest_seq();
            let limit = state.config.session.history_page;
            state.transcript.loading_history = true;
            tokio::spawn(async move {
                let res = cl
                    .request(Request::GetHistory {
                        agent: AgentId::main(),
                        before_seq: first_cell_id,
                        limit,
                    })
                    .await;
                let _ = tx.send(BgResponse::LoadHistoryReply { result: res });
            });
        }
    }
}

fn handle_bg_response(res: BgResponse, state: &mut AppState) -> Vec<Action> {
    match res {
        BgResponse::Reply(Ok(Reply::Error { message })) => {
            let now = now_ms();
            state.transcript.cells.push_back(Cell {
                id: now,
                kind: CellKind::Item(TranscriptKind::Notice {
                    level: ToastLevel::Error,
                    text: message,
                }),
                version: 0,
                ts_ms: now,
                stats: None,
            });
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::Models { models })) => {
            state.models = models;
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::Sessions { sessions })) => {
            state.sessions = sessions;
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::McpServers { servers })) => {
            crate::commands::register_mcp_servers(&servers);
            if let crate::state::Focus::Overlay(crate::state::Overlay::McpPicker {
                servers: ref mut s,
                loading: ref mut l,
                ..
            }) = state.focus
            {
                *s = servers;
                *l = false;
                state.dirty = true;
            }
            vec![]
        }
        BgResponse::Reply(Ok(Reply::McpPrompt { text })) => {
            state.input.insert_str(&text);
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::Plugins { plugins })) => {
            crate::commands::register_plugins(&plugins);
            state.plugins = plugins.clone();
            if let crate::state::Focus::Overlay(crate::state::Overlay::PluginsPicker {
                plugins: ref mut p,
                ..
            }) = state.focus
            {
                *p = plugins;
                state.dirty = true;
            }
            vec![]
        }
        BgResponse::Reply(Ok(Reply::PluginCommand(outcome))) => match outcome {
            PluginCommandOutcome::InsertText { text } => {
                state.input.insert_str(&text);
                state.dirty = true;
                vec![]
            }
            PluginCommandOutcome::SendPrompt { text } => {
                vec![Action::Send(Request::UserMessage { text })]
            }
            PluginCommandOutcome::Nothing => vec![],
        },
        BgResponse::Reply(Ok(Reply::Attached(snapshot))) => {
            let slot_idx = state.active_slot;
            state.record_slot(
                slot_idx,
                &snapshot.meta,
                &snapshot.usage,
                snapshot.agents.len(),
                snapshot.tasks.len(),
            );
            state.apply_client_event(ClientEvent::Snapshot(snapshot), Instant::now());
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::Snapshot(snapshot))) => {
            let slot_idx = state.active_slot;
            state.record_slot(
                slot_idx,
                &snapshot.meta,
                &snapshot.usage,
                snapshot.agents.len(),
                snapshot.tasks.len(),
            );
            state.apply_client_event(ClientEvent::Snapshot(snapshot), Instant::now());
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(_)) => vec![],
        BgResponse::Reply(Err(err)) => {
            let now = now_ms();
            state.transcript.cells.push_back(Cell {
                id: now,
                kind: CellKind::Item(TranscriptKind::Notice {
                    level: ToastLevel::Error,
                    text: err.to_string(),
                }),
                version: 0,
                ts_ms: now,
                stats: None,
            });
            state.dirty = true;
            vec![]
        }
        BgResponse::LoadPanelReply { target, result } => {
            match result {
                Ok(Reply::History {
                    items, has_more, ..
                }) => {
                    if state.panel.target == Some(target) {
                        state.panel.agent_transcript.reset(items, has_more);
                        state.dirty = true;
                    }
                }
                Ok(Reply::TaskOutput {
                    lines, total_lines, ..
                }) => {
                    if state.panel.target == Some(target) {
                        state.panel.task_lines = lines;
                        state.panel.task_total_lines = total_lines;
                        state.dirty = true;
                    }
                }
                Ok(Reply::Error { message }) => {
                    state.push_toast(
                        ToastLevel::Error,
                        "Panel load error".into(),
                        Some(message),
                        Instant::now(),
                    );
                }
                Err(e) => {
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
        BgResponse::LoadHistoryReply { result } => {
            match result {
                Ok(Reply::History {
                    items, has_more, ..
                }) => {
                    state.transcript.prepend(items, has_more);
                    state.dirty = true;
                }
                _ => {
                    state.transcript.loading_history = false;
                }
            }
            vec![]
        }
    }
}
