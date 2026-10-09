//! The timers the event loop selects over besides its two clocks. Each one is
//! armed only while it has work and otherwise pends forever, so nothing ticks
//! in the idle state.

use std::time::{Duration, Instant};

use crate::state::{AppState, TOAST_EXIT_MS, TOAST_TTL_SECS};

/// The paced stream tick, armed only while a transcript has backlog.
pub(super) async fn stream(needs_stream: bool, stream_tick_dur: Duration) {
    if needs_stream {
        tokio::time::sleep(stream_tick_dur).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Armed to the next boundary of the displayed clock, not to a full
/// second from now. Every other future in this `select!` cancels and
/// re-creates this one; measuring from `now` meant a plain 1 s sleep was
/// restarted by each animation frame and never fired, so the duration
/// only moved on unrelated redraws and read 0.1 -> 0.3 -> 0.5 instead of
/// ticking. The boundary is re-derived from the reading every iteration
/// rather than held in a ticker, because the deadline is a wall-clock
/// boundary: there is no phase for a ticker to keep.
pub(super) async fn second(needs_second: bool, state: &AppState) {
    if needs_second {
        let wait = Duration::from_millis(crate::state::activity::ms_to_next_second(
            state.phase_elapsed_ms,
        ));
        tokio::time::sleep(wait).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// A SCHEDULE countdown on the rail stepping to its next reading: one
/// deadline while a countdown is on screen, nothing otherwise.
pub(super) async fn schedule(state: &AppState) {
    match state.schedule_redraw_in(pacode_types::time::now_ms()) {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending::<()>().await,
    }
}

/// The `ctrl+c` confirmation window closing.
pub(super) async fn ctrl_c_expiry(state: &AppState) {
    if let Some(t) = state.ctrl_c_at {
        let dur = Duration::from_secs(2);
        let elapsed = Instant::now().saturating_duration_since(t);
        if elapsed < dur {
            tokio::time::sleep(dur - elapsed).await;
        }
    } else {
        std::future::pending::<()>().await;
    }
}

/// Wake at the nearest toast boundary: the fade start (TTL − EXIT)
/// arms the anim tick so the fade actually gets frames while the
/// app is idle; the expiry (TTL) drops the toast. Past the fade
/// start only the expiry remains — never a zero-duration spin.
pub(super) async fn toast_expiry(state: &AppState) {
    let now = Instant::now();
    let fade_at =
        Duration::from_secs(TOAST_TTL_SECS).saturating_sub(Duration::from_millis(TOAST_EXIT_MS));
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
}

/// A pending frame wakes the loop the moment its own gate opens
/// (`Painter::next_draw_wait`).
pub(super) async fn deferred_draw(wait: Option<Duration>) {
    match wait {
        Some(remaining) => tokio::time::sleep(remaining).await,
        None => std::future::pending::<()>().await,
    }
}

/// The remote-filter debounce of the open menu (§3.2): armed only
/// while a level owes the daemon a query — no timer runs otherwise.
pub(super) async fn menu_remote(state: &AppState) {
    match crate::keys_menu::remote_deadline(state) {
        Some(at) => {
            let remaining = at.saturating_duration_since(Instant::now());
            tokio::time::sleep(remaining).await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// A seq-gap resync that lost its send slot to the debounce — or a
/// failed send that re-armed the want — must not wait on the next
/// daemon event: a quiet stream would strand the heal and leave the
/// rail stale. Armed only while a want waits on the send gate (the
/// in-flight case wakes via `BgResponse::ResyncDone`), firing at the
/// debounce boundary, at most `RESYNC_DEBOUNCE_MS` out.
pub(super) async fn resync(state: &AppState) {
    if !state.resync_want || state.resync_in_flight || state.awaiting_snapshot {
        return std::future::pending::<()>().await;
    }
    let wait = match state.resync_sent_at {
        Some(t) => Duration::from_millis(crate::state::RESYNC_DEBOUNCE_MS)
            .saturating_sub(Instant::now().saturating_duration_since(t)),
        None => Duration::ZERO,
    };
    tokio::time::sleep(wait).await;
}
