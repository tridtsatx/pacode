//! The scheduler's contract: a synthetic sequence of frame costs produces the
//! expected cadence and the expected p50/p95, and the animation phase is a
//! function of elapsed time rather than of how many frames were drawn.

use std::time::Duration;

use super::{Backpressure, DEGRADE_AFTER, MAX_CADENCE_MULTIPLIER, anim_phase};

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

#[test]
fn a_frame_cheaper_than_its_period_never_degrades_the_cadence() {
    // The case the thresholds are tuned for: 20 ms frames on the 66 ms tick.
    let mut bp = Backpressure::new(66);
    for _ in 0..500 {
        assert!(!bp.observe(ms(20)));
    }
    assert_eq!(bp.multiplier(), 1);
    assert_eq!(bp.period(), ms(66));

    // 30% of the budget is slack, not debt.
    let mut bp = Backpressure::new(66);
    for _ in 0..200 {
        bp.observe(ms(66));
    }
    assert_eq!(
        bp.multiplier(),
        1,
        "a frame exactly at budget is affordable"
    );
}

#[test]
fn five_sustained_over_budget_frames_double_the_period_and_recover_on_one_good_frame() {
    let mut bp = Backpressure::new(66);
    for _ in 0..(DEGRADE_AFTER - 1) {
        assert!(!bp.observe(ms(70)), "must not degrade before the streak");
    }
    assert_eq!(bp.multiplier(), 1);
    assert!(bp.observe(ms(70)), "the fifth slow frame degrades");
    assert_eq!(bp.multiplier(), 2);
    assert_eq!(bp.period(), ms(132));

    assert!(bp.observe(ms(10)), "one good frame recovers");
    assert_eq!(bp.multiplier(), 1);
    assert_eq!(bp.period(), ms(66));
}

#[test]
fn degradation_is_capped_at_nine() {
    let mut bp = Backpressure::new(66);
    let mut moves = 0;
    for _ in 0..(DEGRADE_AFTER * 50) {
        if bp.observe(ms(300)) {
            moves += 1;
        }
    }
    assert_eq!(bp.multiplier(), MAX_CADENCE_MULTIPLIER);
    assert_eq!(bp.period(), ms(66 * 9));
    // 1 -> 2 -> 4 -> 8 -> 9: four moves, then the ceiling holds.
    assert_eq!(moves, 4, "the ceiling must stop the ladder, not the loop");
}

#[test]
fn one_hick_frame_does_not_slow_the_client_down() {
    let mut bp = Backpressure::new(66);
    for _ in 0..(DEGRADE_AFTER - 1) {
        bp.observe(ms(5));
    }
    bp.observe(ms(400));
    for _ in 0..(DEGRADE_AFTER - 1) {
        bp.observe(ms(5));
    }
    assert_eq!(bp.multiplier(), 1, "a lone spike is not a regime");
}

#[test]
fn the_budget_is_the_period_so_a_cheap_nominal_is_not_punished() {
    // The same 20 ms frame is comfortably inside the 66 ms budget and far
    // inside the 240 ms one; a constant budget would have degraded `eco` for
    // work it can afford.
    let mut eco = Backpressure::new(240);
    for _ in 0..100 {
        eco.observe(ms(20));
    }
    assert_eq!(eco.budget(), ms(240));
    assert_eq!(eco.multiplier(), 1);
}

/// The requested synthetic-sequence test: N frames of a known cost give a known
/// p50/p95 through the same log2 histogram `pacode stats` prints, and the
/// cadence that came out is a function of the cost list alone.
///
/// Deliberately free of a real clock. A loop that measured itself with
/// `Instant` would make `count` and the drop count depend on how fast the test
/// machine happens to be — the assertions below would be about the CI box, not
/// about the scheduler. The drop rule is therefore fed as an explicit list of
/// "this slot was refused" booleans, which is what the draw gate actually
/// decides, and the costs are pushed unconditionally: a refused slot is a frame
/// that was *not* paid for, so it must not reach the histogram either.
#[test]
fn a_synthetic_frame_sequence_gives_the_expected_quantiles_and_cadence() {
    const CHEAP: usize = 190;
    const NEARLY: usize = 5;
    const SLOW: usize = 3;
    const SPIKE: usize = 1;
    const N: usize = CHEAP + NEARLY + SLOW + SPIKE;

    let mut costs = Vec::with_capacity(N);
    for _ in 0..CHEAP {
        costs.push(ms(4));
    }
    for _ in 0..NEARLY {
        costs.push(ms(5));
    }
    for _ in 0..SLOW {
        costs.push(ms(100));
    }
    costs.push(ms(400));

    // Every third slot is refused: the scheduler was asked three times per
    // 66 ms window and painted once. This is the shape the two-gate loop
    // produces, and it must not disturb the distribution or the cadence.
    let refused = |i: usize| i % 3 == 2;

    let mut bp = Backpressure::new(66);
    let mut dropped = 0usize;
    let mut painted_total: u64 = 0;
    for (i, cost) in costs.iter().enumerate() {
        if refused(i) {
            dropped += 1;
            continue;
        }
        painted_total += cost.as_micros() as u64;
        bp.observe(*cost);
    }

    // 199 slots, every third refused, so 133 of them were actually painted.
    // Quantiles are read off the painted frames only, and they are bucket tops
    // rather than interpolated values.
    let painted = (N - dropped) as u64;
    let hist = bp.snapshot();
    assert_eq!(hist.count, painted, "refused slots are not frames");
    // p50 of 133 samples is the 67th, and 127 of the painted frames are the
    // 4 ms and 5 ms ones: 4000 µs is log2 bucket 11 ([2^11, 2^12) µs =
    // [2.05 ms, 4.10 ms)), so both the median and the p95 read as 4096.
    assert_eq!(hist.quantile_micros(0.50), 4_096, "4 ms -> bucket 11");
    assert_eq!(
        hist.quantile_micros(0.95),
        4_096,
        "still inside the 4 ms mass"
    );
    // p99 is the 132nd sample — the first 100 ms frame. 100 000 µs is bucket 16
    // ([65.5 ms, 131 ms)), so the tail is one bucket, not the six the old
    // expectation assumed for a 26-bucket value.
    assert_eq!(hist.quantile_micros(0.99), 131_072, "100 ms -> bucket 16");
    assert_eq!(hist.max_micros, 400_000, "the worst frame stays visible");
    // And the total is the painted frames': a refused slot was never paid for,
    // so counting its cost in the mean would flatter the client.
    assert_eq!(hist.mean_micros(), painted_total / painted);
    // Degradation: the 100 ms frames are over the 66 ms budget, but the streak
    // is broken by the cheap ones, so the cadence never moved. Five slow frames
    // in a row would be needed, and the refusal pattern leaves two.
    assert_eq!(bp.multiplier(), 1, "an isolated slow frame is not a regime");
    assert_eq!(dropped, N / 3);
}

#[test]
fn a_degraded_run_opens_the_slots_the_multiplier_created() {
    // Ten frames of 300 ms on a 66 ms nominal. The multiplier only moves every
    // `DEGRADE_AFTER` frames, so ten frames are two streaks and reach x4 — the
    // ladder is walked at a fifth the rate a reader would guess from "each slow
    // frame doubles the period".
    let mut bp = Backpressure::new(66);
    for i in 0..10 {
        bp.observe(ms(300));
        if i == 4 {
            assert_eq!(bp.multiplier(), 2, "five 300 ms frames -> x2");
        }
    }
    assert_eq!(bp.multiplier(), 4, "1 -> 2 -> 4 over two streaks");
    assert_eq!(bp.period(), ms(66 * 4));
    // 264 ms is still under the 300 ms those frames cost, so the next streak
    // would take the cadence to x8; that a 300 ms frame is unaffordable even at
    // x4 is exactly the debt the ladder is paying down.
    assert!(ms(300) > bp.period());
    // And it recovers one step per affordable frame: 300 ms is under a 264 ms
    // budget? No — 10 ms is, and one good frame takes x4 to x2.
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 2);
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 1);
}

#[test]
fn a_long_degraded_run_walks_the_ladder_to_its_ceiling_and_back_down() {
    let mut bp = Backpressure::new(66);
    let mut moves = 0;
    for _ in 0..(DEGRADE_AFTER * 10) {
        if bp.observe(ms(300)) {
            moves += 1;
        }
    }
    // 1 -> 2 -> 4 -> 8 -> 9 over four streaks, then the ceiling holds even as
    // the streak keeps completing: 30 further frames of 300 ms move it not at
    // all, which is what a ceiling is for.
    assert_eq!(bp.multiplier(), MAX_CADENCE_MULTIPLIER);
    assert_eq!(moves, 4, "only the ladder moves the multiplier");
    for _ in 0..(DEGRADE_AFTER * 6) {
        assert!(!bp.observe(ms(300)), "the ceiling is not a starting point");
    }
    assert_eq!(bp.multiplier(), MAX_CADENCE_MULTIPLIER);
    assert_eq!(bp.period(), ms(66 * 9));
    // Recovery halves one step per affordable frame, and the budget is the
    // *nominal* period rather than the stretched one (see `budget`), so a 10 ms
    // frame is affordable at every rung: x9 -> x4 -> x2 -> x1 is three frames,
    // and the fourth finds nothing left to give.
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 4);
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 2);
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 1);
    assert!(!bp.observe(ms(10)), "x1 is the floor, not a step");
}

#[test]
fn a_new_nominal_keeps_the_multiplier_so_a_live_edit_is_not_re_measured_from_zero() {
    let mut bp = Backpressure::new(66);
    for _ in 0..(DEGRADE_AFTER * 2) {
        bp.observe(ms(300));
    }
    assert_eq!(bp.multiplier(), 4, "ten frames is two streaks");
    bp.set_nominal_ms(240);
    assert_eq!(bp.budget(), ms(240));
    assert_eq!(bp.multiplier(), 4, "still slow at the new rate");
    // And it recovers on affordable frames, as always: the new budget is 240 ms
    // and 10 ms is under it, so one step comes off per frame.
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 2);
    assert!(bp.observe(ms(10)));
    assert_eq!(bp.multiplier(), 1);
}

#[test]
fn the_animation_phase_is_elapsed_time_not_a_frame_count() {
    // 1.5 s of animation at 66 ms per step: 22 full steps, whatever the frame
    // rate was. A frame counter would say 1 if only one frame was drawn.
    assert_eq!(anim_phase(1500, 66), 22);
    // Skipping frames skips phases, which is the whole point.
    assert_eq!(anim_phase(0, 66), 0);
    assert_eq!(anim_phase(65, 66), 0);
    assert_eq!(anim_phase(66, 66), 1);
    assert_eq!(anim_phase(660, 66), 10);
    // Degenerate periods cannot divide by zero: a zero period freezes instead.
    assert_eq!(anim_phase(1000, 0), 0);
    // An elapsed time past the phase range must not wrap a phase that widgets
    // divide and index. It saturates at the largest value the quotient can take.
    assert_eq!(anim_phase(u64::MAX, u64::MAX), 1);
    assert_eq!(anim_phase(u64::MAX, 1), u64::MAX);
    // And a period of 1 ms over a year is 31 billion steps, which is a number a
    // spinner can index into; hours of elapsed time are not a problem here.
    assert_eq!(anim_phase(86_400_000, 1), 86_400_000);
}

mod phase_clock {
    use super::super::PhaseClock;

    #[test]
    fn with_a_steady_period_it_is_the_elapsed_time_phase() {
        let clock = PhaseClock::new(66);
        assert_eq!(clock.phase(0), 0);
        assert_eq!(clock.phase(1500), 22);
        // Pure: the same elapsed time is the same phase however often it is read.
        assert_eq!(clock.phase(1500), 22);
    }

    #[test]
    fn a_cadence_change_moves_the_cadence_not_the_spinner() {
        // The real case: a toast starts after five seconds of the 66 ms nominal
        // and the period drops to the 16 ms transient tick. Recomputing from the
        // epoch would put the spinner at 5_000 / 16 = 312 where it was showing
        // 75 — a 3.16-turn jump inside one frame.
        let mut clock = PhaseClock::new(66);
        let before = clock.phase(5_000);
        assert_eq!(before, 75);
        clock.reanchor(5_000, 16);
        assert_eq!(
            clock.phase(5_000),
            before,
            "the change is not a step: the glyph must not move on the frame it happens"
        );
        // And the new period now drives it, from where the old one left off.
        assert_eq!(clock.phase(5_000 + 160), before + 10);
        assert_eq!(clock.phase(5_000 + 159), before + 9);
    }

    #[test]
    fn returning_to_the_slow_period_does_not_jump_either() {
        let mut clock = PhaseClock::new(66);
        clock.reanchor(5_000, 16);
        let before = clock.phase(6_000);
        clock.reanchor(6_000, 66);
        assert_eq!(clock.phase(6_000), before);
        assert_eq!(clock.phase(6_000 + 660), before + 10);
    }

    #[test]
    fn dropped_frames_still_skip_phases_across_a_change() {
        // The property the epoch-relative phase gave and must survive: a client
        // that painted one frame in ten is not running in slow motion.
        //
        // Ten draws, 660 ms of wall time apart. The first five run on the 66 ms
        // nominal and the last five on the 16 ms transient tick. A client that
        // degraded the cadence x9 during the second stretch would be drawing
        // 144 ms apart and would have had to burn frames it does not have — the
        // phase is what the *clock* is at, not what this loop painted.
        let mut clock = PhaseClock::new(66);
        let mut phase = clock.phase(0);
        for elapsed in (660..=6_600).step_by(660) {
            // The switch is at 3_960 ms, the first sampled frame past 3.3 s, and
            // it lands on a frame the user is looking at — the case that must
            // not jump.
            let period = if elapsed > 3_300 { 16 } else { 66 };
            clock.reanchor(elapsed, period);
            phase = clock.phase(elapsed);
        }
        // The base is the 66 ms phase at the re-anchor (3_960 / 66 = 60), and the
        // five draws after it span 2_640 ms on a 16 ms clock: 165 steps. A
        // counter of drawn frames would say eleven for the whole run.
        assert_eq!(phase, 3_960 / 66 + (6_600 - 3_960) / 16);
    }

    #[test]
    fn reanchoring_to_the_same_period_is_a_no_op() {
        let mut clock = PhaseClock::new(66);
        clock.reanchor(5_000, 66);
        assert_eq!(clock.phase(5_000), 75);
        assert_eq!(clock.period_ms(), 66);
    }

    #[test]
    fn a_zero_period_does_not_divide_by_zero() {
        // A zero period freezes the phase instead of dividing by zero: a
        // misconfigured cadence should show a still spinner, not one that
        // advances once per millisecond.
        let mut clock = PhaseClock::new(0);
        assert_eq!(clock.phase(0), 0);
        assert_eq!(clock.phase(1_000), 0);
        assert_eq!(clock.phase(u64::MAX), 0);
        // And a change away from it is still a change.
        clock.reanchor(1_000, 66);
        assert_eq!(clock.period_ms(), 66);
        assert_eq!(clock.phase(1_066), 1);
    }

    #[test]
    fn saturation_does_not_wrap_the_base() {
        let mut clock = PhaseClock::new(1);
        clock.reanchor(u64::MAX, 1);
        // Already at the ceiling; the next step saturates rather than wrapping
        // a phase that widgets divide and index.
        assert_eq!(clock.phase(u64::MAX), u64::MAX);
    }
}

/// Ticker tests: the deadlines are a property of the arithmetic, so they run on
/// tokio's paused clock and are read by polling the interval rather than by
/// awaiting it.
///
/// Two things about that, both of which the obvious `timeout(.., ticker.tick())`
/// gets wrong. `#[tokio::test(start_paused = true)]` auto-advances the clock
/// every time the body is polled, so a timeout races the auto-advance against
/// the tick and loses on every boundary it was meant to reach. And `poll_tick`
/// answers `Pending` for a deadline that is still in the future — the *when* is
/// internal state, not something a future returns — so a test can only ask "is
/// it due yet", and it has to move the clock to ask. Polling by hand with a noop
/// waker removes the auto-advance from the picture: the only thing that moves
/// time is the test's own `advance`.
mod ticker {
    use std::task::{Context, Waker};
    use std::time::Duration;

    use super::super::Ticker;

    /// Whether `ticker`'s next tick is due at the current (paused) time.
    fn is_due(ticker: &mut Ticker) -> bool {
        let mut cx = Context::from_waker(Waker::noop());
        std::pin::Pin::new(&mut ticker.inner)
            .poll_tick(&mut cx)
            .is_ready()
    }

    /// Advance to `at` past `origin`, then arm a `period` ticker there — the
    /// late-arming shape the client actually has, since `anim_epoch` is taken at
    /// startup and the first animation may start a minute later.
    async fn armed_at(origin: tokio::time::Instant, at: Duration, period: Duration) -> Ticker {
        tokio::time::advance(at).await;
        Ticker::from(origin, tokio::time::Instant::now(), period)
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    /// Assert that no tick is due, and that nothing about awaiting can make one
    /// due. There is deliberately no `yield_now` in here: on a paused runtime
    /// that is what auto-advances the clock, so a helper written to "let the
    /// tick settle" would instead walk the clock onto the very boundary the test
    /// is asserting is still ahead.
    fn assert_pending(ticker: &mut Ticker) {
        assert!(!is_due(ticker), "a tick is not due before its boundary");
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_ticker_does_not_fire_at_once_even_when_the_origin_is_old() {
        // A ticker built on `origin + period` a minute after its origin would be
        // 60 periods overdue, and tokio fires an overdue interval on the first
        // tick — a frame painted the instant the spinner appears.
        let origin = tokio::time::Instant::now();
        let period = ms(66);
        let mut ticker = armed_at(origin, Duration::from_secs(60), period).await;
        assert_pending(&mut ticker);

        // The first tick is on the 66 ms grid measured from the origin, and the
        // clock is already 60 s into it: 60 s is 909 whole periods, so the next
        // boundary is 60 ms away — not 66, and not 60 s away. Re-anchoring on
        // `now + period` would land on 60.066 s and slip off the grid every
        // client shares.
        tokio::time::advance(ms(59)).await;
        assert_pending(&mut ticker);
        tokio::time::advance(ms(1)).await;
        assert!(is_due(&mut ticker), "the 60.06 s boundary is due");
    }

    #[tokio::test(start_paused = true)]
    async fn a_tick_does_not_repeat_without_the_clock_moving() {
        let origin = tokio::time::Instant::now();
        let mut ticker = Ticker::from(origin, origin, ms(66));
        tokio::time::advance(ms(66)).await;
        assert!(is_due(&mut ticker), "the boundary is due");
        assert!(!is_due(&mut ticker), "and it is spent once taken");
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_keep_the_origin_grid_even_when_the_ticker_late_arms() {
        let origin = tokio::time::Instant::now();
        let period = ms(100);
        // Arm at t = 250 ms: the boundaries ahead are 300, 400, 500. A ticker
        // re-anchored on `now + period` would first fire at 350, and every
        // client would drift apart by how long it was idle.
        let mut ticker = armed_at(origin, ms(250), period).await;
        assert_pending(&mut ticker);

        // Walk the clock in 50 ms steps to 550 and mark the boundaries the
        // ticker owes: 300, 400, 500. A re-anchored ticker would owe 350, 450,
        // 550 instead, and a ticker that fired immediately would owe 250.
        for step in 1..=6u64 {
            tokio::time::advance(ms(50)).await;
            let at = 250 + step * 50;
            let on_grid = (300..=500).contains(&at) && at % 100 == 0;
            assert_eq!(
                is_due(&mut ticker),
                on_grid,
                "at {at} ms a 100 ms grid from origin is {}due",
                if on_grid { "" } else { "not " }
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_period_change_keeps_the_phase_grid_of_the_new_period() {
        let origin = tokio::time::Instant::now();
        let mut ticker = Ticker::from(origin, origin, ms(100));

        // A transient animation ends: 100 ms -> 200 ms at t = 0. The next
        // boundary of the new grid is 200 ms, not 300 — `now + period` would put
        // it a whole period late, and the animation would visibly stutter.
        assert!(ticker.set_period(ms(200), origin.into()));
        assert_eq!(ticker.period(), ms(200));
        assert_pending(&mut ticker);
        tokio::time::advance(ms(199)).await;
        assert_pending(&mut ticker);
        tokio::time::advance(ms(1)).await;
        assert!(is_due(&mut ticker), "the new grid's boundary is at 200 ms");

        // And it keeps that grid rather than re-anchoring on the first tick.
        tokio::time::advance(ms(200)).await;
        assert!(is_due(&mut ticker), "the next boundary is at 400 ms");
    }

    #[tokio::test(start_paused = true)]
    async fn a_period_change_at_a_boundary_does_not_fire_in_the_past() {
        // 500 ms -> 60 ms at t = 500 ms, exactly on a boundary of the old grid.
        // The new grid's next boundary is 540 ms. Re-anchoring on the old
        // deadline gives 560 ms, and so does `now + period`: a whole period late,
        // and off the grid every client ticks on.
        let origin = tokio::time::Instant::now();
        let mut ticker = Ticker::from(origin, origin, ms(500));
        assert!(ticker.set_period(ms(60), origin.into()));
        assert_pending(&mut ticker);
        tokio::time::advance(ms(59)).await;
        assert_pending(&mut ticker);
        tokio::time::advance(ms(1)).await;
        assert!(is_due(&mut ticker), "540 ms, not 560 ms");
    }

    #[tokio::test(start_paused = true)]
    async fn a_period_change_past_a_boundary_lands_on_the_next_one() {
        // The same change armed at t = 530 ms: the 60 ms grid's next boundary is
        // 540 ms. A ticker that put the deadline at the first boundary at-or-
        // after now would answer 540 ms here and 560 ms in the test above — a
        // phase that depends on exactly when the change landed, which is what
        // "keeps the grid" has to rule out.
        let origin = tokio::time::Instant::now();
        let mut ticker = armed_at(origin, ms(530), ms(500)).await;
        assert!(ticker.set_period(ms(60), origin.into()));
        // Armed at 530 ms on a 60 ms grid, so the next boundary is 540 ms: ten
        // milliseconds out, not a whole period.
        assert_pending(&mut ticker);
        tokio::time::advance(ms(9)).await;
        assert_pending(&mut ticker);
        tokio::time::advance(ms(1)).await;
        assert!(is_due(&mut ticker), "540 ms");
    }

    #[tokio::test(start_paused = true)]
    async fn setting_the_same_period_is_a_no_op() {
        let origin = tokio::time::Instant::now();
        let mut ticker = Ticker::from(origin, origin, ms(66));
        assert!(!ticker.set_period(ms(66), origin.into()));
        // And the deadline is untouched: re-arming on an unchanged period would
        // push every tick a period late each time a caller asked for the period
        // it already had.
        tokio::time::advance(ms(65)).await;
        assert_pending(&mut ticker);
        tokio::time::advance(ms(1)).await;
        assert!(is_due(&mut ticker), "the boundary is still at 66 ms");
    }
}
