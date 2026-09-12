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
    Reply {
        /// Which cached list the request refreshed — `None` for non-list
        /// requests. Drives `state.lists` freshness/in-flight tracking (§8.2).
        kind: Option<crate::cache::ListKind>,
        res: Result<Reply, pacode_client::ClientError>,
    },
    LoadPanelReply {
        target: PanelTarget,
        result: Result<Reply, pacode_client::ClientError>,
    },
    LoadHistoryReply {
        result: Result<Reply, pacode_client::ClientError>,
    },
    BashCompleted(Result<crate::bash::BashOutput, String>),
    /// An `@` path-completion scan finished — the popup reads `ready`.
    AtCompleted {
        key: crate::state::input::AtKey,
        candidates: Vec<crate::at_complete::AtCandidate>,
    },
    /// A `!` bash-completion scan finished.
    BashCompletionDone {
        key: crate::state::input::BashKey,
        candidates: Vec<crate::bash::complete::Candidate>,
    },
    /// A files preview finished (§8.1: the read + lex ran on a blocking
    /// thread, never in draw).
    FilePreviewReady {
        key: crate::state::files::PreviewKey,
        resolved: std::path::PathBuf,
        text: Option<crate::state::files::TextPreview>,
    },
    /// The Files level's deferred `git status` scan landed — the
    /// `new`/`deleted`/`renamed` labels (§5.5). `at_version` is the
    /// `files.rows_version` the scan described; a later row change rescans.
    FileStatusesReady {
        statuses: std::collections::BTreeMap<String, pacode_vcs::FileStatus>,
        at_version: u64,
    },
    /// The Files level's `enter` probe landed — applies only while its path
    /// is still the panel's `FileDiff` target (a stale one drops).
    FileDiffReady {
        path: String,
        diff: pacode_vcs::FileDiff,
    },
    /// `Request::GitShow` finished (§5.10). Carries the request's hash so a
    /// `Reply::Error` — which names no commit — still lands on the right row:
    /// a hover-driven fetch must not spam the transcript, it folds into the
    /// detail slot instead.
    GitShowReady {
        hash: String,
        res: Result<Reply, pacode_client::ClientError>,
    },
    /// `Request::GitDiff` finished — the commit-diff panel's payload, keyed
    /// by the request's hash for the same reason as [`Self::GitShowReady`].
    GitDiffReady {
        hash: String,
        res: Result<Reply, pacode_client::ClientError>,
    },
    /// The Background level's output-tail read finished (§5.9): the last ≤
    /// `ui::menu::background::TAIL_CAP_BYTES` of the task's spool file, read
    /// on a blocking thread — never in the draw path (§8.1).
    TaskTailReady {
        tail: crate::state::rail::TaskTail,
    },
    /// A pasted-image strip thumbnail finished rendering.
    ThumbReady {
        token: usize,
        cell_w: u16,
        cell_h: u16,
        preview: Option<pacode_image::Preview>,
    },
    /// The clipboard copy `ui::draw` stashed in `state.pending_copy` ran.
    CopyDone {
        auto: bool,
        chars: usize,
    },
}

pub async fn run(opts: TuiOptions) -> Result<pacode_types::SessionId, TuiError> {
    let timing = crate::timing::enabled();
    let t_start = Instant::now();
    log::info!("TUI starting up");

    let t = Instant::now();
    let mut term_guard = TerminalGuard::enter(opts.config.ui.mouse)?;
    crate::timing::mark(timing, "terminal_enter", t);

    // Progressive paint, now cache-first (§8.2): build the state, run the one
    // allowed blocking cache read on `spawn_blocking`, apply whatever came
    // back, then draw the first frame — all before any daemon IO. The socket
    // connect, a possible daemon spawn and the attach snapshot then resolve
    // while the user already sees the chrome and a "connecting…" footer.
    let t = Instant::now();
    let size = term_guard.terminal.size()?;
    let mut state = AppState::new(
        opts.config.clone(),
        opts.app_version.clone(),
        size.width,
        size.height,
    );
    state.cwd = opts.cwd.clone();
    state.paths = opts.client.paths.clone();
    state.cache = crate::cache::DiskCache::new(state.paths.cache_dir.clone());
    state.connection = crate::state::Connection::Connecting;
    crate::timing::mark(timing, "app_state_new", t);

    let t = Instant::now();
    let load = {
        let cache = state.cache.clone();
        let ctx = crate::cache::LoadCtx {
            cwd: opts.cwd.clone(),
            mcp_sig: crate::cache::mcp_sig(&opts.config),
            marketplace_source: opts.config.plugins.marketplace.clone(),
        };
        tokio::task::spawn_blocking(move || crate::cache::startup_load(&cache, &ctx))
            .await
            .unwrap_or_default()
    };
    if load.caps.evicted > 0 {
        log::info!(
            "cache: evicted {} file(s), {} B freed",
            load.caps.evicted,
            load.caps.freed_bytes
        );
    }
    let session_start = state.session_start_ms;
    crate::cache::apply_snapshot(&mut state, load.snapshot, session_start);
    crate::timing::mark(timing, "cache_load", t);

    term_guard.terminal.draw(|f| {
        ui::draw(f, &mut state);
        crate::font::apply_weight_to_frame(f, &state.config.font);
    })?;
    crate::timing::mark(timing, "first_frame", t_start);

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

    // §8.2: every post-attach list refresh runs in parallel — none may block
    // a frame. Each reply folds in independently, replaces its state section
    // wholesale and rewrites its cache file. (`ListSkills` does not exist in
    // the protocol: skills ride inside `PluginInfo.skill_dirs`.)
    for act in [
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListMcpServers),
        Action::Send(Request::ListSessions { limit: 50 }),
        Action::Send(Request::ListModels),
        Action::GitLog { after: None },
    ] {
        dispatch_action(act, &mut state, &client, &bg_tx);
    }
    if !state.config.plugins.marketplace.is_empty() {
        let source = state.config.plugins.marketplace.clone();
        dispatch_action(
            Action::Send(Request::BrowseMarketplace {
                source,
                query: String::new(),
            }),
            &mut state,
            &client,
            &bg_tx,
        );
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
    // The refresh rate the auto-probe reported — `None` until it lands
    // (`frame_budget` then budgets for the 60 Hz floor).
    let mut detected_hz: Option<u16> = None;
    let mut last_layout = ScreenLayout::default();
    // Input image-strip escapes currently on screen (spec §1): tracked so the
    // post-draw flush can diff and never re-send tens of MB of kitty/sixel
    // payload per frame.
    let mut active_image_escapes: Vec<(ratatui::layout::Rect, String)> = Vec::new();

    loop {
        let files_view = files_view_on_screen(&state);
        if !files_view {
            if state.files.cached_preview.is_some() {
                state.files.clear_preview();
            }
            // Reopening rescans: `statused_at` records the rows_version the
            // labels describe — dropping it re-arms the deferred status job.
            state.files.statused_at = None;
        }

        // The Background level's output-tail cache lives only while the
        // level is on screen — reopening re-arms it from scratch (§5.9).
        if !background_view_on_screen(&state) {
            state.rail.clear_task_tail();
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
            let t_frame = Instant::now();
            term_guard.terminal.draw(|f| {
                last_layout = ui::draw(f, &mut state);
                crate::font::apply_weight_to_frame(f, &state.config.font);
            })?;
            // §8.3: warn when a frame ran over its refresh budget.
            crate::timing::warn_frame_over_budget(
                timing,
                t_frame.elapsed(),
                crate::timing::frame_budget(detected_hz),
            );
            state.dirty = false;
            last_draw = Instant::now();

            // Input image-strip escapes stashed by `ui::images::draw` —
            // emitted only on change.
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

        // Deferred work the draw path recorded: completion scans, image
        // renders, the stashed clipboard copy — all of it runs on blocking
        // threads, replies land back through `bg_rx` (§8.1). Daemon-bound
        // wants come back as actions and go through `dispatch_action`.
        for act in dispatch_deferred(&mut state, &bg_tx) {
            dispatch_action(act, &mut state, &client, &bg_tx);
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

        // The remote-filter debounce of the open menu (§3.2): armed only
        // while a level owes the daemon a query — no timer runs otherwise.
        let menu_remote_sleep = async {
            match crate::keys_menu::remote_deadline(&state) {
                Some(at) => {
                    let remaining = at.saturating_duration_since(Instant::now());
                    tokio::time::sleep(remaining).await;
                }
                None => std::future::pending::<()>().await,
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
                    detected_hz = hz;
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
            _ = menu_remote_sleep => {
                let now = Instant::now();
                let actions = crate::keys_menu::remote_filter_due(&mut state, now);
                for act in actions {
                    dispatch_action(act, &mut state, &client, &bg_tx);
                }
            }
        }
    }

    let attached_session = state
        .meta
        .as_ref()
        .map(|m| m.id.clone())
        .unwrap_or(session_id);
    log::info!("TUI loop ended, quitting session {attached_session}");
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
            // §8.2: a list request marks its section in-flight (the pickers
            // show `loading…` until the reply clears it).
            let kind = crate::cache::ListKind::for_request(&req);
            if let Some(kind) = kind {
                state.lists.mark_sent(kind);
            }
            // §8.2 invalidation: a mutation drops the affected list's file
            // now; the follow-up `ListX` reply rewrites it.
            match &req {
                Request::InstallPlugin { .. } | Request::UninstallPlugin { .. } => {
                    state.invalidate_cache(crate::cache::ListKind::Plugins);
                    state.invalidate_cache(crate::cache::ListKind::Commands);
                }
                Request::SetMcpServerEnabled { .. } | Request::RestartMcpServer { .. } => {
                    state.invalidate_cache(crate::cache::ListKind::McpServers);
                    state.invalidate_cache(crate::cache::ListKind::Commands);
                }
                _ => {}
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(req).await;
                let _ = tx.send(BgResponse::Reply { kind, res });
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
            state.lists.mark_sent(crate::cache::ListKind::GitLog);
            state.dirty = true;
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitLog { after }).await;
                let _ = tx.send(BgResponse::Reply {
                    kind: Some(crate::cache::ListKind::GitLog),
                    res,
                });
            });
        }
        Action::GitShow { hash } => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitShow { hash: hash.clone() }).await;
                let _ = tx.send(BgResponse::GitShowReady { hash, res });
            });
        }
        Action::GitDiff { hash } => {
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl.request(Request::GitDiff { hash: hash.clone() }).await;
                let _ = tx.send(BgResponse::GitDiffReady { hash, res });
            });
        }
        Action::LoadPanel => {
            if let Some(ref target) = state.panel.target {
                let target = target.clone();
                // The MCP log is read locally off the daemon log file by
                // `menu::mcp` — there is no daemon request behind the target.
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
                    // The MCP log and the file diff are produced locally; the
                    // commit diff was already requested by the Git level's
                    // `enter` — `LoadPanel` fires for neither.
                    PanelTarget::McpServer(_)
                    | PanelTarget::FileDiff { .. }
                    | PanelTarget::CommitDiff { .. } => None,
                };
                if let Some(request) = request {
                    let cl = Arc::clone(client);
                    let tx = bg_tx.clone();
                    tokio::spawn(async move {
                        let res = cl.request(request).await;
                        let _ = tx.send(BgResponse::LoadPanelReply {
                            target,
                            result: res,
                        });
                    });
                }
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

/// Service the work items the draw path recorded this pass: completion scans,
/// image preview/thumbnail renders, the stashed clipboard copy. Everything
/// lands on `spawn_blocking` — the frame path itself stays clean (§8.1).
/// Daemon-bound wants return as actions for `dispatch_action` instead — they
/// share the request's bookkeeping (`begin_log`, `mark_sent`).
fn dispatch_deferred(
    state: &mut AppState,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) -> Vec<Action> {
    if let Some(key) = state.input.at_comp.claim_want() {
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates = crate::at_complete::complete_at_path(&key.query, &key.cwd);
            let _ = tx.send(BgResponse::AtCompleted { key, candidates });
        });
    }

    if let Some(key) = state.input.bash_comp.claim_want() {
        // The history snapshot rides along — the scan must not see the deque
        // grow mid-read, and the key's `history_len` invalidates anyway.
        let history = state.input.bash_history.clone();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates =
                crate::bash::complete::complete(&key.text, key.cursor, &history, &key.cwd);
            let _ = tx.send(BgResponse::BashCompletionDone { key, candidates });
        });
    }

    // Files preview: resolve + read/lex off-thread for the menu detail card
    // (§8.1 — the draw only records `preview_want`).
    if let Some(want) = state.files.preview_want.clone()
        && state.files.preview_in_flight.as_ref() != Some(&want)
        && state.files.cached_preview.as_ref().map(|c| &c.key) != Some(&want)
    {
        state.files.preview_in_flight = Some(want.clone());
        let cwd = state.cwd();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let resolved = resolve_preview_path(&want.raw, &cwd);
            let text = crate::ui::menu::files::text_preview(&resolved);
            let _ = tx.send(BgResponse::FilePreviewReady {
                key: want,
                resolved,
                text,
            });
        });
    }

    // Files level: refresh the `git status` labels once the row set changed
    // and no scan runs. `at_version` rides along so the reply marks which
    // `rows_version` it described.
    if files_view_on_screen(state)
        && state.files.statused_at != Some(state.files.rows_version)
        && !state.files.statuses_in_flight
    {
        state.files.statuses_in_flight = true;
        let at_version = state.files.rows_version;
        let cwd = state.cwd();
        let paths: Vec<String> = state.files.rows.iter().map(|r| r.path.clone()).collect();
        let tx = bg_tx.clone();
        tokio::spawn(async move {
            // A failed scan applies as an empty map — the labels simply stay
            // off until the next row change (git missing / not a repo /
            // timeout all look the same to the rows).
            let statuses = pacode_vcs::worktree_statuses(&cwd, &paths)
                .await
                .unwrap_or_default();
            let _ = tx.send(BgResponse::FileStatusesReady {
                statuses,
                at_version,
            });
        });
    }

    // Files level `enter`: the worktree diff for the panel. The want dies
    // when the panel's target moved on; an in-flight probe defers a re-arm.
    if let Some(path) = state.files.diff_want.clone() {
        let still_target = matches!(
            &state.panel.target,
            Some(PanelTarget::FileDiff { path: p }) if *p == path
        );
        if !still_target {
            state.files.diff_want = None;
        } else if state.files.diff_in_flight.is_none() {
            state.files.diff_want = None;
            state.files.diff_in_flight = Some(path.clone());
            let cwd = state.cwd();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let diff = pacode_vcs::file_diff(&cwd, &path).await;
                let _ = tx.send(BgResponse::FileDiffReady { path, diff });
            });
        }
    }

    // Background level: the detail card's output tail — a blocking read of
    // the selected task's spool file (§5.9). The want is keyed by (task,
    // `output_bytes`): a `TaskUpdated` carrying more output re-keys it and
    // the read re-arms by itself — the daemon's task events drive the
    // refresh, no timer and no event-side invalidation.
    if let Some(want) = state.rail.task_tail_want.clone()
        && state.rail.task_tail_in_flight.as_ref() != Some(&want)
        && state.rail.task_tail.as_ref().map(|t| t.key()).as_ref() != Some(&want)
    {
        // A task that vanished between draw and dispatch has no file to
        // read — the want dies with the next draw's re-record.
        if let Some(path) = state.rail.task(&want.task).map(|t| t.output_path.clone()) {
            state.rail.task_tail_in_flight = Some(want.clone());
            let tx = bg_tx.clone();
            tokio::task::spawn_blocking(move || {
                let (text, truncated) = crate::ui::menu::background::read_tail(&path);
                let _ = tx.send(BgResponse::TaskTailReady {
                    tail: crate::state::rail::TaskTail {
                        task: want.task,
                        output_bytes: want.output_bytes,
                        text,
                        truncated,
                    },
                });
            });
        }
    }

    // Pasted-image strip thumbnails: one job per missing/mismatched slot.
    for (token, path, cell_w, cell_h) in std::mem::take(&mut state.image_strip.want) {
        let k = (token, cell_w, cell_h);
        let already = state
            .image_strip
            .thumbs
            .get(&token)
            .is_some_and(|t| t.cell_w == cell_w && t.cell_h == cell_h);
        if already || state.image_strip.in_flight.contains(&k) {
            continue;
        }
        state.image_strip.in_flight.insert(k);
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let req = pacode_image::PreviewRequest {
                path,
                cell_w,
                cell_h,
            };
            let preview = pacode_image::render(&req, pacode_image::detect()).ok();
            let _ = tx.send(BgResponse::ThumbReady {
                token,
                cell_w,
                cell_h,
                preview,
            });
        });
    }

    // The selection copy `ui::draw` prepared — OSC 52 + arboard, off-thread.
    if let Some(copy) = state.pending_copy.take() {
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::clipboard::copy(&copy.text) {
                log::warn!("clipboard copy failed: {e}");
            }
            let _ = tx.send(BgResponse::CopyDone {
                auto: copy.auto,
                chars: copy.text.chars().count(),
            });
        });
    }

    // Git level (§5.10): the draw records what the cursor wants — the
    // selected commit's `--stat` for the detail pane, and the next history
    // page when the cursor sits on the last loaded row. Both go out as
    // actions so `Action::GitLog`'s `begin_log`/`mark_sent` bookkeeping
    // applies. A stale want (menu closed, another level on top) dies here
    // rather than firing a fetch nobody sees.
    let mut actions = Vec::new();
    if git_view_on_screen(state) {
        if let Some(hash) = state.git.show_want.clone()
            && state.git.detail.as_ref().map(|d| &d.hash) != Some(&hash)
            && state.git.show_in_flight.is_none()
        {
            state.git.show_in_flight = Some(hash.clone());
            actions.push(Action::GitShow { hash });
        }
        // A page that already failed on this `after` stays blocked while
        // the want still names it — the cursor moving off (or a landed
        // `apply_log`, which clears `more_failed`) is what re-arms it.
        if state.git.more_want != state.git.more_failed {
            state.git.more_failed = None;
        }
        if let Some(after) = state.git.more_want.clone()
            && state.git.has_more
            && !state.git.loading
            && state.git.more_failed.as_deref() != Some(after.as_str())
        {
            actions.push(Action::GitLog { after: Some(after) });
        }
    } else {
        state.git.show_want = None;
        state.git.more_want = None;
        state.git.more_failed = None;
    }
    actions
}

/// Whether the `Git` menu level is on screen — gates the detail-stat and
/// next-page wants (§5.10), like [`files_view_on_screen`] does for Files.
fn git_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Git)
    )
}

/// Whether the `Files` menu level is on screen (§5.5) — gates the preview
/// cache's cleanup and the deferred `git status` scan.
fn files_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Files)
    )
}

/// Whether the `Background` menu level is on screen — gates the task-tail
/// cache's cleanup (§5.9).
fn background_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Background)
    )
}

/// The preview pane's path resolution — `raw` relative to the session cwd
/// when that joined path exists. Runs inside `spawn_blocking`: `exists()` is
/// a filesystem stat and must never run inside `ui::draw` (§8.1).
fn resolve_preview_path(raw: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let joined = cwd.join(p);
    if joined.exists() {
        joined
    } else {
        p.to_path_buf()
    }
}

fn handle_bg_response(res: BgResponse, state: &mut AppState) -> Vec<Action> {
    match res {
        BgResponse::Reply { kind, res } => {
            // §8.2: the reply ends the in-flight mark; a successful one
            // promotes the section to `Fresh`, a failed one leaves the cached
            // copy — and its `offline copy` badge — alone.
            if let Some(kind) = kind {
                let ok = matches!(&res, Ok(r) if !matches!(r, Reply::Error { .. }));
                state.lists.on_reply(kind, ok);
                // A failed `GitLog` never reaches `apply_log`, so its
                // loading flags would stick — the menu would badge "loading"
                // forever and the page want would never refire (§5.10).
                if !ok && matches!(kind, crate::cache::ListKind::GitLog) {
                    state.git.fail_log();
                    state.dirty = true;
                }
            }
            handle_reply(res, state)
        }
        BgResponse::AtCompleted { key, candidates } => {
            state.input.at_comp.finish_scan(key, candidates);
            state.dirty = true;
            vec![]
        }
        BgResponse::BashCompletionDone { key, candidates } => {
            state.input.bash_comp.finish_scan(key, candidates);
            state.dirty = true;
            vec![]
        }
        BgResponse::FilePreviewReady {
            key,
            resolved,
            text,
        } => {
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
            statuses,
            at_version,
        } => {
            state.files.apply_statuses(statuses, at_version);
            state.dirty = true;
            vec![]
        }
        BgResponse::GitShowReady { hash, res } => {
            match res {
                Ok(Reply::GitShow { hash: h, stat }) => state.git.apply_show(h, stat),
                // `Reply::Error` names no commit — the request's hash keys it.
                Ok(Reply::Error { message }) => state.git.fail_show(hash, message),
                Ok(_) => state.git.fail_show(hash, "unexpected reply".to_string()),
                Err(e) => state.git.fail_show(hash, e.to_string()),
            }
            state.dirty = true;
            vec![]
        }
        BgResponse::GitDiffReady { hash, res } => {
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
        BgResponse::TaskTailReady { tail } => {
            state.rail.task_tail_in_flight = None;
            // Applies while the level still wants this task at this size —
            // a moved cursor or a `TaskUpdated` re-keys the want and the
            // next `dispatch_deferred` pass re-arms the read.
            state.rail.task_tail = Some(tail);
            state.dirty = true;
            vec![]
        }
        BgResponse::FileDiffReady { path, diff } => {
            state.files.diff_in_flight = None;
            // Applies only while the panel still targets this file — a
            // response for a since-unselected path drops (§5.5).
            if matches!(
                &state.panel.target,
                Some(PanelTarget::FileDiff { path: p }) if *p == path
            ) {
                state.panel.diff = Some(crate::state::PanelDiff {
                    path,
                    patch: diff.patch,
                    status: diff.status,
                    truncated: diff.truncated,
                    note: diff.note,
                    rendered: None,
                });
            }
            state.dirty = true;
            vec![]
        }
        BgResponse::ThumbReady {
            token,
            cell_w,
            cell_h,
            preview,
        } => {
            state.image_strip.in_flight.remove(&(token, cell_w, cell_h));
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
        BgResponse::CopyDone { auto, chars } => {
            state.on_copy_done(auto, chars);
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

/// Fold one daemon reply into state. List replies replace their section
/// wholesale (§8.2) and — once state moved — rewrite their cache file.
fn handle_reply(
    res: Result<Reply, pacode_client::ClientError>,
    state: &mut AppState,
) -> Vec<Action> {
    use crate::cache::ListKind;
    match res {
        Ok(Reply::Error { message }) => {
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
                })]
            }
            PluginCommandOutcome::Nothing => vec![],
        },
        Ok(Reply::Attached(snapshot)) | Ok(Reply::Snapshot(snapshot)) => {
            // §8.2 project switch: a new cwd means the old project's git list
            // must not linger — `apply_client_event` resets the state and the
            // per-project cache key does the file side. Re-fetch for the new
            // project so the submenu is populated when opened.
            let project_changed = state
                .meta
                .as_ref()
                .is_some_and(|m| m.cwd != snapshot.meta.cwd);
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
            if project_changed {
                vec![Action::GitLog { after: None }]
            } else {
                vec![]
            }
        }
        Ok(_) => vec![],
        Err(err) => {
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
    }
}
