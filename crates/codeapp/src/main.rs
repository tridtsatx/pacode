//! codeapp binary.
//!
//! ```text
//! codeapp [PROMPT] [--resume ID] [--model M] [--effort E] [--mode M] [-C DIR] [--socket P]
//! codeapp serve [--detach] [--socket P]
//! codeapp run PROMPT [--json] [--model M] [--effort E] [--mode M] [-C DIR]
//! codeapp sessions [list [--limit N] | delete ID]
//! codeapp daemon [status | stop [--force]]
//! ```
//!
//! - default: load config, `TuiOptions` with `Attach::New{cwd,..}` or `Resume`, run the TUI
//!   on a `current_thread` runtime.
//! - `serve`: `--detach` re-executes itself with `setsid` (via `codeapp_client::spawn_daemon`)
//!   and exits; otherwise builds the core and runs the daemon on a multi-thread runtime
//!   with 2 workers; logs to `paths.daemon_log()` (level from `CODEAPP_LOG`).
//! - `run`: headless — connect, attach New, send the prompt, print assistant text to
//!   stdout as it streams (or every event as NDJSON with `--json`), exit after
//!   `TurnEnded`; permission requests are auto-denied unless `--mode bypass|auto`.
//! - `sessions`, `daemon`: thin wrappers over the client.

mod cli;

fn main() {
    if let Err(err) = cli::main() {
        eprintln!("codeapp: {err:#}");
        std::process::exit(1);
    }
}
