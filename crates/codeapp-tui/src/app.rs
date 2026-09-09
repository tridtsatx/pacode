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

use crate::{TuiError, TuiOptions};

pub async fn run(opts: TuiOptions) -> Result<(), TuiError> {
    let _ = opts;
    todo!("app::run")
}
