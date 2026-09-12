//! Event loop and redraw scheduling.
//!
//! One `tokio::select!` over: client events (`ClientEvent`), terminal events
//! (`crossterm::event::EventStream`), the stream tick (paced by `config.ui.ups`,
//! only while `state.needs_stream_tick()`), the second tick (1 s, only while
//! `state.needs_second_tick()`), the animation tick (the pacman interval while
//! the turn runs; 125 ms for ambient pulses; 70 ms while a transient animation —
//! toast slide/fade, overlay unfold, welcome cascade — is on screen), and toast
//! expiry (armed only while toasts exist).
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
    BashCompleted(Result<crate::bash::BashOutput, String>),
}

pub async fn run(opts: TuiOptions) -> Result<pacode_types::SessionId, TuiError> {
    let timing = crate::timing::enabled();
    let t_start = Instant::now();
    log::info!("TUI starting up");

    let t = Instant::now();
    let mut term_guard = TerminalGuard::enter(opts.config.ui.mouse)?;
    crate::timing::mark(timing, "terminal_enter", t);

    // Progressive paint: build the state and draw the first frame before any
    // daemon IO. The socket connect, a possible daemon spawn and the attach
    // snapshot (first history page included) then resolve while the user
    // already sees the chrome and a "connecting…" footer.
    let t = Instant::now();
    let size = term_guard.terminal.size()?;
    let mut state = AppState::new(
        opts.config.clone(),
        opts.app_version.clone(),
        size.width,
        size.height,
    );
    state.cwd = opts.cwd.clone();
    state.connection = crate::state::Connection::Connecting;
    crate::timing::mark(timing, "app_state_new", t);

    term_guard.terminal.draw(|f| {
        ui::draw(f, &mut state);
        crate::font::apply_weight_to_frame(f, &state.config.font);
    })?;
    crate::timing::mark(timing, "first_frame", t_start);

    let client_paths = opts.client.paths.clone();
    state.paths = client_paths;
    let t = Instant::now();
    let (client, mut events) = Client::connect(opts.client).await?;
    crate::timing::mark(timing, "client_connect", t);
    let t = Instant::now();
    let snapshot = client.attach(opts.attach.clone()).await?;
    crate::timing::mark(timing, "attach_snapshot", t);
    let session_id = snapshot.meta.id.clone();
    log::info!("TUI attached to session {session_id}");
    let client = Arc::new(client);

    state.slots[0] = Some(crate::state::SessionSlot {
        id: snapshot.meta.id.clone(),
        title: snapshot.meta.title(),
        turns: snapshot.usage.turns,
        context_tokens: snapshot.usage.context_tokens,
        agents_count: snapshot.agents.len(),
        tasks_count: snapshot.tasks.len(),
    });
    let t = Instant::now();
    state.apply_client_event(ClientEvent::Snapshot(snapshot), Instant::now());
    crate::timing::mark(timing, "apply_snapshot", t);

    let t = Instant::now();
    let font_outcome = term_guard.apply_font(&opts.config.font);
    crate::timing::mark(timing, "apply_font", t);
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
        let _ = client
            .ok(Request::UserMessage {
                text: prompt,
                images: Vec::new(),
            })
            .await;
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

    // `ui.ups = "auto"` probes external tools (hyprctl/wlr-randr/xrandr —
    // measured ~37 ms warm, up to 200 ms per miss). Run it off the critical
    // path and adopt the result when it lands; until then draw uncapped —
    // nothing animates during attach anyway, so no extra frames are spent.
    // `_hz_keepalive` keeps one sender alive so `hz_rx.recv()` in the loop
    // pends forever once the probe result is consumed (a closed channel would
    // return `None` instantly and spin the select loop).
    let t_probe = Instant::now();
    let ups = opts.config.ui.ups;
    let (hz_tx, mut hz_rx) = mpsc::unbounded_channel::<Option<u16>>();
    let _hz_keepalive = hz_tx.clone();
    let mut frame_interval = match ups {
        pacode_types::Ups::Auto => {
            std::thread::spawn(move || {
                let _ = hz_tx.send(pacode_config::display::detect_refresh_hz());
            });
            None
        }
        pacode_types::Ups::Fixed(_) | pacode_types::Ups::Dynamic => ups.frame_interval(None),
    };
    crate::timing::mark(timing, "refresh_hz_spawn", t_probe);

    let mut last_draw = Instant::now() - frame_interval.unwrap_or(Duration::from_millis(33));
    let mut last_layout = ScreenLayout::default();
    let mut active_escape: Option<(ratatui::layout::Rect, std::path::PathBuf)> = None;
    // Input image-strip escapes currently on screen (spec §1): tracked so the
    // post-draw flush can diff and never re-send tens of MB of kitty/sixel
    // payload per frame.
    let mut active_image_escapes: Vec<(ratatui::layout::Rect, String)> = Vec::new();

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
                // kitty d=a deletes every image, including the input strip.
                active_image_escapes.clear();
            }
        }

        // Refresh the activity phase before drawing, so the draw path reads state
        // only and the displayed duration never depends on when a redraw happened.
        state.tick_phase(Instant::now());

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
                        active_image_escapes.clear();
                    }
                    let _ = term_guard.draw_image_escape(area, &esc);
                    active_escape = Some((area, path));
                } else if let Some((old_area, _)) = active_escape.take() {
                    let _ = term_guard.clear_image_area(old_area);
                    active_image_escapes.clear();
                }
            } else if let Some((old_area, _)) = active_escape.take() {
                let _ = term_guard.clear_image_area(old_area);
                active_image_escapes.clear();
            }

            // Input image-strip escapes stashed by `ui::images::draw`. Emitted
            // only on change; a files-overlay clear above emptied the tracker
            // (kitty d=a is global), which forces a re-emit here. Clearing the
            // strip in turn kills the files image for a frame — it re-emits
            // unconditionally every drawn frame.
            let next_escapes = std::mem::take(&mut state.pending_image_escapes);
            if next_escapes != active_image_escapes {
                for (old_area, _) in active_image_escapes.drain(..) {
                    let _ = term_guard.clear_image_area(old_area);
                }
                for (area, esc) in &next_escapes {
                    let _ = term_guard.draw_image_escape(*area, esc);
                }
                active_image_escapes = next_escapes;
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

        // The second tick is armed to the next boundary of the displayed clock, not
        // to a full second from now. Every other timer in this select! cancels and
        // re-creates this future; measuring from `now` meant a plain 1 s sleep was
        // restarted by each animation frame and never fired, so the duration only
        // moved on unrelated redraws and read 0.1 -> 0.3 -> 0.5 instead of ticking.
        let needs_second = state.needs_second_tick() || state.phase.is_some();
        let next_boundary_ms = crate::state::activity::ms_to_next_second(state.phase_elapsed_ms);
        let second_sleep = async {
            if needs_second {
                tokio::time::sleep(Duration::from_millis(next_boundary_ms)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        let anim_sleep = async {
            // Every arm of this sleep is a visible animation; while nothing
            // animates, the future stays pending and no timer runs.
            let now = Instant::now();
            let fast = state.fast_anim_active(now);
            if state.turn_active || state.transcript.has_backlog() {
                let mut interval_ms = crate::ui::anim::pacman_interval_ms(
                    state.transcript.stream_backlog_chars(),
                    state.transcript.has_pending_final(),
                );
                if fast {
                    interval_ms = interval_ms.min(crate::ui::anim::TRANSIENT_TICK_MS);
                }
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            } else if state.needs_anim_tick(now) || state.toast_needs_anim(now) {
                let interval_ms = if fast {
                    crate::ui::anim::TRANSIENT_TICK_MS
                } else {
                    125
                };
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
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
                        if let Some(req) = state.drain_prompt_queue() {
                            dispatch_action(Action::Send(req), &mut state, &client, &bg_tx);
                        }
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
                        Event::Paste(text) => {
                            crate::paste::handle_paste(&mut state, text, Instant::now())
                        }
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
            detected = hz_rx.recv() => {
                if let Some(hz) = detected {
                    crate::timing::mark(timing, "refresh_hz_detect", t_probe);
                    frame_interval = ups.frame_interval(hz);
                }
            }
            _ = stream_sleep => {
                state.tick_stream(Instant::now());
            }
            _ = second_sleep => {
                let now = Instant::now();
                // The 1 s blink phase for rail pulses that run while the fast
                // anim tick does not (idle main, background work alive).
                state.second_parity = !state.second_parity;
                if state.rail.has_live_agents() || state.rail.has_running_tasks() {
                    state.dirty = true;
                }
                if state.rail.update_idle(state.turn_active, now) {
                    state.dirty = true;
                }
                // Marks the frame dirty itself when the whole-second reading changed.
                state.tick_phase(now);
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
    log::info!("TUI loop ended, quitting session {attached_session}");
    if let Some((area, _)) = active_escape.take() {
        let _ = term_guard.clear_image_area(area);
    }
    for (area, _) in active_image_escapes.drain(..) {
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
            // Resolve `[Image N]` paste tokens into `@<path>` text refs plus
            // `ImageSource` entries (spec §1). This intercept covers every
            // submit path: direct submit, `drain_prompt_queue`, plugin sends.
            let req = match req {
                Request::UserMessage { text, images } => {
                    let (text, resolved) =
                        crate::state::images::resolve_image_tokens(&state.pasted_images, &text);
                    if !resolved.is_empty() {
                        state.pasted_images.mark_submitted(&text);
                    }
                    let mut images = images;
                    images.extend(resolved);
                    Request::UserMessage { text, images }
                }
                other => other,
            };
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(req).await;
                let _ = tx.send(BgResponse::Reply(res));
            });
        }
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
            state.transcript.cells.push_back(Cell {
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
            });
            state.transcript.scroll_to_bottom();
            state.dirty = true;
        }
        Action::RunBashCaptured { command } => {
            log::info!("bash-mode captured start: {command}");
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            state.running_bash = Some(cancel_tx);
            state.dirty = true;
            let tx = bg_tx.clone();
            let cwd = state.cwd();
            tokio::spawn(async move {
                let res = crate::bash::run_captured(&command, &cwd, cancel_rx).await;
                let _ = tx.send(BgResponse::BashCompleted(res));
            });
        }
        Action::GitLog { after } => {
            state.git.begin_log(after.as_deref());
            state.dirty = true;
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitLog { after }).await;
                let _ = tx.send(BgResponse::Reply(res));
            });
        }
        Action::GitShow { hash } => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitShow { hash }).await;
                let _ = tx.send(BgResponse::Reply(res));
            });
        }
        Action::GitDiff { hash } => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitDiff { hash }).await;
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
            log::debug!("history page requested: before_seq={first_cell_id:?}, limit={limit}");
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
        BgResponse::Reply(Ok(Reply::AuthStatus { providers })) => {
            state.auth_providers = providers;
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
        BgResponse::Reply(Ok(Reply::MarketplacePlugins {
            plugins: listing,
            stale: is_stale,
        })) => {
            if let crate::state::Focus::Overlay(crate::state::Overlay::PluginsPicker {
                ref mut market,
                ref mut loading,
                ref mut stale,
                ref mut index,
                ..
            }) = state.focus
            {
                *index = (*index).min(listing.len().saturating_sub(1));
                *market = listing;
                *loading = false;
                *stale = is_stale;
                state.dirty = true;
            }
            vec![]
        }
        BgResponse::Reply(Ok(Reply::GitLog {
            commits,
            has_more,
            status,
        })) => {
            state.git.apply_log(commits, has_more, status);
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::GitShow { hash, stat })) => {
            state.git.apply_show(hash, stat);
            state.dirty = true;
            vec![]
        }
        BgResponse::Reply(Ok(Reply::GitDiff {
            hash,
            patch,
            truncated,
        })) => {
            state.git.apply_diff(hash, patch, truncated);
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
                vec![Action::Send(Request::UserMessage {
                    text,
                    images: Vec::new(),
                })]
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
                    log::debug!(
                        "history page loaded: {} items, has_more={has_more}",
                        items.len()
                    );
                    state.transcript.prepend(items, has_more);
                    state.dirty = true;
                }
                _ => {
                    log::warn!("history page request failed or returned non-history reply");
                    state.transcript.loading_history = false;
                }
            }
            vec![]
        }
        BgResponse::BashCompleted(res) => {
            state.running_bash = None;
            state.dirty = true;
            match res {
                Ok(out) => {
                    log::debug!(
                        "bash-mode captured completed: exit_code={:?}, truncated={}",
                        out.exit_code,
                        out.truncated
                    );
                    let now = now_ms();
                    state.transcript.cells.push_back(Cell {
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
                    });
                    state.transcript.scroll_to_bottom();
                }
                Err(err) => {
                    log::warn!("bash-mode captured error: {err}");
                    let now = now_ms();
                    state.transcript.cells.push_back(Cell {
                        id: now,
                        kind: CellKind::Item(TranscriptKind::Notice {
                            level: ToastLevel::Error,
                            text: err,
                        }),
                        version: 0,
                        ts_ms: now,
                        stats: None,
                    });
                    state.transcript.scroll_to_bottom();
                }
            }
            vec![]
        }
    }
}
