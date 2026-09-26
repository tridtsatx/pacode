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

use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

use pacode_client::{Client, ClientEvent, ClientEventContext};
use pacode_types::state::ToastLevel;
use pacode_types::time::now_ms;
use pacode_types::{AgentId, Attach, PluginCommandOutcome, Reply, Request, TranscriptKind};

use crate::keys::{Action, handle_key, handle_mouse, prompt_batch_action};
use crate::layout::ScreenLayout;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{
    AppState, PanelTarget, PromptBatch, SessionContext, TOAST_EXIT_MS, TOAST_TTL_SECS,
    UserMessageInFlight,
};
use crate::terminal::TerminalGuard;
use crate::ui;
use crate::{TuiError, TuiOptions};

/// Cap on session-bound requests queued while the attach is in flight —
/// past it the oldest is dropped; a user cannot out-type a snapshot.
const PENDING_REQUESTS_CAP: usize = 16;

/// Ceiling on the file an inline thumbnail decodes: a kitty/sixel escape
/// carries the encoded image, so an unbounded paste would put megabytes into
/// the frame path's per-frame clone. Over the cap the slot keeps its
/// placeholder; the prompt still attaches the original file to the message.
const IMAGE_THUMB_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameClock {
    ups: pacode_types::Ups,
    detected_hz: Option<u16>,
    probe_generation: u64,
    interval: Option<Duration>,
}

impl FrameClock {
    fn new(ups: pacode_types::Ups) -> Self {
        Self {
            ups,
            detected_hz: None,
            probe_generation: 0,
            interval: match ups {
                pacode_types::Ups::Auto | pacode_types::Ups::Dynamic => None,
                pacode_types::Ups::Fixed(_) => ups.frame_interval(None),
            },
        }
    }

    fn set_ups(&mut self, ups: pacode_types::Ups) -> bool {
        if self.ups == ups {
            return false;
        }
        self.ups = ups;
        self.probe_generation = self.probe_generation.wrapping_add(1);
        self.interval = match ups {
            pacode_types::Ups::Auto | pacode_types::Ups::Dynamic => None,
            pacode_types::Ups::Fixed(_) => ups.frame_interval(self.detected_hz),
        };
        true
    }

    fn accept_probe(&mut self, generation: u64, hz: Option<u16>) -> bool {
        if generation != self.probe_generation || self.ups != pacode_types::Ups::Auto {
            return false;
        }
        self.detected_hz = hz;
        self.interval = self.ups.frame_interval(hz);
        true
    }
}

fn spawn_refresh_probe(tx: mpsc::UnboundedSender<(u64, Option<u16>)>, generation: u64) {
    std::thread::spawn(move || {
        let _ = tx.send((generation, pacode_config::display::detect_refresh_hz()));
    });
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;

#[derive(Clone, Copy)]
enum AttachRequestKind {
    Attach,
    Switch,
    Retry,
}

struct AttachRequest {
    context: SessionContext,
    token: u64,
    request_client_context: ClientEventContext,
    attach: Attach,
    kind: AttachRequestKind,
    timing_started: Option<Instant>,
}

enum ReplyScope {
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

enum BgResponse {
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

fn spawn_attach_request(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    request: AttachRequest,
) {
    state.attach_in_flight = true;
    let cl = Arc::clone(client);
    let tx = bg_tx.clone();
    tokio::spawn(async move {
        let result = match request.kind {
            AttachRequestKind::Attach => cl.attach(request.attach.clone()).await,
            AttachRequestKind::Switch => cl.switch_session(request.attach.clone()).await,
            AttachRequestKind::Retry => cl.retry_attach(request.attach.clone()).await,
        };
        if let Some(started) = request.timing_started {
            crate::timing::mark(crate::timing::enabled(), "attach_snapshot", started);
        }
        let client_context = cl.event_context();
        let _ = tx.send(BgResponse::AttachFinished {
            context: request.context,
            token: request.token,
            request_client_context: request.request_client_context,
            client_context,
            attach: request.attach,
            result,
        });
    });
}

fn dispatch_pending_attach_retry(
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    let Some(pending) = state.take_pending_attach_retry() else {
        return;
    };
    spawn_attach_request(
        state,
        client,
        bg_tx,
        AttachRequest {
            context: pending.context,
            token: pending.token,
            request_client_context: client.event_context(),
            attach: pending.attach,
            kind: AttachRequestKind::Retry,
            timing_started: None,
        },
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

struct PanelLoads {
    next: u64,
    in_flight: Option<(SessionContext, PanelTarget, u64)>,
    applied: u64,
}

thread_local! {
    static PANEL_LOADS: std::cell::RefCell<PanelLoads> = const {
        std::cell::RefCell::new(PanelLoads { next: 0, in_flight: None, applied: 0 })
    };
}

/// One panel load per (session, target): navigation emits `Action::LoadPanel`
/// per step, so without this a held arrow key runs a `GetHistory` page per
/// agent and the replies race into the same panel. Returns the token the reply
/// must carry.
fn begin_panel_load(context: &SessionContext, target: &PanelTarget) -> Option<u64> {
    PANEL_LOADS.with(|cell| {
        let mut loads = cell.borrow_mut();
        if loads
            .in_flight
            .as_ref()
            .is_some_and(|(pending, panel, _)| pending == context && panel == target)
        {
            return None;
        }
        loads.next = loads.next.wrapping_add(1);
        let token = loads.next;
        loads.in_flight = Some((context.clone(), target.clone(), token));
        Some(token)
    })
}

/// Whether this reply is the newest load of its target. A page that a newer
/// load already replaced is dropped instead of overwriting the panel, and the
/// slot is released either way — a load left behind would refuse every later
/// request for the same target.
fn panel_load_reply_is_current(context: &SessionContext, target: &PanelTarget, token: u64) -> bool {
    PANEL_LOADS.with(|cell| {
        let mut loads = cell.borrow_mut();
        if loads
            .in_flight
            .as_ref()
            .is_none_or(|(pending, panel, sent)| {
                pending != context || panel != target || *sent != token
            })
        {
            return false;
        }
        loads.in_flight = None;
        if token <= loads.applied {
            return false;
        }
        loads.applied = token;
        true
    })
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

    let mut last_draw = Instant::now() - frame_interval.unwrap_or(Duration::from_millis(33));
    // Baseline of the animation clock's draw gate, tracked separately so a
    // content-gated frame does not displace the animation phase.
    let mut last_anim_draw = last_draw;
    // The animation clock. `Some` exactly while `anim_active` holds, and
    // phase-stable across a period change (`backpressure::Ticker`), so neither
    // a stream event nor a live `ui.animation` edit can reset its phase.
    let mut anim_tick: Option<crate::backpressure::Ticker> = None;
    // The nominal period in force, and the backpressure state that stretches
    // it. The budget is the nominal period itself — see `backpressure` for why
    // a constant budget would degrade a client that is deliberately cheap.
    let mut nominal_anim_ms = state.config.ui.animation.tick_ms();
    let mut backpressure = crate::backpressure::Backpressure::new(nominal_anim_ms);
    // The period the animation gate is measured against: the nominal while the
    // frames are affordable, stretched by the multiplier while they are not.
    // The second tick is not stretched — a clock reading that walks backwards is
    // worse than one that skips a beat.
    let mut anim_period = backpressure.period();
    // Absolute animation clock. Every animated block reads its phase from this
    // one elapsed value (see `backpressure::anim_phase`), so a degraded cadence
    // slows the frames down and not the animation: skipped frames skip phases
    // instead of queueing them.
    let anim_epoch = Instant::now();
    // The phase every animated block reads: a count of steps taken since the
    // last period change, not since startup. A change of cadence re-anchors the
    // count rather than multiplying it, so 66 ms -> 16 ms does not throw the
    // spinner 3.16 turns forward in a single frame.
    let mut anim_clock = crate::backpressure::PhaseClock::new(nominal_anim_ms);
    // Whether a missed content/anim frame has already been counted for the slot
    // it missed. See the drop accounting at the draw gate for why a skip is
    // latched rather than counted on every loop iteration.
    let mut counted_content_skip = false;
    let mut counted_anim_skip = false;
    // Whether each gate was open on the previous pass, so a missed slot can be
    // told apart from an event that has simply not reached its turn yet. False at
    // startup: nothing has been painted yet, so no slot has been missed.
    let mut content_slot_open = false;
    let mut anim_slot_open = false;
    let mut last_layout = ScreenLayout::default();
    // Input image-strip escapes currently on screen (spec §1): tracked so the
    // post-draw flush can diff and never re-send tens of MB of kitty/sixel
    // payload per frame.
    let mut active_image_escapes: Vec<(ratatui::layout::Rect, String)> = Vec::new();
    // Set when escapes were removed/moved: the next draw must repaint the
    // cells the images covered (ratatui's diff can't see tty pixels).
    let mut needs_image_repaint = false;

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

        // Redraw on two independent clocks: content (`state.dirty`) is gated by
        // `frame_interval` (`ui.ups`); animation (`state.dirty_anim`) is gated
        // by the current animation period. Either flag with its gate elapsed
        // draws a frame that flushes both, so a pending anim frame can ride a
        // content draw and never stalls waiting on the slower clock.
        let now = Instant::now();
        let content_gate_open = match frame_interval {
            Some(interval) => now.saturating_duration_since(last_draw) >= interval,
            None => true,
        };
        let anim_gate_open = now.saturating_duration_since(last_anim_draw) >= anim_period;
        // A frame the scheduler could have painted and did not, because its
        // slot was still occupied by the previous one. Counted once per *slot*,
        // not once per loop iteration: the loop wakes far more often than
        // either gate opens, and a counter that counted idle iterations would
        // make a healthy client look like one throwing its frames away. Each
        // flag is judged against its own gate — a `dirty_anim` two microseconds
        // short of its period is early, not dropped.
        let content_ready = state.dirty && content_gate_open;
        let anim_ready = state.dirty_anim && anim_gate_open;
        if crate::render_metrics::RenderMetrics::recording() {
            // A *missed slot*, not a missed dirty event. Both flags are level
            // triggered: `dirty` stays set until a frame flushes it, so the
            // first time it is seen the gate is almost always still closed and
            // the event is merely waiting for its turn. Counting that would
            // report one dropped frame for every event ever drawn and the
            // metric would measure the opposite of what it claims. A slot is
            // missed only when the gate was open on the previous pass and is
            // closed now with something still pending: that closed interval is
            // a frame the client could have painted and did not. The latch
            // re-arms on the next open gate for the same reason.
            if content_slot_open && state.dirty && !content_gate_open && !counted_content_skip {
                crate::render_metrics::RenderMetrics::record_drop();
                counted_content_skip = true;
            }
            if anim_slot_open && state.dirty_anim && !anim_gate_open && !counted_anim_skip {
                crate::render_metrics::RenderMetrics::record_drop();
                counted_anim_skip = true;
            }
            // The slot is open: whatever is pending gets painted, so the next
            // miss is a new one.
            counted_content_skip &= !content_gate_open;
            counted_anim_skip &= !anim_gate_open;
            content_slot_open = content_gate_open;
            anim_slot_open = anim_gate_open;
        }
        if content_ready || anim_ready {
            let flushed_anim = state.dirty_anim;
            let t_frame = Instant::now();
            let recording = crate::render_metrics::RenderMetrics::recording();
            let frame_kind = if needs_image_repaint {
                // A buffer reset repaints every cell: the most expensive shape a
                // frame can take, and it happened because content moved an image.
                crate::render_metrics::FrameKind::Full
            } else if !state.dirty {
                // The animation clock on its own.
                crate::render_metrics::FrameKind::Anim
            } else {
                crate::render_metrics::FrameKind::Content
            };
            if recording {
                // The main transcript and an open agent panel are both streams, and a
                // frame that drains one of them is reading past the other: the
                // larger is the number that says how far the client fell behind.
                crate::render_metrics::RenderMetrics::note_backlog(
                    state
                        .transcript
                        .stream_backlog_chars()
                        .max(state.panel.agent_transcript.stream_backlog_chars()),
                );
            }
            // DEC 2026 synchronized output: the terminal buffers the frame and
            // presents it whole instead of painting a half-drawn one. The
            // `CompletedFrame` borrow is dropped before `end_sync` so the sync
            // region still closes when `draw` errored.
            term_guard.begin_sync();
            let repainted = needs_image_repaint;
            let mut clear_cost = Duration::ZERO;
            if repainted {
                // Image escapes were removed or moved last frame: the tty
                // pixels they occupied sit on cells ratatui's buffer already
                // considers painted, so a plain diff would leave ghosts. A
                // buffer reset forces a full repaint inside the sync region.
                let t_clear = Instant::now();
                let _ = term_guard.terminal.clear();
                if recording {
                    clear_cost = t_clear.elapsed();
                }
                needs_image_repaint = false;
            }
            // `Terminal::draw` is two steps this report has to tell apart: the
            // render closure (this crate's widgets — all of it ours), and the
            // diff plus the write and flush (the terminal's). A slow layout and
            // a slow write have different owners and different fixes, so they
            // are measured separately; the timings are only taken while
            // recording is on.
            //
            // Resizes are not in that split: ratatui runs `autoresize` inside
            // `draw`, before the closure, and a resize reallocates both buffers
            // and clears them. Its cost lands in the frame total — a resize
            // frame really is that expensive — and the report says so rather
            // than blaming the layout.
            let t_layout = Instant::now();
            let mut layout_cost = Duration::ZERO;
            let draw_res = term_guard
                .terminal
                .draw(|f| {
                    let prepared = ui::prepare_frame(f.area(), &mut state, Instant::now());
                    last_layout = ui::draw(f, &state, &prepared);
                    crate::font::apply_weight_to_frame(f, &state.config.font);
                    if recording {
                        layout_cost = t_layout.elapsed();
                    }
                })
                .map(|_| ());
            // `t_backend` is the instant the closure ended, so the span to now
            // is the half `draw` did on its own: the cell diff, the write and
            // the flush. It is measured from `t_layout` rather than from
            // `Instant::elapsed()` on a fresh instant, which would be zero by
            // construction and report a healthy client as having a free
            // backend.
            let t_backend = Instant::now();
            let backend_cost = t_backend.duration_since(t_layout);
            term_guard.end_sync();
            draw_res?;
            let frame_cost = t_frame.elapsed();
            if recording {
                crate::render_metrics::RenderMetrics::record(
                    frame_kind,
                    crate::render_metrics::FrameCost {
                        layout: layout_cost,
                        backend: backend_cost,
                        frame: frame_cost,
                    },
                );
                // `frame - layout - backend` is the sync markers and a
                // `Terminal::clear()`, which belong to neither half; the clear
                // is timed separately so an image repaint is not read as
                // layout.
                if !clear_cost.is_zero() {
                    crate::render_metrics::RenderMetrics::record_clear(clear_cost);
                }
            }
            // §8.3: warn when a frame ran over its refresh budget.
            crate::timing::warn_frame_over_budget(
                timing,
                frame_cost,
                crate::timing::frame_budget(frame_clock.detected_hz),
            );
            // Backpressure: the measured cost of the frame sets the cadence of
            // the clock, not the cost of the clock setting it. The content gate
            // is deliberately untouched — a keystroke is not a clock.
            if backpressure.observe(frame_cost) {
                anim_period = backpressure.period();
                crate::render_metrics::RenderMetrics::note_multiplier(backpressure.multiplier());
            }
            state.dirty = false;
            state.dirty_anim = false;
            last_draw = Instant::now();
            if flushed_anim {
                last_anim_draw = last_draw;
            }

            // Input image-strip escapes stashed by `ui::images::draw` —
            // emitted only on change.
            let next_escapes = std::mem::take(&mut state.pending_image_escapes);
            if next_escapes != active_image_escapes {
                if !active_image_escapes.is_empty() {
                    let _ = term_guard.clear_image_escapes();
                    needs_image_repaint = true;
                }
                for (area, esc) in &next_escapes {
                    let _ = term_guard.draw_image_escape(*area, esc);
                }
                active_image_escapes = next_escapes;
            }
        }

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
        // Armed to the next boundary of the displayed clock, not to a full
        // second from now. Every other future in this `select!` cancels and
        // re-creates this one; measuring from `now` meant a plain 1 s sleep was
        // restarted by each animation frame and never fired, so the duration
        // only moved on unrelated redraws and read 0.1 -> 0.3 -> 0.5 instead of
        // ticking. The boundary is re-derived from the reading every iteration
        // rather than held in a ticker, because the deadline is a wall-clock
        // boundary: there is no phase for a ticker to keep.
        let second_sleep = async {
            if needs_second {
                let wait = Duration::from_millis(crate::state::activity::ms_to_next_second(
                    state.phase_elapsed_ms,
                ));
                tokio::time::sleep(wait).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        // The animation clock: a phase-stable ticker, armed exactly while
        // something animates — the same set of conditions that armed the old
        // `anim_sleep`. Its cadence comes from `ui.animation` and the measured
        // frame cost alone and never looks at the stream backlog: a growing
        // backlog must not speed the spinner up.
        let now = Instant::now();
        let anim_mode = state.config.ui.animation;
        let anim_active = state.turn_active
            || state.transcript.has_backlog()
            || state.needs_anim_tick(now)
            || state.toast_needs_anim(now)
            || state.fast_anim_active(now);
        let wanted_anim_ms = if state.fast_anim_active(now) {
            anim_mode.fast_tick_ms()
        } else {
            anim_mode.tick_ms()
        };
        // A live `ui.animation` edit (or a transient animation starting or
        // ending) moves the nominal, and the nominal is both the ticker's period
        // and the backpressure budget, so both follow it here rather than at the
        // frame that happens to notice. Phase-stable: the ticker keeps it.
        if wanted_anim_ms != nominal_anim_ms {
            // A period change re-anchors the phase rather than re-deriving it
            // from the epoch: `anim_phase(elapsed, period)` alone multiplies
            // the phase by the ratio of the two periods, so leaving the fast
            // tick after 5 s puts the spinner at phase 312 where it was 75 —
            // a 3.16-turn jump in the frame the toast starts. The phase is the
            // number of steps taken since the last change, and the change
            // itself is not a step.
            anim_clock.reanchor(crate::backpressure::elapsed_ms(anim_epoch), wanted_anim_ms);
            nominal_anim_ms = wanted_anim_ms;
            backpressure.set_nominal_ms(nominal_anim_ms);
            anim_period = backpressure.period();
        }
        // A period change — a transient animation starting or ending, a live
        // `ui.animation` edit, or the backpressure multiplier moving — is the
        // only reason the ticker is re-armed, and `Ticker` moves its deadline to
        // the next boundary of the new period instead of resetting the phase, so
        // none of the three can make the animation jerk.
        let anim_ms = anim_period.as_millis() as u64;
        if anim_active {
            let wanted = Duration::from_millis(anim_ms);
            match anim_tick.as_mut() {
                Some(t) => {
                    t.set_period(wanted, anim_epoch);
                }
                None => anim_tick = Some(crate::backpressure::Ticker::new(wanted, anim_epoch)),
            }
        } else {
            // Nothing animates: drop the ticker so no timer is armed at rest.
            anim_tick = None;
        }
        let anim_sleep = async {
            match anim_tick.as_mut() {
                // `Ticker::tick` is cancel-safe: the select! can drop this
                // future for any other branch without losing the tick phase.
                Some(t) => {
                    t.tick().await;
                }
                None => std::future::pending::<()>().await,
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
            // Wake at the nearest toast boundary: the fade start (TTL − EXIT)
            // arms the anim tick so the fade actually gets frames while the
            // app is idle; the expiry (TTL) drops the toast. Past the fade
            // start only the expiry remains — never a zero-duration spin.
            let now = Instant::now();
            let fade_at = Duration::from_secs(TOAST_TTL_SECS)
                .saturating_sub(Duration::from_millis(TOAST_EXIT_MS));
            let expire_at = Duration::from_secs(TOAST_TTL_SECS);
            let next = state
                .toasts
                .iter()
                .map(|t| {
                    let age = now.saturating_duration_since(t.shown_at);
                    if age < fade_at {
                        fade_at - age
                    } else {
                        expire_at.saturating_sub(age)
                    }
                })
                .min();
            match next {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending::<()>().await,
            }
        };

        // A pending frame wakes the loop the moment its own gate opens: the
        // content gate for `state.dirty`, the animation gate for
        // `state.dirty_anim`. The earliest of the two arms the sleep, so an
        // animation frame is never stalled waiting on the content clock.
        let content_wait = if state.dirty {
            frame_interval.map(|interval| {
                let elapsed = Instant::now().saturating_duration_since(last_draw);
                interval.saturating_sub(elapsed)
            })
        } else {
            None
        };
        let anim_wait = if state.dirty_anim {
            let elapsed = Instant::now().saturating_duration_since(last_anim_draw);
            Some(anim_period.saturating_sub(elapsed))
        } else {
            None
        };
        let deferred_draw_sleep = async {
            match content_wait.into_iter().chain(anim_wait).min() {
                Some(remaining) => tokio::time::sleep(remaining).await,
                None => std::future::pending::<()>().await,
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

        // A seq-gap resync that lost its send slot to the debounce — or a
        // failed send that re-armed the want — must not wait on the next
        // daemon event: a quiet stream would strand the heal and leave the
        // rail stale. Armed only while a want waits on the send gate (the
        // in-flight case wakes via `BgResponse::ResyncDone`), firing at the
        // debounce boundary, at most `RESYNC_DEBOUNCE_MS` out.
        let resync_sleep = async {
            if !state.resync_want || state.resync_in_flight || state.awaiting_snapshot {
                return std::future::pending::<()>().await;
            }
            let wait = match state.resync_sent_at {
                Some(t) => Duration::from_millis(crate::state::RESYNC_DEBOUNCE_MS)
                    .saturating_sub(Instant::now().saturating_duration_since(t)),
                None => Duration::ZERO,
            };
            tokio::time::sleep(wait).await;
        };

        tokio::select! {
            client_ev = events.recv() => {
                match client_ev {
                    Some(ev) => {
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
                            clear_list_loading(&mut state);
                        }
                        dispatch_pending_attach_retry(&mut state, &client, &bg_tx);
                        let reconnect_boundary = snapshot_landed
                            && snapshot_applied
                            && state.take_reconnect_snapshot_boundary();
                        if let Some(action) = take_resync_action(&mut state, now) {
                            dispatch_action(action, &mut state, &client, &bg_tx);
                        }
                        for action in take_global_resync_actions(
                            &mut state,
                            (global_resync && snapshot_applied) || reconnect_boundary,
                        ) {
                            dispatch_global_refresh_action(action, &mut state, &client, &bg_tx);
                        }
                        for action in snapshot_actions_after_apply(
                            &mut state,
                            pending_attach,
                            snapshot_landed,
                            snapshot_applied,
                        ) {
                            dispatch_action(action, &mut state, &client, &bg_tx);
                        }
                        if let Some(req) = state.drain_prompt_queue() {
                            dispatch_action(Action::Send(req), &mut state, &client, &bg_tx);
                        }
                        if let Some(sequence) = state.drain_pending_sequence() {
                            dispatch_action(
                                prompt_batch_action(sequence),
                                &mut state,
                                &client,
                                &bg_tx,
                            );
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
                    let actions = handle_bg_response(res, &mut state);
                    dispatch_pending_attach_retry(&mut state, &client, &bg_tx);
                    for act in actions {
                        dispatch_action(act, &mut state, &client, &bg_tx);
                    }
                    if let Some(req) = state.drain_prompt_queue() {
                        dispatch_action(Action::Send(req), &mut state, &client, &bg_tx);
                    }
                    if let Some(sequence) = state.drain_pending_sequence() {
                        dispatch_action(
                            prompt_batch_action(sequence),
                            &mut state,
                            &client,
                            &bg_tx,
                        );
                    }
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
                // The phase is read off the absolute animation clock rather
                // than advanced here: a frame this client skipped has to skip
                // its phase too, or the animation runs in slow motion exactly
                // when the client is too slow to draw it
                // (`backpressure::PhaseClock`).
                let elapsed = crate::backpressure::elapsed_ms(anim_epoch);
                state.anim_frame = anim_clock.phase(elapsed);
                // The sweep reads the same clock: elapsed time positions the
                // band, and the period actually in force is what keeps it to
                // one cell per painted frame. Under backpressure a band still
                // moving one cell per *nominal* tick would visibly skip, while
                // the rest of the animation correctly slows down.
                state.anim = pacode_render::AnimClock::new(elapsed, backpressure.period_ms());
                state.dirty_anim = true;
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
    if !active_image_escapes.is_empty() {
        let _ = term_guard.clear_image_escapes();
        active_image_escapes.clear();
    }
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

fn prompt_gate_open(state: &AppState) -> bool {
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
fn bound_paste(state: &mut AppState, text: String, now: Instant) -> String {
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

fn resolve_request(state: &mut AppState, request: Request) -> Request {
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

fn queue_user_message_request(state: &mut AppState, request: Request) {
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

fn queue_sequence(state: &mut AppState, requests: Vec<Request>, text: String) {
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

fn dispatch_action(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    dispatch_action_inner(act, state, client, bg_tx, false);
}

fn dispatch_global_refresh_action(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
) {
    dispatch_action_inner(act, state, client, bg_tx, true);
}

fn dispatch_action_inner(
    act: Action,
    state: &mut AppState,
    client: &Arc<Client>,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    global_refresh: bool,
) {
    match act {
        Action::Quit => {
            state.pending_requests.clear();
            state.pending_attach = None;
            state.attach_in_flight = false;
            state.invalidate_bash_run();
            state.quit = true;
        }
        Action::SwitchSession { attach } => {
            if state.pending_attach.as_ref().is_some_and(|pending| {
                pending.attach == attach && (pending.retry_requested || state.attach_in_flight)
            }) {
                return;
            }
            if state.meta.is_some() {
                state.leave_active_slot();
            }
            if state
                .pending_attach
                .as_ref()
                .is_none_or(|pending| pending.attach != attach)
            {
                state.begin_attach(attach.clone());
            }
            if state.attach_in_flight {
                return;
            }
            let attach_client_context = client.event_context();
            let token = state
                .set_pending_attach_client_epoch(
                    attach_client_context.session_epoch.wrapping_add(1),
                )
                .expect("pending attach must exist");
            let attach_epoch = state
                .pending_attach_epoch_signal()
                .expect("pending attach epoch must exist");
            attach_epoch.store(
                attach_client_context.session_epoch.wrapping_add(1),
                Ordering::Release,
            );
            let context = state.session_context();
            spawn_attach_request(
                state,
                client,
                bg_tx,
                AttachRequest {
                    context,
                    token,
                    request_client_context: attach_client_context,
                    attach,
                    kind: AttachRequestKind::Switch,
                    timing_started: None,
                },
            );
        }
        Action::SendSequence { requests } => {
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
        Action::ContinuePromptBatch { id } => {
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
        Action::Send(req) => {
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
            state.transcript.push_cell(Cell {
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
                perm_explain: None,
            });
            state.transcript.scroll_to_bottom();
            state.dirty = true;
        }
        Action::RunBashCaptured { command } => {
            if state.awaiting_snapshot {
                return;
            }
            log::info!("bash-mode captured start: {command}");
            let context = state.session_context();
            let client_context = client.event_context();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            let run_id = state.start_bash_run(cancel_tx);
            state.dirty = true;
            let tx = bg_tx.clone();
            let cwd = state.cwd();
            tokio::spawn(async move {
                let result = crate::bash::run_captured(&command, &cwd, cancel_rx).await;
                let _ = tx.send(BgResponse::BashCompleted {
                    run_id,
                    context,
                    client_context,
                    result,
                });
            });
        }
        Action::GitLog { after } => {
            if state.awaiting_snapshot {
                queue_pending_request(state, Request::GitLog { after });
                return;
            }
            if state.meta.is_none() {
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                queue_pending_request(state, Request::GitLog { after });
                return;
            }
            state.git.begin_log(after.as_deref());
            state.lists.mark_sent(crate::cache::ListKind::GitLog);
            state.dirty = true;
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(client_context.clone(), Request::GitLog { after })
                    .await;
                let _ = tx.send(BgResponse::Reply {
                    scope: ReplyScope::Session {
                        context,
                        client_context,
                    },
                    kind: Some(crate::cache::ListKind::GitLog),
                    global_refresh: false,
                    res,
                });
            });
        }
        Action::GitShow { hash } => {
            if state.awaiting_snapshot {
                state.git.show_in_flight = None;
                return;
            }
            if state.meta.is_none() {
                state.git.show_in_flight = None;
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                state.git.show_in_flight = None;
                return;
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(
                        client_context.clone(),
                        Request::GitShow { hash: hash.clone() },
                    )
                    .await;
                let _ = tx.send(BgResponse::GitShowReady {
                    context,
                    client_context,
                    hash,
                    res,
                });
            });
        }
        Action::GitDiff { hash } => {
            if state.meta.is_none() || state.awaiting_snapshot {
                queue_pending_request(state, Request::GitDiff { hash });
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                queue_pending_request(state, Request::GitDiff { hash });
                return;
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let res = cl
                    .request_session_for(
                        client_context.clone(),
                        Request::GitDiff { hash: hash.clone() },
                    )
                    .await;
                let _ = tx.send(BgResponse::GitDiffReady {
                    context,
                    client_context,
                    hash,
                    res,
                });
            });
        }
        Action::LoadPanel => {
            if state.meta.is_none() || state.awaiting_snapshot {
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                return;
            }
            let Some(target) = state.panel.target.clone() else {
                return;
            };
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
                PanelTarget::McpServer(_)
                | PanelTarget::FileDiff { .. }
                | PanelTarget::CommitDiff { .. } => None,
            };
            let Some(request) = request else {
                return;
            };
            let Some(token) = begin_panel_load(&context, &target) else {
                return;
            };
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let result = cl
                    .request_session_for(client_context.clone(), request)
                    .await;
                let _ = tx.send(BgResponse::LoadPanelReply {
                    context,
                    client_context,
                    target,
                    token,
                    result,
                });
            });
        }
        Action::LoadHistory => {
            if state.meta.is_none() || state.awaiting_snapshot || state.transcript.loading_history {
                return;
            }
            let context = state.session_context();
            let client_context = client.event_context();
            if client_context.session.is_none() {
                return;
            }
            let cl = Arc::clone(client);
            let tx = bg_tx.clone();
            let first_cell_id = state.transcript.oldest_seq();
            let limit = state.config.session.history_page;
            log::debug!("history page requested: before_seq={first_cell_id:?}, limit={limit}");
            state.transcript.loading_history = true;
            tokio::spawn(async move {
                let result = cl
                    .request_session_for(
                        client_context.clone(),
                        Request::GetHistory {
                            agent: AgentId::main(),
                            before_seq: first_cell_id,
                            limit,
                        },
                    )
                    .await;
                let _ = tx.send(BgResponse::LoadHistoryReply {
                    context,
                    client_context,
                    before_seq: first_cell_id,
                    result,
                });
            });
        }
    }
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

fn session_reply_is_current(
    state: &AppState,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> bool {
    state.context_is_current(context) && state.event_context_is_current(client_context)
}

fn deferred_reply_is_current(
    state: &AppState,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> bool {
    state.context_is_current(context)
        && (client_context.session.is_none() || state.event_context_is_current(client_context))
}

fn reply_scope_is_current(state: &AppState, scope: &ReplyScope) -> bool {
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

fn clear_list_loading(state: &mut AppState) {
    state.lists.models.in_flight = false;
    state.lists.plugins.in_flight = false;
    state.lists.marketplace.in_flight = false;
    state.lists.mcp.in_flight = false;
    state.lists.sessions.in_flight = false;
    state.lists.git.in_flight = false;
}

fn take_resync_action(state: &mut AppState, now: Instant) -> Option<Action> {
    let request = state.take_resync_request(now)?;
    clear_list_loading(state);
    Some(Action::Send(request))
}

fn take_global_resync_actions(state: &mut AppState, requested: bool) -> Vec<Action> {
    if !requested
        || state.meta.is_none()
        || state.pending_attach.is_some()
        || !matches!(state.connection, crate::state::Connection::Connected)
    {
        return Vec::new();
    }
    let context = state.ui_context();
    if state.global_refresh_context_is_current(context) {
        return Vec::new();
    }
    // A list already waiting in the pending queue is about to be asked for by
    // the attach/snapshot flush, so the refresh must not ask a second time.
    let queued: Vec<crate::cache::ListKind> = state
        .pending_requests
        .iter()
        .filter_map(crate::cache::ListKind::for_request)
        .collect();
    let actions = global_resync_actions()
        .into_iter()
        .filter(|action| match action {
            Action::Send(request) => crate::cache::ListKind::for_request(request)
                .is_none_or(|kind| !queued.contains(&kind) && !state.lists.get(kind).in_flight),
            _ => false,
        })
        .collect::<Vec<_>>();
    if !state.begin_global_refresh(actions.len()) {
        return Vec::new();
    }
    state.record_global_refresh_context(context);
    actions
}

fn release_failed_resync(state: &mut AppState, now: Instant) -> Vec<Action> {
    state.release_resync_barrier(true, now);
    state
        .drain_prompt_queue()
        .map(Action::Send)
        .into_iter()
        .collect()
}

fn attach_error_is_transient(error: &pacode_client::ClientError) -> bool {
    matches!(error, pacode_client::ClientError::Disconnected)
}

fn queue_pending_request(state: &mut AppState, req: Request) {
    if state.pending_requests.contains(&req) {
        log::debug!("pending request already queued: {}", req.name());
        return;
    }
    if state.pending_requests.len() >= PENDING_REQUESTS_CAP {
        let dropped = state.pending_requests.pop_front();
        log::warn!(
            "pending request queue full ({PENDING_REQUESTS_CAP}); dropped oldest: {dropped:?}"
        );
    }
    state.pending_requests.push_back(req);
}

/// A request the daemon cannot serve yet: the client holds no session for it,
/// so it waits for the next attach instead of vanishing. Losing it silently
/// strands whatever the user is blocked on — a permission reply, a question
/// answer, a cron edit.
fn defer_unbound_request(state: &mut AppState, req: Request) {
    let is_user_message = matches!(&req, Request::UserMessage { .. });
    let is_resync = matches!(&req, Request::GetSnapshot);
    if is_user_message {
        queue_user_message_request(state, req);
    } else if is_resync {
        state.release_resync_barrier(false, Instant::now());
    } else {
        queue_pending_request(state, req);
    }
}

/// Actions dispatched as one batch: the same request twice in a batch is one
/// request, not two. Post-attach flushes drain the pending queue and then ask
/// for the same lists again, and a plugin command can repeat its own send.
fn dedup_actions(actions: Vec<Action>) -> Vec<Action> {
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

/// Fold an attach/snapshot reply into state: record the slot, apply the
/// snapshot, flush requests queued while no session existed, and re-fetch
/// the git list on a project switch (§8.2). Shared by `Reply::Attached`/
/// `Reply::Snapshot` in [`handle_reply`] and `BgResponse::AttachFinished`.
fn apply_attach_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
    token: u64,
    client_context: &ClientEventContext,
    attach: &Attach,
) -> Option<Vec<Action>> {
    if !state.accepts_attach_with_token(context, token, client_context, attach, &snapshot) {
        return None;
    }
    state.pending_attach = None;
    state.accept_attach_context(client_context);
    Some(apply_snapshot_and_actions(state, snapshot))
}

fn apply_scoped_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
) -> Vec<Action> {
    if !state.context_is_current(context)
        || state
            .meta
            .as_ref()
            .is_some_and(|meta| meta.id != snapshot.meta.id)
    {
        return Vec::new();
    }
    apply_snapshot_and_actions(state, snapshot)
}

fn apply_resync_snapshot(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
    context: &SessionContext,
    client_context: &ClientEventContext,
) -> Option<Vec<Action>> {
    if !state.context_is_current(context)
        || snapshot.seq < state.last_event_seq.unwrap_or(0)
        || !state.accepts_event_snapshot(client_context, &snapshot)
    {
        return None;
    }
    let project_changed = state
        .meta
        .as_ref()
        .is_some_and(|meta| meta.cwd != snapshot.meta.cwd);
    if !state.apply_snapshot(snapshot, Instant::now()) {
        return None;
    }
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    if project_changed {
        actions.push(Action::GitLog { after: None });
    }
    Some(actions)
}

fn apply_snapshot_and_actions(
    state: &mut AppState,
    snapshot: pacode_types::SessionSnapshot,
) -> Vec<Action> {
    let project_changed = state
        .meta
        .as_ref()
        .is_some_and(|meta| meta.cwd != snapshot.meta.cwd);
    state.apply_snapshot(snapshot, Instant::now());
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    if project_changed {
        actions.push(Action::GitLog { after: None });
    }
    actions
}

fn global_resync_actions() -> Vec<Action> {
    vec![
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListMcpServers),
        Action::Send(Request::ListSessions { limit: 50 }),
        Action::Send(Request::ListModels),
        Action::Send(Request::ListAuth),
    ]
}

fn post_attach_actions(state: &mut AppState) -> Vec<Action> {
    let mut actions = Vec::new();
    if prompt_gate_open(state)
        && let Some(prompt) = state.initial_prompt.take()
    {
        actions.push(Action::Send(Request::UserMessage {
            text: prompt,
            images: Vec::new(),
            message_id: None,
        }));
    }
    actions.extend([
        Action::Send(Request::ListPlugins),
        Action::Send(Request::ListMcpServers),
        Action::Send(Request::ListSessions { limit: 50 }),
        Action::Send(Request::ListModels),
        Action::GitLog { after: None },
    ]);
    if !state.config.plugins.marketplace.is_empty() {
        let source = state.config.plugins.marketplace.clone();
        actions.push(Action::Send(Request::BrowseMarketplace {
            source,
            query: String::new(),
        }));
    }
    actions
}

fn flush_completed_attach(state: &mut AppState, had_pending_attach: bool) -> Vec<Action> {
    if !had_pending_attach || state.pending_attach.is_some() {
        return Vec::new();
    }
    let mut actions: Vec<Action> = state.pending_requests.drain(..).map(Action::Send).collect();
    actions.extend(post_attach_actions(state));
    actions
}

fn snapshot_actions_after_apply(
    state: &mut AppState,
    had_pending_attach: bool,
    snapshot_landed: bool,
    snapshot_applied: bool,
) -> Vec<Action> {
    if !snapshot_landed || !snapshot_applied {
        return Vec::new();
    }
    if had_pending_attach && state.pending_attach.is_none() {
        return flush_completed_attach(state, true);
    }
    state.pending_requests.drain(..).map(Action::Send).collect()
}

/// Service the work items recorded before paint: completion scans, image
/// preview/thumbnail renders, the stashed clipboard copy. Everything
/// lands on `spawn_blocking` — the frame path itself stays clean (§8.1).
/// Daemon-bound wants return as actions for `dispatch_action` instead — they
/// share the request's bookkeeping (`begin_log`, `mark_sent`).
fn dispatch_deferred(
    state: &mut AppState,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    client: &Client,
) -> Vec<Action> {
    if state.awaiting_snapshot {
        return Vec::new();
    }
    if let Some(key) = state.input.at_comp.claim_want() {
        let context = state.session_context();
        let client_context = client.event_context();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates = crate::at_complete::complete_at_path(&key.query, &key.cwd);
            let _ = tx.send(BgResponse::AtCompleted {
                context,
                client_context,
                key,
                candidates,
            });
        });
    }

    if let Some(key) = state.input.bash_comp.claim_want() {
        let context = state.session_context();
        let client_context = client.event_context();
        let history = state.input.bash_history.clone();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates =
                crate::bash::complete::complete(&key.text, key.cursor, &history, &key.cwd);
            let _ = tx.send(BgResponse::BashCompletionDone {
                context,
                client_context,
                key,
                candidates,
            });
        });
    }

    // Files preview: resolve + read/lex off-thread for the menu detail card
    // (§8.1 — the draw only records `preview_want`).
    if let Some(want) = state.files.preview_want.clone()
        && state.files.preview_in_flight.as_ref() != Some(&want)
        && state.files.cached_preview.as_ref().map(|c| &c.key) != Some(&want)
    {
        state.files.preview_in_flight = Some(want.clone());
        let context = state.session_context();
        let client_context = client.event_context();
        let cwd = state.cwd();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let resolved = resolve_preview_path(&want.raw, &cwd);
            let text = crate::ui::menu::files::text_preview(&resolved);
            let _ = tx.send(BgResponse::FilePreviewReady {
                context,
                client_context,
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
        let context = state.session_context();
        let client_context = client.event_context();
        let at_version = state.files.rows_version;
        let cwd = state.cwd();
        let paths: Vec<String> = state.files.rows.iter().map(|r| r.path.clone()).collect();
        let tx = bg_tx.clone();
        tokio::spawn(async move {
            let statuses = pacode_vcs::worktree_statuses(&cwd, &paths)
                .await
                .unwrap_or_default();
            let _ = tx.send(BgResponse::FileStatusesReady {
                context,
                client_context,
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
            let context = state.session_context();
            let client_context = client.event_context();
            let cwd = state.cwd();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let diff = pacode_vcs::file_diff(&cwd, &path).await;
                let _ = tx.send(BgResponse::FileDiffReady {
                    context,
                    client_context,
                    path,
                    diff,
                });
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
            let context = state.session_context();
            let client_context = client.event_context();
            let tx = bg_tx.clone();
            tokio::task::spawn_blocking(move || {
                let (text, truncated) = crate::ui::menu::background::read_tail(&path);
                let _ = tx.send(BgResponse::TaskTailReady {
                    context,
                    client_context,
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
    let context = state.session_context();
    let client_context = client.event_context();
    let render_wants = std::mem::take(&mut state.image_strip.want);
    let mut pending_wants = Vec::new();
    for want in render_wants {
        let k = (want.0, want.2, want.3);
        let already = state
            .image_strip
            .thumbs
            .get(&want.0)
            .is_some_and(|thumb| thumb.cell_w == want.2 && thumb.cell_h == want.3);
        if already || state.image_strip.in_flight.contains(&k) {
            continue;
        }
        if !state.image_strip.can_start_render() {
            if pending_wants.len() < crate::state::images::IMAGE_RENDER_MAX_WANTS {
                pending_wants.push(want);
            }
            continue;
        }
        let (token, path, cell_w, cell_h) = want;
        state.image_strip.in_flight.insert(k);
        let job_context = context.clone();
        let job_client_context = client_context.clone();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let preview = render_thumbnail(&path, cell_w, cell_h);
            let _ = tx.send(BgResponse::ThumbReady {
                context: job_context,
                client_context: job_client_context,
                token,
                cell_w,
                cell_h,
                preview,
            });
        });
    }
    state.image_strip.want = pending_wants;

    // The selection copy prepared before paint — OSC 52 + arboard, off-thread.
    if let Some(mut copy) = state.pending_copy.take() {
        let original_chars = copy.text.chars().count();
        if original_chars > crate::ui::COPY_MAX_CHARS {
            copy.text = copy.text.chars().take(crate::ui::COPY_MAX_CHARS).collect();
            log::warn!(
                "clipboard copy truncated from {original_chars} to {} chars",
                crate::ui::COPY_MAX_CHARS
            );
        }
        let context = state.session_context();
        let client_context = client.event_context();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::clipboard::copy(&copy.text) {
                log::warn!("clipboard copy failed: {e}");
            }
            let _ = tx.send(BgResponse::CopyDone {
                context,
                client_context,
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

/// Decode one inline thumbnail, bounded by file size: a kitty/sixel escape
/// carries the encoded image, and the frame path clones that payload on every
/// frame, so a huge paste would cost megabytes per frame for a slot. Past the
/// cap the slot answers `None` — the strip paints its placeholder and never
/// re-arms — while the prompt still attaches the original file.
fn render_thumbnail(
    path: &std::path::Path,
    cell_w: u16,
    cell_h: u16,
) -> Option<pacode_image::Preview> {
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if size > IMAGE_THUMB_MAX_BYTES {
        log::info!(
            "inline thumbnail skipped: {} is {size} bytes (cap {IMAGE_THUMB_MAX_BYTES})",
            path.display()
        );
        return None;
    }
    pacode_image::render(
        &pacode_image::PreviewRequest {
            path: path.to_path_buf(),
            cell_w,
            cell_h,
        },
        pacode_image::detect(),
    )
    .ok()
}

fn handle_bg_response(res: BgResponse, state: &mut AppState) -> Vec<Action> {
    dedup_actions(fold_bg_response(res, state))
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
        } => {
            if request_client_context.connection_epoch > client_context.connection_epoch
                || request_client_context.session_epoch > client_context.session_epoch
            {
                return Vec::new();
            }
            if !state.context_is_current(&context)
                || state.pending_attach.as_ref().is_none_or(|pending| {
                    pending.context != context || pending.attach != attach || pending.token != token
                })
            {
                return Vec::new();
            }
            match result {
                Ok(snapshot) => {
                    let had_pending_attach = state.pending_attach.is_some();
                    let Some(actions) = apply_attach_snapshot(
                        state,
                        snapshot,
                        &context,
                        token,
                        &client_context,
                        &attach,
                    ) else {
                        return Vec::new();
                    };
                    state.attach_in_flight = false;
                    log::info!(
                        "TUI attached to session {}",
                        state
                            .meta
                            .as_ref()
                            .map(|meta| meta.id.as_str())
                            .unwrap_or("")
                    );
                    let mut ordered_actions = actions;
                    ordered_actions.extend(flush_completed_attach(state, had_pending_attach));
                    ordered_actions
                }
                Err(pacode_client::ClientError::Timeout(err)) => {
                    state.attach_in_flight = false;
                    if state.request_attach_retry() {
                        log::warn!("session attach timed out; retrying the pending attempt: {err}");
                        Vec::new()
                    } else {
                        log::error!("session attach retry limit reached: {err}");
                        state.clear_pending_attach(format!("attach failed: {err}"));
                        if context.generation == 0 && context.slot == 0 {
                            state.attach_error = Some(pacode_client::ClientError::Timeout(err));
                            state.quit = true;
                        }
                        Vec::new()
                    }
                }
                Err(err) => {
                    state.attach_in_flight = false;
                    if attach_error_is_transient(&err) {
                        log::warn!("session attach will retry after reconnect: {err}");
                        return Vec::new();
                    }
                    let reason = err.to_string();
                    state.clear_pending_attach(reason.clone());
                    log::error!("session attach failed: {err}");
                    state.push_notice_with_level(
                        ToastLevel::Error,
                        format!("attach failed: {reason}"),
                    );
                    if context.generation == 0 && context.slot == 0 {
                        state.attach_error = Some(err);
                        state.quit = true;
                    }
                    Vec::new()
                }
            }
        }
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
        } => {
            if !state.resync_reply_is_current(&context, &client_context) {
                return Vec::new();
            }
            let mut actions = match result {
                Ok(Reply::Attached(snapshot) | Reply::Snapshot(snapshot)) => {
                    let Some(actions) =
                        apply_resync_snapshot(state, snapshot, &context, &client_context)
                    else {
                        log::warn!("snapshot resync returned a stale snapshot");
                        return release_failed_resync(state, Instant::now());
                    };
                    actions
                }
                Ok(Reply::Error { message }) => {
                    log::warn!("snapshot resync answered with error: {message}");
                    return release_failed_resync(state, Instant::now());
                }
                Ok(reply) => {
                    log::warn!("snapshot resync returned an unexpected reply: {reply:?}");
                    return release_failed_resync(state, Instant::now());
                }
                Err(error) => {
                    log::warn!("snapshot resync failed: {error}");
                    return release_failed_resync(state, Instant::now());
                }
            };
            if let Some(request) = state.drain_prompt_queue() {
                actions.push(Action::Send(request));
            }
            actions
        }
        BgResponse::UserMessageDone {
            ticket,
            client_context,
            result,
        } => {
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
        BgResponse::UserMessageStepDone {
            ticket,
            client_context,
            request,
            result,
        } => {
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
        } => {
            if !panel_load_reply_is_current(&context, &target, token) {
                return Vec::new();
            }
            if !session_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            let shown = state.panel.target.as_ref() == Some(&target);
            match result {
                Ok(Reply::History {
                    items, has_more, ..
                }) => {
                    if shown {
                        state.panel.agent_transcript.reset(items, has_more);
                        state.dirty = true;
                    }
                }
                Ok(Reply::TaskOutput {
                    lines, total_lines, ..
                }) => {
                    if shown {
                        state.panel.task_lines = lines;
                        state.panel.task_total_lines = total_lines;
                        state.dirty = true;
                    }
                }
                Ok(Reply::Error { message }) => {
                    if shown {
                        state.push_toast(
                            ToastLevel::Error,
                            "Panel load error".into(),
                            Some(message),
                            Instant::now(),
                        );
                    }
                }
                Err(e) if shown => {
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
        BgResponse::LoadHistoryReply {
            context,
            client_context,
            before_seq,
            result,
        } => {
            if !session_reply_is_current(state, &context, &client_context) {
                return Vec::new();
            }
            if state.transcript.oldest_seq() != before_seq {
                // A page that no longer answers the oldest row would wedge the
                // flag: the next scroll would never ask for history again.
                state.transcript.loading_history = false;
                return Vec::new();
            }
            match result {
                Ok(Reply::History {
                    items, has_more, ..
                }) => {
                    log::debug!(
                        "history page loaded: {} items, has_more={has_more}",
                        items.len()
                    );
                    let shift = state.transcript.prepend(items, has_more);
                    if !state.agent_replaces_dialog() {
                        state.selection.rebase_lines(shift);
                    }
                    state.dirty = true;
                }
                _ => {
                    log::warn!("history page request failed or returned non-history reply");
                    state.transcript.loading_history = false;
                }
            }
            vec![]
        }
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

/// Fold one daemon reply into state. List replies replace their section
/// wholesale (§8.2) and — once state moved — rewrite their cache file.
fn handle_reply(
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
