//! Frame scheduler: nominal cadence, measured backpressure, one shared clock.
//!
//! # The cadence
//!
//! The nominal animation periods stay where they are — 66/100/240 ms, and
//! 16/33/70 ms for the fast transient tick (`pacode_types::AnimationMode`).
//! Nothing here raises the nominal rate.
//!
//! # Backpressure
//!
//! A frame that costs more than the nominal period cannot be painted at the
//! nominal rate: the client would spend all of its time inside the draw and
//! none of it answering input, and every input event would arrive as a dropped
//! slot. So the measured cost of the last few frames sets a *multiplier* on
//! the nominal period — the same shape as oh-my-pi's adaptive backpressure
//! (`FRAME_COST_CEILING_MS = 200`, degradation up to x9), with the thresholds
//! below rather than its 30 fps nominal, which this project explicitly does
//! not want.
//!
//! The thresholds, and why:
//!
//! - **budget = the nominal animation period.** Not a fixed 200 ms: the point
//!   of the degradation is to keep up with the clock the user configured. A
//!   frame that costs less than one period has slack; a frame that costs more
//!   cannot keep it. At the 66 ms `smooth` nominal a 20 ms frame is 30% of the
//!   budget — three frames of slack, and no degradation at all. At the 240 ms
//!   `eco` nominal the same 20 ms frame is 8% — a UI that is deliberately
//!   already cheap is not slowed down by being cheap.
//! - **five consecutive over-budget frames to degrade, one to recover.** At
//!   the 66 ms nominal five is 330 ms of genuinely slow frames; one slow frame
//!   (a first paint of a huge transcript, a resize) must not slow the client
//!   down, and recovery must be immediate or a single hitch costs a second of
//!   a degraded spinner.
//! - **+x2 per step up to x9.** The step has to be big enough to escape a
//!   regime that is one notch too fast, and small enough not to overshoot into
//!   a sluggish x8 when x2 would do. x9 at 66 ms is 594 ms — a clock coarse
//!   enough to read as "chugging" rather than "stuttering", which is the right
//!   place to end up when a frame costs more than a tenth of a second.
//! - **the content clock is never degraded.** A content frame is a
//!   response to a keystroke or a token; stretching those is what makes a
//!   client feel broken. Backpressure only ever slows the clock a user is not
//!   waiting on.
//!
//! # Why the animation does not stutter
//!
//! A degraded cadence means fewer *frames*, and if the animation phase is a
//! frame counter the animation itself slows down with it — pac-man would
//! cross the track in five seconds. So the phase is derived from absolute
//! elapsed time instead: `phase = elapsed_ms / period_ms`
//! ([`anim_phase`]), the same principle as `state::activity::thinking_color`,
//! where the colour is a function of elapsed milliseconds and not of how many
//! redraws happened to land. Dropping frames then skips intermediate phases
//! rather than queueing them, and a frame that took 200 ms advances the
//! animation three steps in one go. This is also the shared clock: every
//! animated block reads its phase from one `elapsed_ms` — the loop's — instead
//! of keeping a timer of its own.

use std::time::{Duration, Instant};

use pacode_types::metrics::LatencyHist;

/// Sustained over-budget frames before the cadence degrades. At the 66 ms
/// nominal that is 330 ms of genuinely slow frames.
pub const DEGRADE_AFTER: u32 = 5;

/// Multiplier added per degradation step, and the ceiling — the oh-my-pi
/// constants (`DEGRADE_MULTIPLIER`, `MAX_CADENCE_MULTIPLIER`).
pub const DEGRADE_MULTIPLIER: u32 = 2;
pub const MAX_CADENCE_MULTIPLIER: u32 = 9;

/// Floor under a degraded period. A nominal of 16 ms times nine is 144 ms,
/// which is fine; this only matters if a future nominal is tiny, and a period
/// below a frame's own cost would spin.
const MIN_DEGRADED_MS: u64 = 16;

/// Animation phase for `elapsed_ms` of animation time at `period_ms` per
/// step. Absolute time in, phase out: the caller can skip any number of frames
/// and land on the phase the clock is actually at.
///
/// A zero `period_ms` is a frozen phase, not a division: a misconfigured
/// cadence should show a still spinner rather than one advancing once per
/// millisecond. An `elapsed_ms` longer than the phase range saturates rather
/// than wrapping, because a phase is something widgets divide and index — a
/// wrapped one is a panic in someone else's render.
pub fn anim_phase(elapsed_ms: u64, period_ms: u64) -> u64 {
    if period_ms == 0 {
        return 0;
    }
    elapsed_ms / period_ms
}

/// Milliseconds of animation time elapsed since `epoch`, saturating.
///
/// The one clock every animated block reads: the loop's, handed down instead of
/// a block starting a timer of its own. `Instant::elapsed` is monotonic, so a
/// long-lived client cannot wrap the way a wall clock would.
pub fn elapsed_ms(epoch: Instant) -> u64 {
    let millis = epoch.elapsed().as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// A phase-stable `tokio::time::Interval` that survives a period change.
///
/// `tokio::time::interval` starts its first tick immediately, and re-creating
/// it on every period change (a transient animation starting or ending, or
/// `ui.animation` edited live) threw that away: the first tick after a change
/// landed at once and every following tick was offset from the one before it
/// by exactly the amount the phase had moved. [`Ticker::set_period`] instead
/// moves the deadline to the next boundary of the *new* period measured from
/// the same origin, so the tick keeps firing on the `origin + k * period` grid
/// and a change is a phase shift, not a reset.
#[derive(Debug)]
pub struct Ticker {
    inner: tokio::time::Interval,
}

impl Ticker {
    /// A ticker for `period`, firing on the `origin + k * period` boundaries
    /// that are still ahead of now.
    pub fn new(period: Duration, origin: Instant) -> Self {
        Self::from(origin.into(), tokio::time::Instant::now(), period)
    }

    /// A ticker for `period` measured from `origin`, taken now.
    ///
    /// `origin` and `now` are tokio instants rather than `std::time::Instant`
    /// because tokio's paused clock is a *wrapper* around the std one, and on a
    /// paused clock `tokio::time::Instant::now()` is not `std::time::Instant::now()`.
    /// A ticker built from the std clock would read "now" as the real wall clock
    /// — far past every boundary a paused test has advanced to — and its first
    /// tick would be overdue. One type for both clocks is what keeps the tests
    /// that pin the boundary grid meaningful.
    ///
    /// `period` is an absolute zero for a cadence, so a zero period is dropped
    /// rather than built: an `Interval` with a zero period is a loop that yields
    /// as fast as the runtime can be polled, and a caller that asked for one has
    /// a bug that should not become a spin. 1 ms is the floor, which is also what
    /// the backpressure period floor enforces.
    fn from(origin: tokio::time::Instant, now: tokio::time::Instant, period: Duration) -> Self {
        let period = period.max(Duration::from_millis(1));
        Self {
            inner: interval_from(origin, now, period),
        }
    }

    pub fn period(&self) -> Duration {
        self.inner.period()
    }

    /// Move to `period` if it differs, keeping the phase: the next deadline is
    /// the next boundary of `period` after `origin`, and the ones after that
    /// keep the same grid.
    pub fn set_period(&mut self, period: Duration, origin: Instant) -> bool {
        if self.inner.period() == period {
            return false;
        }
        self.inner = Self::from(origin.into(), tokio::time::Instant::now(), period).inner;
        true
    }

    /// Wait for the next tick. Cancel-safe: dropping this future loses the
    /// wait, not the phase.
    pub async fn tick(&mut self) {
        self.inner.tick().await;
    }
}

/// An `Interval` firing on the next boundary of `period` strictly after `now`,
/// measured from `origin`.
///
/// The first deadline is the first boundary *after the moment the ticker is
/// built*, not `origin + period`. A ticker created a long time after its origin
/// — which is every ticker in this client, since `anim_epoch` is taken once at
/// startup and the ticker is armed when the first thing starts animating —
/// would otherwise be handed a deadline in the past, and tokio fires an overdue
/// `Interval` on the first `tick()`: the animation would paint a frame the
/// instant it was created, and every following tick would sit off the boundary
/// grid by however long the ticker stood unarmed.
fn interval_from(
    origin: tokio::time::Instant,
    now: tokio::time::Instant,
    period: Duration,
) -> tokio::time::Interval {
    let start = next_boundary_after(origin, now, period);
    let mut inner = tokio::time::interval_at(start, period);
    inner.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    inner
}

/// The first `origin + k * period` with `k >= 1` that is strictly after `now`.
///
/// `now` sitting exactly on a boundary is the case worth spelling out. A client
/// arms its ticker on the tick that started an animation, so `now == origin` is
/// routine, and the boundary at `origin` itself is not a tick to wait for: the
/// first *future* boundary is `origin + period`. The same holds for a period
/// change: changing period at the instant of a tick must land the next tick on
/// the new grid's next boundary, which is the one after the one already reached.
/// So the step is `floor((now - origin) / period) + 1` and nothing else — no
/// "if it does not look strictly greater, give up and use now + period" clause,
/// which pushed the tick a whole period late every time it was reached.
///
/// Saturating throughout: `as_nanos` on a span longer than `u64` nanoseconds
/// (584 years) saturates rather than wrapping, and the `checked_mul` on the way
/// back falls out to the next whole period past `now` — the answer a
/// non-representable boundary would have to be anyway.
fn next_boundary_after(
    origin: tokio::time::Instant,
    now: tokio::time::Instant,
    period: Duration,
) -> tokio::time::Instant {
    if period.is_zero() {
        return origin;
    }
    if now <= origin {
        return origin + period;
    }
    let since = now - origin;
    let whole = u64::try_from(since.as_nanos() / period.as_nanos()).unwrap_or(u64::MAX);
    let Some(step) = period.checked_mul(u32::try_from(whole).unwrap_or(u32::MAX)) else {
        return now + period;
    };
    origin + step + period
}

/// The shared animation phase, re-anchored on every period change.
///
/// [`anim_phase`] on its own is enough only while the period holds: it divides
/// elapsed time by the period, so a change multiplies the phase by the ratio of
/// the two. Dropping from the 66 ms nominal to the 16 ms transient tick after
/// five seconds would put the spinner at phase 312 where it was showing 75 — a
/// 3.16-turn jump inside the one frame the toast appears in, which reads as a
/// glitch rather than as a cadence change.
///
/// So the phase is a count of steps taken since the *last* change, and the
/// change is not a step: [`PhaseClock::reanchor`] settles the old period's
/// fractional tail into the base before the new one starts counting. Dropped
/// frames still skip phases — the clock is absolute, not a counter of draws —
/// which is the property [`anim_phase`] exists for.
#[derive(Debug, Clone, Copy)]
pub struct PhaseClock {
    period_ms: u64,
    base: u64,
    elapsed_at_base_ms: u64,
}

impl PhaseClock {
    pub fn new(period_ms: u64) -> Self {
        Self {
            period_ms,
            base: 0,
            elapsed_at_base_ms: 0,
        }
    }

    pub fn period_ms(&self) -> u64 {
        self.period_ms
    }

    /// The current phase for `elapsed_ms` since the epoch. Saturating, and a
    /// pure function of the clock: calling it twice with the same elapsed time
    /// gives the same phase.
    pub fn phase(&self, elapsed_ms: u64) -> u64 {
        self.base.saturating_add(anim_phase(
            elapsed_ms.saturating_sub(self.elapsed_at_base_ms),
            self.period_ms,
        ))
    }

    /// Count on `period_ms` from now, without disturbing the phase.
    ///
    /// A zero period is *not* clamped: [`anim_phase`] already defines it as a
    /// frozen phase rather than a division by zero, and a client whose
    /// configured cadence is zero should show a still spinner instead of one
    /// advancing once per millisecond.
    pub fn reanchor(&mut self, elapsed_ms: u64, period_ms: u64) {
        if period_ms == self.period_ms {
            return;
        }
        self.base = self.phase(elapsed_ms);
        self.elapsed_at_base_ms = elapsed_ms;
        self.period_ms = period_ms;
    }
}

/// The backpressure state of one client: the multiplier in force, the streak
/// that produced it, and the distribution of the decisions it made.
#[derive(Debug)]
pub struct Backpressure {
    nominal_ms: u64,
    multiplier: u32,
    over: u32,
    under: u32,
    decisions: LatencyHist,
}

impl Backpressure {
    /// `nominal_ms` is the animation period in force, and therefore the frame
    /// cost budget: see the module docs for why the budget is the period and
    /// not a constant.
    pub fn new(nominal_ms: u64) -> Self {
        Self {
            nominal_ms: nominal_ms.max(1),
            multiplier: 1,
            over: 0,
            under: 0,
            decisions: LatencyHist::new(),
        }
    }

    /// Feed one frame's total cost. Returns `true` when the multiplier moved.
    pub fn observe(&mut self, cost: Duration) -> bool {
        self.decisions.record_duration(cost);
        let budget = self.budget();
        if cost > budget {
            self.under = 0;
            self.over = self.over.saturating_add(1);
            if self.over >= DEGRADE_AFTER && self.multiplier < MAX_CADENCE_MULTIPLIER {
                self.over = 0;
                self.multiplier = (self.multiplier.saturating_mul(DEGRADE_MULTIPLIER))
                    .min(MAX_CADENCE_MULTIPLIER);
                return true;
            }
            return false;
        }
        self.over = 0;
        self.under = self.under.saturating_add(1);
        // One good frame is enough: recovery must be immediate or a single
        // hitch costs a second of a degraded spinner.
        if self.under >= 1 && self.multiplier > 1 {
            self.under = 0;
            self.multiplier = (self.multiplier / DEGRADE_MULTIPLIER).max(1);
            return true;
        }
        false
    }

    /// What a frame is allowed to cost right now.
    pub fn budget(&self) -> Duration {
        Duration::from_micros(self.nominal_ms.saturating_mul(1000))
    }

    pub fn multiplier(&self) -> u32 {
        self.multiplier
    }

    /// The cadence the animation clock should run at: the nominal period
    /// stretched by the multiplier, never below [`MIN_DEGRADED_MS`].
    pub fn period(&self) -> Duration {
        let ms = self
            .nominal_ms
            .saturating_mul(self.multiplier as u64)
            .max(MIN_DEGRADED_MS);
        Duration::from_millis(ms)
    }

    /// [`Self::period`] in whole milliseconds, which is the unit the animation
    /// clock and the sweep both work in. The period is already built from
    /// milliseconds, so the conversion is exact for every value the multiplier
    /// and the cap can produce.
    pub fn period_ms(&self) -> u64 {
        self.nominal_ms
            .saturating_mul(self.multiplier as u64)
            .max(MIN_DEGRADED_MS)
    }

    /// Adopt a new nominal (a live `ui.animation` edit). The multiplier is
    /// kept: the client is still slow at the new rate, and re-measuring it from
    /// scratch would show a clean client for five frames after a change.
    pub fn set_nominal_ms(&mut self, nominal_ms: u64) {
        self.nominal_ms = nominal_ms.max(1);
    }

    /// The distribution of every observed frame cost, for the report.
    pub fn snapshot(&self) -> pacode_types::metrics::HistogramSnapshot {
        self.decisions.snapshot()
    }
}

#[cfg(test)]
#[path = "backpressure_tests.rs"]
mod tests;
