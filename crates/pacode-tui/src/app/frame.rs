//! The loop's two clocks and the draw gate.
//!
//! `FrameClock` paces content frames (`ui.ups`, with the refresh-rate probe
//! for `auto`); `AnimPacer` is the animation clock — a phase-stable ticker
//! armed only while something animates, stretched by backpressure; `Painter`
//! owns what the draw path keeps between frames — the gate baselines, the
//! dropped-frame accounting, the layout last drawn and the image escapes on
//! screen.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::TuiError;
use crate::layout::ScreenLayout;
use crate::state::AppState;
use crate::terminal::TerminalGuard;
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameClock {
    pub(super) ups: pacode_types::Ups,
    pub(super) detected_hz: Option<u16>,
    pub(super) probe_generation: u64,
    pub(super) interval: Option<Duration>,
}

impl FrameClock {
    pub(super) fn new(ups: pacode_types::Ups) -> Self {
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

    pub(super) fn set_ups(&mut self, ups: pacode_types::Ups) -> bool {
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

    pub(super) fn accept_probe(&mut self, generation: u64, hz: Option<u16>) -> bool {
        if generation != self.probe_generation || self.ups != pacode_types::Ups::Auto {
            return false;
        }
        self.detected_hz = hz;
        self.interval = self.ups.frame_interval(hz);
        true
    }
}

pub(super) fn spawn_refresh_probe(tx: mpsc::UnboundedSender<(u64, Option<u16>)>, generation: u64) {
    std::thread::spawn(move || {
        let _ = tx.send((generation, pacode_config::display::detect_refresh_hz()));
    });
}

/// The animation clock and the backpressure that paces it.
pub(super) struct AnimPacer {
    /// The animation clock. `Some` exactly while `anim_active` holds, and
    /// phase-stable across a period change (`backpressure::Ticker`), so neither
    /// a stream event nor a live `ui.animation` edit can reset its phase.
    tick: Option<crate::backpressure::Ticker>,
    /// The nominal period in force, and the backpressure state that stretches
    /// it. The budget is the nominal period itself — see `backpressure` for why
    /// a constant budget would degrade a client that is deliberately cheap.
    nominal_ms: u64,
    backpressure: crate::backpressure::Backpressure,
    /// The period the animation gate is measured against: the nominal while the
    /// frames are affordable, stretched by the multiplier while they are not.
    /// The second tick is not stretched — a clock reading that walks backwards is
    /// worse than one that skips a beat.
    period: Duration,
    /// Absolute animation clock. Every animated block reads its phase from this
    /// one elapsed value (see `backpressure::anim_phase`), so a degraded cadence
    /// slows the frames down and not the animation: skipped frames skip phases
    /// instead of queueing them.
    epoch: Instant,
    /// The phase every animated block reads: a count of steps taken since the
    /// last period change, not since startup. A change of cadence re-anchors the
    /// count rather than multiplying it, so 66 ms -> 16 ms does not throw the
    /// spinner 3.16 turns forward in a single frame.
    clock: crate::backpressure::PhaseClock,
}

impl AnimPacer {
    pub(super) fn new(nominal_ms: u64) -> Self {
        let backpressure = crate::backpressure::Backpressure::new(nominal_ms);
        let period = backpressure.period();
        Self {
            tick: None,
            nominal_ms,
            backpressure,
            period,
            epoch: Instant::now(),
            clock: crate::backpressure::PhaseClock::new(nominal_ms),
        }
    }

    pub(super) fn period(&self) -> Duration {
        self.period
    }

    /// Backpressure: the measured cost of the frame sets the cadence of
    /// the clock, not the cost of the clock setting it. The content gate
    /// is deliberately untouched — a keystroke is not a clock.
    fn observe_frame(&mut self, frame_cost: Duration) {
        if self.backpressure.observe(frame_cost) {
            self.period = self.backpressure.period();
            crate::render_metrics::RenderMetrics::note_multiplier(self.backpressure.multiplier());
        }
    }

    /// The animation clock: a phase-stable ticker, armed exactly while
    /// something animates — the same set of conditions that armed the old
    /// `anim_sleep`. Its cadence comes from `ui.animation` and the measured
    /// frame cost alone and never looks at the stream backlog: a growing
    /// backlog must not speed the spinner up.
    pub(super) fn rearm(&mut self, state: &AppState) {
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
        if wanted_anim_ms != self.nominal_ms {
            // A period change re-anchors the phase rather than re-deriving it
            // from the epoch: `anim_phase(elapsed, period)` alone multiplies
            // the phase by the ratio of the two periods, so leaving the fast
            // tick after 5 s puts the spinner at phase 312 where it was 75 —
            // a 3.16-turn jump in the frame the toast starts. The phase is the
            // number of steps taken since the last change, and the change
            // itself is not a step.
            self.clock
                .reanchor(crate::backpressure::elapsed_ms(self.epoch), wanted_anim_ms);
            self.nominal_ms = wanted_anim_ms;
            self.backpressure.set_nominal_ms(self.nominal_ms);
            self.period = self.backpressure.period();
        }
        // A period change — a transient animation starting or ending, a live
        // `ui.animation` edit, or the backpressure multiplier moving — is the
        // only reason the ticker is re-armed, and `Ticker` moves its deadline to
        // the next boundary of the new period instead of resetting the phase, so
        // none of the three can make the animation jerk.
        let anim_ms = self.period.as_millis() as u64;
        if anim_active {
            let wanted = Duration::from_millis(anim_ms);
            match self.tick.as_mut() {
                Some(t) => {
                    t.set_period(wanted, self.epoch);
                }
                None => {
                    self.tick = Some(crate::backpressure::Ticker::new(wanted, self.epoch));
                }
            }
        } else {
            // Nothing animates: drop the ticker so no timer is armed at rest.
            self.tick = None;
        }
    }

    /// The next animation tick; pends forever while no ticker is armed.
    pub(super) async fn tick(&mut self) {
        match self.tick.as_mut() {
            // `Ticker::tick` is cancel-safe: the select! can drop this
            // future for any other branch without losing the tick phase.
            Some(t) => {
                t.tick().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    pub(super) fn on_tick(&self, state: &mut AppState) {
        // The phase is read off the absolute animation clock rather
        // than advanced here: a frame this client skipped has to skip
        // its phase too, or the animation runs in slow motion exactly
        // when the client is too slow to draw it
        // (`backpressure::PhaseClock`).
        let elapsed = crate::backpressure::elapsed_ms(self.epoch);
        state.anim_frame = self.clock.phase(elapsed);
        // The sweep reads the same clock: elapsed time positions the
        // band, and the period actually in force is what keeps it to
        // one cell per painted frame. Under backpressure a band still
        // moving one cell per *nominal* tick would visibly skip, while
        // the rest of the animation correctly slows down.
        state.anim = pacode_render::AnimClock::new(elapsed, self.backpressure.period_ms());
        state.dirty_anim = true;
    }
}

/// What the draw path keeps between frames.
pub(super) struct Painter {
    last_draw: Instant,
    /// Baseline of the animation clock's draw gate, tracked separately so a
    /// content-gated frame does not displace the animation phase.
    last_anim_draw: Instant,
    /// Whether a missed content/anim frame has already been counted for the slot
    /// it missed. See the drop accounting at the draw gate for why a skip is
    /// latched rather than counted on every loop iteration.
    counted_content_skip: bool,
    counted_anim_skip: bool,
    /// Whether each gate was open on the previous pass, so a missed slot can be
    /// told apart from an event that has simply not reached its turn yet. False at
    /// startup: nothing has been painted yet, so no slot has been missed.
    content_slot_open: bool,
    anim_slot_open: bool,
    pub(super) last_layout: ScreenLayout,
    /// Input image-strip escapes currently on screen (spec §1): tracked so the
    /// post-draw flush can diff and never re-send tens of MB of kitty/sixel
    /// payload per frame.
    active_image_escapes: Vec<(ratatui::layout::Rect, String)>,
    /// Set when escapes were removed/moved: the next draw must repaint the
    /// cells the images covered (ratatui's diff can't see tty pixels).
    needs_image_repaint: bool,
}

impl Painter {
    pub(super) fn new(frame_interval: Option<Duration>) -> Self {
        let last_draw = Instant::now() - frame_interval.unwrap_or(Duration::from_millis(33));
        Self {
            last_draw,
            last_anim_draw: last_draw,
            counted_content_skip: false,
            counted_anim_skip: false,
            content_slot_open: false,
            anim_slot_open: false,
            last_layout: ScreenLayout::default(),
            active_image_escapes: Vec::new(),
            needs_image_repaint: false,
        }
    }

    /// Redraw on two independent clocks: content (`state.dirty`) is gated by
    /// `frame_interval` (`ui.ups`); animation (`state.dirty_anim`) is gated
    /// by the current animation period. Either flag with its gate elapsed
    /// draws a frame that flushes both, so a pending anim frame can ride a
    /// content draw and never stalls waiting on the slower clock.
    pub(super) fn draw_if_due(
        &mut self,
        term_guard: &mut TerminalGuard,
        state: &mut AppState,
        frame_interval: Option<Duration>,
        anim: &mut AnimPacer,
        detected_hz: Option<u16>,
        timing: bool,
    ) -> Result<(), TuiError> {
        let now = Instant::now();
        let content_gate_open = match frame_interval {
            Some(interval) => now.saturating_duration_since(self.last_draw) >= interval,
            None => true,
        };
        let anim_gate_open = now.saturating_duration_since(self.last_anim_draw) >= anim.period;
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
            if self.content_slot_open
                && state.dirty
                && !content_gate_open
                && !self.counted_content_skip
            {
                crate::render_metrics::RenderMetrics::record_drop();
                self.counted_content_skip = true;
            }
            if self.anim_slot_open && state.dirty_anim && !anim_gate_open && !self.counted_anim_skip
            {
                crate::render_metrics::RenderMetrics::record_drop();
                self.counted_anim_skip = true;
            }
            // The slot is open: whatever is pending gets painted, so the next
            // miss is a new one.
            self.counted_content_skip &= !content_gate_open;
            self.counted_anim_skip &= !anim_gate_open;
            self.content_slot_open = content_gate_open;
            self.anim_slot_open = anim_gate_open;
        }
        if !(content_ready || anim_ready) {
            return Ok(());
        }
        let flushed_anim = state.dirty_anim;
        let t_frame = Instant::now();
        let recording = crate::render_metrics::RenderMetrics::recording();
        let frame_kind = if self.needs_image_repaint {
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
        let repainted = self.needs_image_repaint;
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
            self.needs_image_repaint = false;
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
        let last_layout = &mut self.last_layout;
        let draw_res = term_guard
            .terminal
            .draw(|f| {
                let prepared = ui::prepare_frame(f.area(), state, Instant::now());
                *last_layout = ui::draw(f, state, &prepared);
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
            crate::timing::frame_budget(detected_hz),
        );
        anim.observe_frame(frame_cost);
        state.dirty = false;
        state.dirty_anim = false;
        self.last_draw = Instant::now();
        if flushed_anim {
            self.last_anim_draw = self.last_draw;
        }

        // Input image-strip escapes stashed by `ui::images::draw` —
        // emitted only on change.
        let next_escapes = std::mem::take(&mut state.pending_image_escapes);
        if next_escapes != self.active_image_escapes {
            if !self.active_image_escapes.is_empty() {
                let _ = term_guard.clear_image_escapes();
                self.needs_image_repaint = true;
            }
            for (area, esc) in &next_escapes {
                let _ = term_guard.draw_image_escape(*area, esc);
            }
            self.active_image_escapes = next_escapes;
        }
        Ok(())
    }

    /// How long until a pending frame's own gate opens: the content gate for
    /// `state.dirty`, the animation gate for `state.dirty_anim`. The earliest
    /// of the two arms the sleep, so an animation frame is never stalled
    /// waiting on the content clock.
    pub(super) fn next_draw_wait(
        &self,
        state: &AppState,
        frame_interval: Option<Duration>,
        anim_period: Duration,
    ) -> Option<Duration> {
        let content_wait = if state.dirty {
            frame_interval.map(|interval| {
                let elapsed = Instant::now().saturating_duration_since(self.last_draw);
                interval.saturating_sub(elapsed)
            })
        } else {
            None
        };
        let anim_wait = if state.dirty_anim {
            let elapsed = Instant::now().saturating_duration_since(self.last_anim_draw);
            Some(anim_period.saturating_sub(elapsed))
        } else {
            None
        };
        content_wait.into_iter().chain(anim_wait).min()
    }

    /// Clear the image escapes still on screen (shutdown).
    pub(super) fn clear_image_escapes(&mut self, term_guard: &mut TerminalGuard) {
        if !self.active_image_escapes.is_empty() {
            let _ = term_guard.clear_image_escapes();
            self.active_image_escapes.clear();
        }
    }
}
