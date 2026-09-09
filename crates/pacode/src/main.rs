//! pacode binary.
//!
//! ```text
//! pacode [PROMPT] [--resume ID] [--model M] [--effort E] [--mode M] [-C DIR] [--socket P]
//! pacode serve [--detach] [--socket P]
//! pacode run PROMPT [--json] [--model M] [--effort E] [--mode M] [-C DIR]
//! pacode sessions [list [--limit N] | delete ID]
//! pacode daemon [status | stop [--force]]
//! ```
//!
//! - default: load config, `TuiOptions` with `Attach::New{cwd,..}` or `Resume`, run the TUI
//!   on a `current_thread` runtime.
//! - `serve`: `--detach` re-executes itself with `setsid` (via `pacode_client::spawn_daemon`)
//!   and exits; otherwise builds the core and runs the daemon on a multi-thread runtime
//!   with 2 workers; logs to `paths.daemon_log()` (level from `PACODE_LOG`).
//! - `run`: headless — connect, attach New, send the prompt, print assistant text to
//!   stdout as it streams (or every event as NDJSON with `--json`), exit after
//!   `TurnEnded`; permission requests are auto-denied unless `--mode bypass|auto`.
//! - `sessions`, `daemon`: thin wrappers over the client.

mod cli;

fn main() {
    // Let `pacode sessions list | head` end quietly instead of panicking on EPIPE.
    #[cfg(unix)]
    // SAFETY: resetting the SIGPIPE disposition before any thread exists is sound.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(err) = cli::main() {
        eprintln!("pacode: {err:#}");
        std::process::exit(1);
    }
}
