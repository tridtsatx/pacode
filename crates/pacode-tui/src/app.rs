//! Event loop and redraw scheduling.
//!
//! One `tokio::select!` over: client events (`ClientEvent`), terminal events
//! (`crossterm::event::EventStream`), the stream tick (paced by `config.ui.ups`,
//! only while `state.needs_stream_tick()`), the second tick (1 s, only while
//! `state.needs_second_tick()`), the animation tick (a phase-stable
//! `tokio::time::Interval` clocked by `config.ui.animation`: `tick_ms` while
//! the turn/spinner runs, `fast_tick_ms` while a transient animation — toast
//! slide/fade, overlay unfold, welcome cascade — is on screen; armed only
//! while something animates), and toast expiry (armed only while toasts exist).
//!
//! Two clocks pace two dirty flags: content sets `state.dirty` and is gated by
//! `frame_interval` (`ui.ups`); the animation tick sets `state.dirty_anim` and
//! is gated by the current animation period. Either flag with its gate elapsed
//! draws a frame that clears both (otherwise a deferred draw is scheduled for
//! the earliest pending gate). Nothing ticks in the idle state.
//!
//! Startup: paint the first frame, connect (spawning the daemon), then loop —
//! the attach runs as a background request so keys work while the daemon is
//! still opening the session. Its snapshot lands via `BgResponse::AttachFinished`,
//! which applies it, flushes requests queued meanwhile, and sends
//! `initial_prompt` plus the post-attach list batch. On `quit`: leave the alt
//! screen and close the client (the daemon and its sessions keep running).

mod attach;
mod bg_response;
mod deferred;
mod dispatch;
mod frame;
mod panel;
mod prompt;
mod reply;
mod resync;
mod wakeups;

use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

use pacode_client::{Client, ClientEvent};
use pacode_types::state::ToastLevel;

use self::attach::{
    AttachRequest, AttachRequestKind, dispatch_pending_attach_retry, snapshot_actions_after_apply,
    spawn_attach_request,
};
use self::bg_response::{BgResponse, handle_bg_response};
use self::deferred::{background_view_on_screen, dispatch_deferred, files_view_on_screen};
use self::dispatch::{dispatch_action, dispatch_global_refresh_action};
use self::frame::{AnimPacer, FrameClock, Painter, spawn_refresh_probe};
use self::prompt::bound_paste;
use self::resync::{clear_list_loading, take_global_resync_actions, take_resync_action};
use crate::keys::{Action, handle_key, handle_mouse, prompt_batch_action};
use crate::state::AppState;
use crate::terminal::TerminalGuard;
use crate::ui;
use crate::{TuiError, TuiOptions};

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;

// `app_tests` reaches the loop's helpers through `use super::*`, as it did
// before they moved into submodules.
#[cfg(test)]
use {
    self::{bg_response::*, deferred::*, dispatch::*, panel::*, prompt::*, reply::*},
    crate::state::transcript::CellKind,
    crate::state::{PanelTarget, PromptBatch, SessionContext},
    pacode_client::ClientEventContext,
    pacode_types::{AgentId, Reply, TranscriptKind},
};

/// `None` when the loop exited before a session attached (the attach is a
/// background request — quitting during it is allowed).
pub async fn run(mut opts: TuiOptions) -> Result<Option<pacode_types::SessionId>, TuiError> {
    let timing = crate::timing::enabled();
    let t_start = Instant::now();
    log::info!("TUI starting up");

    let t = Instant::now();
    let mut term_guard = TerminalGuard::enter(opts.config.ui.mouse)?;
    crate::timing::mark(timing, "terminal_enter", t);

    // Arm the frame metrics before the first frame and hold the gate for the
    // whole run, so the reading `pacode stats` prints is this client's and not
    // the previous one's. Off unless `PACODE_FRAME_METRICS` says otherwise —
    // an unarmed gate writes nothing and does not even remove the last
    // client's report.
    let _frame_metrics = crate::render_metrics::MetricsGate::install(&opts.client.paths.state_dir);

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
    state.analyze_mode = opts.analyze_mode;
    state.paths = opts.client.paths.clone();
    state.cache = crate::cache::DiskCache::new(state.paths.cache_dir.clone());
    state.connection = crate::state::Connection::Connecting;
    // The `-p` prompt flies once the attach lands — `AttachFinished` sends it.
    state.initial_prompt = opts.initial_prompt.take();
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
        let prepared = ui::prepare_frame(f.area(), &mut state, Instant::now());
        ui::draw(f, &state, &prepared);
        crate::font::apply_weight_to_frame(f, &state.config.font);
    })?;
    crate::timing::mark(timing, "first_frame", t_start);

    let t = Instant::now();
    let (client, mut events) = Client::connect(opts.client).await?;
    crate::timing::mark(timing, "client_connect", t);
    let client = Arc::new(client);

    let mut term_events = EventStream::new();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<BgResponse>();

    // The attach is the slow part of startup — a cold session open may wait
    // on the daemon's store and MCP work — so it runs as a background
    // request and the event loop starts now. `AttachFinished` applies the
    // snapshot, flushes `state.pending_requests`, sends `initial_prompt`,
    // and fires the §8.2 post-attach list batch. `client.attach` (not a raw
    // `request`) keeps the remembered session populated for reconnects.
    let t_attach = Instant::now();
    let attach_context = state.begin_attach(opts.attach.clone());
    let attach_client_context = client.event_context();
    let attach_token = state
        .set_pending_attach_client_epoch(attach_client_context.session_epoch.wrapping_add(1))
        .expect("pending attach must exist");
    let attach_epoch = state
        .pending_attach_epoch_signal()
        .expect("pending attach epoch must exist");
    attach_epoch.store(
        attach_client_context.session_epoch.wrapping_add(1),
        Ordering::Release,
    );
    spawn_attach_request(
        &mut state,
        &client,
        &bg_tx,
        AttachRequest {
            context: attach_context,
            token: attach_token,
            request_client_context: attach_client_context,
            attach: opts.attach.clone(),
            kind: AttachRequestKind::Attach,
            timing_started: Some(t_attach),
        },
    );

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

    // `ui.ups = "auto"` probes external tools (hyprctl/wlr-randr/xrandr —
    // measured ~37 ms warm, up to 200 ms per miss). Run it off the critical
    // path and adopt the result when it lands; until then draw uncapped —
    // nothing animates during attach anyway, so no extra frames are spent.
    // `_hz_keepalive` keeps one sender alive so `hz_rx.recv()` in the loop
    // pends forever once the probe result is consumed (a closed channel would
    // return `None` instantly and spin the select loop).
    let t_probe = Instant::now();
    let (hz_tx, mut hz_rx) = mpsc::unbounded_channel::<(u64, Option<u16>)>();
    let _hz_keepalive = hz_tx.clone();
    let mut frame_clock = FrameClock::new(state.config.ui.ups);
    if frame_clock.ups == pacode_types::Ups::Auto {
        spawn_refresh_probe(hz_tx.clone(), frame_clock.probe_generation);
    }
    let mut frame_interval = frame_clock.interval;
    crate::timing::mark(timing, "refresh_hz_spawn", t_probe);

    let mut painter = Painter::new(frame_interval);
    let mut anim = AnimPacer::new(state.config.ui.animation.tick_ms());

    loop {
        if term_guard.mouse_enabled() != state.config.ui.mouse
            && let Err(error) = term_guard.set_mouse(state.config.ui.mouse)
        {
            log::warn!("failed to update terminal mouse capture: {error}");
            state.config.ui.mouse = term_guard.mouse_enabled();
        }
        if frame_clock.set_ups(state.config.ui.ups) {
            frame_interval = frame_clock.interval;
            if frame_clock.ups == pacode_types::Ups::Auto {
                spawn_refresh_probe(hz_tx.clone(), frame_clock.probe_generation);
            }
        }
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

        painter.draw_if_due(
            &mut term_guard,
            &mut state,
            frame_interval,
            &mut anim,
            frame_clock.detected_hz,
            timing,
        )?;

        // Deferred work recorded before paint: completion scans, image
        // renders, the stashed clipboard copy — all of it runs on blocking
        // threads, replies land back through `bg_rx` (§8.1). Daemon-bound
        // wants come back as actions and go through `dispatch_action`.
        for act in dispatch_deferred(&mut state, &bg_tx, &client) {
            dispatch_action(act, &mut state, &client, &bg_tx);
        }

        if state.quit {
            break;
        }

        // Compute timers dynamically
        let needs_stream = state.needs_stream_tick();
        let stream_tick_dur = frame_interval.unwrap_or(Duration::from_millis(33));
        let stream_sleep = wakeups::stream(needs_stream, stream_tick_dur);

        // The second tick is armed to the next boundary of the displayed clock, not
        // to a full second from now. Every other timer in this select! cancels and
        // re-creates this future; measuring from `now` meant a plain 1 s sleep was
        // restarted by each animation frame and never fired, so the duration only
        // moved on unrelated redraws and read 0.1 -> 0.3 -> 0.5 instead of ticking.
        let needs_second = state.needs_second_tick() || state.phase.is_some();
        let second_sleep = wakeups::second(needs_second, &state);

        anim.rearm(&state);
        let ctrl_c_expiry_sleep = wakeups::ctrl_c_expiry(&state);
        let toast_expiry_sleep = wakeups::toast_expiry(&state);
        let deferred_draw_sleep =
            wakeups::deferred_draw(painter.next_draw_wait(&state, frame_interval, anim.period()));
        let menu_remote_sleep = wakeups::menu_remote(&state);
        let resync_sleep = wakeups::resync(&state);
        let anim_sleep = anim.tick();

        tokio::select! {
            client_ev = events.recv() => {
                match client_ev {
                    Some(ev) => on_client_event(ev, &mut state, &client, &bg_tx),
                    None => {
                        break;
                    }
                }
            }
            term_ev = term_events.next() => {
                if let Some(Ok(ev)) = term_ev {
                    let actions = match ev {
                        Event::Key(key) => handle_key(&mut state, key, Instant::now()),
                        Event::Mouse(mouse) => {
                            handle_mouse(&mut state, mouse, &painter.last_layout)
                        }
                        Event::Paste(text) => {
                            let now = Instant::now();
                            let text = bound_paste(&mut state, text, now);
                            crate::paste::handle_paste(&mut state, text, now)
                        }
                        Event::Resize(w, h) => {
                            state.cols = w;
                            state.rows = h;
                            state.dirty = true;
                            // The next frame reallocates ratatui's buffers and
                            // repaints every cell; marking it here is what lets
                            // the report separate a window drag from a client
                            // that is slow at painting.
                            crate::render_metrics::RenderMetrics::record_resize_frame();
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
                    on_bg_response(res, &mut state, &client, &bg_tx);
                }
            }
            detected = hz_rx.recv() => {
                if let Some((generation, hz)) = detected
                    && frame_clock.accept_probe(generation, hz)
                {
                    crate::timing::mark(timing, "refresh_hz_detect", t_probe);
                    frame_interval = frame_clock.interval;
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
                let turn_active = state.turn_active;
                if state.rail.update_idle(turn_active, now) {
                    state.dirty = true;
                }
                // Marks the frame dirty itself when the whole-second reading changed.
                state.tick_phase(now);
            }
            _ = anim_sleep => {
                anim.on_tick(&mut state);
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
            _ = resync_sleep => {
                // The debounce window closed with a want still armed — the
                // send gate inside re-checks attachment and connection.
                if let Some(action) = take_resync_action(&mut state, Instant::now()) {
                    dispatch_action(action, &mut state, &client, &bg_tx);
                }
            }
        }
    }

    // `None` is a real outcome: quitting while the background attach was
    // still in flight means no session was ever attached.
    let attached_session = state.meta.as_ref().map(|m| m.id.clone());
    match &attached_session {
        Some(id) => log::info!("TUI loop ended, quitting session {id}"),
        None => log::info!("TUI loop ended before a session attached"),
    }
    painter.clear_image_escapes(&mut term_guard);
    drop(events);
    drop(bg_rx);
    match Arc::try_unwrap(client) {
        Ok(client) => client.close().await,
        Err(client) => {
            log::warn!(
                "client close skipped: {} outstanding Arc clone(s)",
                Arc::strong_count(&client).saturating_sub(1)
            );
            drop(client);
        }
    }
    // A failed startup attach propagated through `state.attach_error` — the
    // CLI prints it the same way the old synchronous `?` did.
    if let Some(err) = state.attach_error.take() {
        return Err(err.into());
    }
    if matches!(
        state.connection,
        crate::state::Connection::Disconnected { .. }
    ) {
        return Err(pacode_client::ClientError::Disconnected.into());
    }
    Ok(attached_session)
}

/// A daemon/client event: fold it in, then send whatever it unblocked — an
/// attach retry, a seq-gap or global resync, the post-snapshot flush, and
/// queued prompts.
fn on_client_event(
    ev: ClientEvent,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let global_resync = matches!(&ev, ClientEvent::ResyncNeeded { .. });
    let snapshot_landed = matches!(&ev, ClientEvent::Snapshot { .. });
    let reset_deferred = matches!(
        &ev,
        ClientEvent::Disconnected { .. } | ClientEvent::Reconnecting { .. }
    );
    let pending_attach = state.pending_attach.is_some();
    let now = Instant::now();
    let snapshot_applied = state.apply_client_event(ev, now);
    if reset_deferred && snapshot_applied {
        state.session.cancel_deferred_work();
        clear_list_loading(state);
    }
    dispatch_pending_attach_retry(state, client, bg_tx);
    let reconnect_boundary =
        snapshot_landed && snapshot_applied && state.take_reconnect_snapshot_boundary();
    if let Some(action) = take_resync_action(state, now) {
        dispatch_action(action, state, client, bg_tx);
    }
    for action in take_global_resync_actions(
        state,
        (global_resync && snapshot_applied) || reconnect_boundary,
    ) {
        dispatch_global_refresh_action(action, state, client, bg_tx);
    }
    for action in
        snapshot_actions_after_apply(state, pending_attach, snapshot_landed, snapshot_applied)
    {
        dispatch_action(action, state, client, bg_tx);
    }
    if let Some(req) = state.drain_prompt_queue() {
        dispatch_action(Action::Send(req), state, client, bg_tx);
    }
    if let Some(sequence) = state.drain_pending_sequence() {
        dispatch_action(prompt_batch_action(sequence), state, client, bg_tx);
    }
}

/// A background result: fold it in, then dispatch what it produced and
/// whatever it unblocked — an attach retry and queued prompts.
fn on_bg_response(
    res: BgResponse,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let actions = handle_bg_response(res, state);
    dispatch_pending_attach_retry(state, client, bg_tx);
    for act in actions {
        dispatch_action(act, state, client, bg_tx);
    }
    if let Some(req) = state.drain_prompt_queue() {
        dispatch_action(Action::Send(req), state, client, bg_tx);
    }
    if let Some(sequence) = state.drain_pending_sequence() {
        dispatch_action(prompt_batch_action(sequence), state, client, bg_tx);
    }
}
