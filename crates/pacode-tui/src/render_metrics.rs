//! What the frame path is actually paying, measured at the boundaries it owns.
//!
//! Every counter here is updated on a frame event, never by a timer: an idle
//! client has no frames, so an idle client pays nothing to be measurable — the
//! same rule the daemon's own metrics follow. Recording is off until someone
//! arms it ([`MetricsGate`]), because on a client the histograms are the only
//! per-frame cost, and a client that never runs `pacode stats` should not pay
//! for a report nobody reads.
//!
//! The latencies reuse `pacode_types::metrics::LatencyHist` — the same log2
//! microsecond histogram `pacode stats` already prints for store and provider
//! calls — so a quantile means the same thing in both places and there is one
//! quantile implementation rather than two.
//!
//! # Where the reading lives
//!
//! A TUI is a client, not a service: a frame that happened in a process which
//! has since exited cannot be fetched over the socket. So the client writes one
//! JSON line into the state directory on quit, and `pacode stats` reads it next
//! to the daemon snapshot it already fetches. One line, replaced wholesale, so
//! the cost is bounded by the number of counters rather than by the run length.
//!
//! # What is deliberately not measured
//!
//! Allocations. Counting them needs a counting global allocator, and swapping
//! the allocator to instrument a report nobody asked for is a trade this
//! codebase does not make — `pacode` already offers a `mimalloc` feature for
//! the cases where allocator behaviour is the question. `Vec` capacities and
//! the line cache are the levers instead, and those are in the cache's own
//! accounting.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use pacode_types::metrics::{HistogramSnapshot, LatencyHist};

/// Environment variable that arms the frame metrics for the process. Off by
/// default: see the module docs.
pub const METRICS_ENV: &str = "PACODE_FRAME_METRICS";

/// File the reading lands in, under `Paths::state_dir`.
const REPORT_FILE: &str = "render-metrics.json";

/// How the frame path was entered. Content and animation are counted apart
/// because they are paid for by different things: a content frame is the price
/// of new information, an animation frame is the price of a clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameKind {
    /// `dirty_anim` alone: the animation clock asked for the frame.
    #[default]
    Anim,
    /// A dirty flag with new content behind it (stream, key, daemon event).
    Content,
    /// `Terminal::clear()` ran first (image escapes moved): ratatui repaints
    /// every cell, which is the most expensive shape a frame can take. Still
    /// counted as content — it was content that caused it.
    Full,
}

impl FrameKind {
    /// Text form for the report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anim => "anim",
            Self::Content => "content",
            Self::Full => "full",
        }
    }

    /// Whether the frame was entered on a dirty flag rather than the clock.
    pub fn is_content(self) -> bool {
        !matches!(self, Self::Anim)
    }
}

/// A frame's cost, split at the boundary that belongs to different code:
/// `layout` is the transcript and the widgets, `backend` is everything
/// ratatui does after the render closure — the diff, the write and the flush.
/// `frame` is the whole thing. Read together they say whether a slow client is
/// slow in its own rendering or slow in the terminal.
///
/// There is deliberately no third split. `Terminal::draw` runs the closure and
/// then applies the buffer inside one call, so "the write" is not separable
/// from the diff that produces it: the only instant available between them is
/// the return, and a reading taken there is the same number twice. A separate
/// `write` histogram would have been a second label on one measurement.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCost {
    /// The render closure: `prepare_frame`, the widgets, the font pass.
    pub layout: Duration,
    /// From the end of the render closure to the return of `draw`: ratatui's
    /// cell diff, the buffer write, and the backend flush. Also carries
    /// `autoresize` when the terminal changed size, which ratatui runs before
    /// the closure and which is not separately timeable.
    pub backend: Duration,
    /// Time from the start of the frame to the end of the write. `layout` and
    /// `backend` are both inside it, and `frame - layout - backend` is the
    /// sync markers and a `Terminal::clear()`.
    pub frame: Duration,
}

/// Point-in-time reading of the frame path, printed by `pacode stats`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenderStats {
    /// Draws in this window, all kinds.
    pub draws: u64,
    /// Draws entered with `state.dirty` set.
    pub content_draws: u64,
    /// Draws entered on the animation clock alone.
    pub anim_draws: u64,
    /// Draws that followed a `Terminal::clear()`.
    pub full_draws: u64,
    /// Frames the scheduler wanted and did not paint, because the slot was too
    /// soon after the previous one. Without this the cadence multiplier would
    /// be indistinguishable from a quiet client.
    pub dropped_frames: u64,
    /// Multiplier the backpressure scheduler was applying at the last frame
    /// (1 = nominal cadence).
    pub cadence_multiplier: u64,
    /// Characters the stream could not hand to a frame yet, at its peak since
    /// this client started.
    pub stream_backlog_high_water: u64,
    /// Characters still queued at the last frame. `None` while the strip is
    /// empty: reporting 0 there would hide the difference between "drained"
    /// and "never had one".
    pub stream_backlog: Option<u64>,
    /// Wall clock the reading was taken from, so a report that outlived its
    /// window is legible rather than silently current.
    pub sampled_at_ms: u64,
    pub frame: HistogramSnapshot,
    pub layout: HistogramSnapshot,
    pub backend: HistogramSnapshot,
    pub clear: HistogramSnapshot,
    /// Frames that also reallocated ratatui's buffers. A resize cannot be
    /// timed separately: ratatui runs `autoresize` inside `Terminal::draw`,
    /// before the render closure, so its cost is inseparable from the frame's
    /// and lands in `frame`. The counter is what keeps one window drag from
    /// reading as a client that is slow at painting.
    pub resize_frames: u64,
}

#[derive(Debug, Default)]
struct Counters {
    draws: AtomicU64,
    content: AtomicU64,
    anim: AtomicU64,
    full: AtomicU64,
    dropped: AtomicU64,
    resizes: AtomicU64,
    backlog_high_water: AtomicU64,
    backlog_live: AtomicBool,
    backlog: AtomicU64,
    multiplier: AtomicU64,
}

/// The live counters: one per client process, recorded only while armed.
#[derive(Debug)]
pub struct RenderMetrics {
    on: AtomicBool,
    counters: Counters,
    frame: LatencyHist,
    layout: LatencyHist,
    backend: LatencyHist,
    clear: LatencyHist,
}

impl RenderMetrics {
    const fn new() -> Self {
        Self {
            on: AtomicBool::new(false),
            counters: Counters {
                draws: AtomicU64::new(0),
                content: AtomicU64::new(0),
                anim: AtomicU64::new(0),
                full: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                resizes: AtomicU64::new(0),
                backlog_high_water: AtomicU64::new(0),
                backlog_live: AtomicBool::new(false),
                backlog: AtomicU64::new(0),
                multiplier: AtomicU64::new(1),
            },
            frame: LatencyHist::new(),
            layout: LatencyHist::new(),
            backend: LatencyHist::new(),
            clear: LatencyHist::new(),
        }
    }

    /// Whether recording is on. Every call site asks once and takes the
    /// trivial branch when it is off.
    pub fn recording() -> bool {
        METRICS.on.load(Ordering::Relaxed)
    }

    /// Record a finished frame. A no-op while recording is off, so the call
    /// site stays branch-free apart from this.
    pub fn record(kind: FrameKind, cost: FrameCost) {
        if !Self::recording() {
            return;
        }
        METRICS.frame.record_duration(cost.frame);
        METRICS.layout.record_duration(cost.layout);
        METRICS.backend.record_duration(cost.backend);
        let c = &METRICS.counters;
        c.draws.fetch_add(1, Ordering::Relaxed);
        match kind {
            FrameKind::Anim => {
                c.anim.fetch_add(1, Ordering::Relaxed);
            }
            FrameKind::Content => {
                c.content.fetch_add(1, Ordering::Relaxed);
            }
            FrameKind::Full => {
                c.content.fetch_add(1, Ordering::Relaxed);
                c.full.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A frame the scheduler wanted and did not paint.
    ///
    /// Gated like every other recorder: the counters are read by `pacode stats`
    /// from a live process, and a client that never opted in must report zeroes
    /// rather than the shape of whatever else shares the process.
    pub fn record_drop() {
        if !Self::recording() {
            return;
        }
        METRICS.counters.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// A `Terminal::clear()` — the buffer reset an image repaint forces. Rare
    /// and the most expensive shape a frame can take, so it is counted apart
    /// from the frame total it is already inside.
    pub fn record_clear(elapsed: Duration) {
        if Self::recording() {
            METRICS.clear.record_duration(elapsed);
        }
    }

    /// A frame that also carried a terminal resize: ratatui reallocated both
    /// buffers and cleared them before the closure ran. Counted, not timed —
    /// see [`RenderStats::resize_frames`].
    pub fn record_resize_frame() {
        if !Self::recording() {
            return;
        }
        METRICS.counters.resizes.fetch_add(1, Ordering::Relaxed);
    }

    /// Publish the multiplier the scheduler is applying, so `stats` can explain
    /// the drop counter printed next to it.
    pub fn note_multiplier(multiplier: u32) {
        if !Self::recording() {
            return;
        }
        METRICS
            .counters
            .multiplier
            .store(multiplier as u64, Ordering::Relaxed);
    }

    /// The output backlog at the start of a frame: characters the stream still
    /// held, i.e. how far the reader was behind the model.
    pub fn note_backlog(chars: usize) {
        if !Self::recording() {
            return;
        }
        let c = &METRICS.counters;
        c.backlog.store(chars as u64, Ordering::Relaxed);
        c.backlog_live.store(true, Ordering::Relaxed);
        c.backlog_high_water
            .fetch_max(chars as u64, Ordering::Relaxed);
    }

    /// The reading, from the process-wide counters.
    pub fn snapshot() -> RenderStats {
        let c = &METRICS.counters;
        RenderStats {
            draws: c.draws.load(Ordering::Relaxed),
            content_draws: c.content.load(Ordering::Relaxed),
            anim_draws: c.anim.load(Ordering::Relaxed),
            full_draws: c.full.load(Ordering::Relaxed),
            dropped_frames: c.dropped.load(Ordering::Relaxed),
            cadence_multiplier: c.multiplier.load(Ordering::Relaxed).max(1),
            stream_backlog_high_water: c.backlog_high_water.load(Ordering::Relaxed),
            stream_backlog: c
                .backlog_live
                .load(Ordering::Relaxed)
                .then(|| c.backlog.load(Ordering::Relaxed)),
            sampled_at_ms: pacode_types::time::now_ms(),
            frame: METRICS.frame.snapshot(),
            layout: METRICS.layout.snapshot(),
            backend: METRICS.backend.snapshot(),
            clear: METRICS.clear.snapshot(),
            resize_frames: c.resizes.load(Ordering::Relaxed),
        }
    }
}

static METRICS: RenderMetrics = RenderMetrics::new();

/// Arms the frame metrics for this process and writes the reading on drop.
///
/// `state_dir` is the `Paths::state_dir` this client was given. Nothing is
/// written unless recording is armed: a run that never asked to be measured
/// leaves no trace, and in particular does not delete the previous run's
/// reading — that file belongs to the client that wrote it, and `stats`
/// reports it with whatever `sampled_at_ms` it carries.
pub struct MetricsGate {
    path: Option<PathBuf>,
}

impl MetricsGate {
    pub fn install(state_dir: &Path) -> Self {
        if !env_requests_metrics(std::env::var(METRICS_ENV).ok().as_deref()) {
            return Self { path: None };
        }
        METRICS.on.store(true, Ordering::Relaxed);
        Self {
            path: Some(state_dir.join(REPORT_FILE)),
        }
    }

    /// Where the reading went, once this gate is dropped.
    pub fn report_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Serialize the reading. Failure is a debug line and nothing else:
    /// telemetry must never cost a frame or a session.
    pub fn write_report(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let bytes = match serde_json::to_vec(&RenderMetrics::snapshot()) {
            Ok(bytes) => bytes,
            // The struct is all integers and a histogram of integers, so this
            // cannot happen; treating it as a failed write keeps the gate
            // infallible rather than propagating a serialization bug into quit.
            Err(error) => {
                log::debug!("frame metrics: report not serialized: {error}");
                return;
            }
        };
        if let Err(error) = write_atomic(path, &bytes) {
            log::debug!("frame metrics: report not written: {error}");
        }
    }
}

impl Drop for MetricsGate {
    fn drop(&mut self) {
        self.write_report();
    }
}

/// `PACODE_FRAME_METRICS` as a pure function, so the gate is testable without
/// mutating the process environment.
pub fn env_requests_metrics(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true"))
}

/// Read the previous client's reading, if it left one. `None` for a missing or
/// unreadable file: a report is telemetry, and a corrupt one must not turn
/// `pacode stats` into a failure.
pub fn read_report(path: &Path) -> Option<RenderStats> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[path = "render_metrics_tests.rs"]
mod tests;
