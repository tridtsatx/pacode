//! Event loop and redraw scheduling.
//!
//! One `tokio::select!` over: client events (`ClientEvent`), terminal events
//! (`crossterm::event::EventStream`), the stream tick (33 ms, only while
//! `state.needs_stream_tick()`), the second tick (1 s, only while
//! `state.needs_second_tick()`), and toast expiry (armed only while toasts exist).
//! A frame is drawn when `state.dirty` and at least 16 ms passed since the last frame
//! (otherwise a deferred draw is scheduled). Nothing ticks in the idle state.
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
use pacode_types::{AgentId, Reply, Request, TranscriptKind};

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

pub async fn run(opts: TuiOptions) -> Result<(), TuiError> {
    let mut term_guard = TerminalGuard::enter(opts.config.ui.mouse)?;

    let client_paths = opts.client.paths.clone();
    let (client, mut events) = Client::connect(opts.client).await?;
    let snapshot = client.attach(opts.attach.clone()).await?;
    let client = Arc::new(client);

    let size = term_guard.terminal.size()?;
    let mut state = AppState::new(
        opts.config.clone(),
        opts.app_version.clone(),
        size.width,
        size.height,
    );
    state.paths = client_paths;
    state.apply_client_event(ClientEvent::Snapshot(snapshot), Instant::now());

    if let Some(prompt) = opts.initial_prompt {
        let _ = client.ok(Request::UserMessage { text: prompt }).await;
    }

    let mut term_events = EventStream::new();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<BgResponse>();

    let mut last_draw = Instant::now() - Duration::from_millis(20);
    let mut last_layout = ScreenLayout::default();

    loop {
        // Redraw if dirty and >= 16 ms since last draw
        let now = Instant::now();
        if state.dirty && now.saturating_duration_since(last_draw) >= Duration::from_millis(16) {
            term_guard.terminal.draw(|f| {
                last_layout = ui::draw(f, &mut state);
            })?;
            state.dirty = false;
            last_draw = Instant::now();
        }

        if state.quit {
            break;
        }

        // Compute timers dynamically
        let needs_stream = state.needs_stream_tick();
        let stream_sleep = async {
            if needs_stream {
                tokio::time::sleep(Duration::from_millis(33)).await;
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
            if state.needs_anim_tick() {
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
                let elapsed = Instant::now().saturating_duration_since(last_draw);
                let remaining = Duration::from_millis(16).saturating_sub(elapsed);
                tokio::time::sleep(remaining).await;
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
                    handle_bg_response(res, &mut state);
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

    drop(events);
    drop(bg_rx);
    if let Ok(c) = Arc::try_unwrap(client) {
        c.close().await;
    }
    Ok(())
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
            let first_cell_id = state.transcript.cells.front().map(|c| c.id);
            state.transcript.loading_history = true;
            tokio::spawn(async move {
                let res = cl
                    .request(Request::GetHistory {
                        agent: AgentId::main(),
                        before_seq: first_cell_id,
                        limit: 100,
                    })
                    .await;
                let _ = tx.send(BgResponse::LoadHistoryReply { result: res });
            });
        }
    }
}

fn handle_bg_response(res: BgResponse, state: &mut AppState) {
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
        }
        BgResponse::Reply(Ok(Reply::Models { models })) => {
            state.models = models;
            state.dirty = true;
        }
        BgResponse::Reply(Ok(Reply::Sessions { sessions })) => {
            state.sessions = sessions;
            state.dirty = true;
        }
        BgResponse::Reply(Ok(_)) => {}
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
        }
        BgResponse::LoadPanelReply { target, result } => match result {
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
        },
        BgResponse::LoadHistoryReply { result } => match result {
            Ok(Reply::History {
                items, has_more, ..
            }) => {
                state.transcript.prepend(items, has_more);
                state.dirty = true;
            }
            _ => {
                state.transcript.loading_history = false;
            }
        },
    }
}
