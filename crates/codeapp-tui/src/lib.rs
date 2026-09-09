//! The terminal client (TUI spec `docs/superpowers/specs/2026-09-09-tui-layout-design.md`
//! and mockup). Alt-screen, own scrollback, persistent rail.
//!
//! Module map:
//! - `app`: event loop — client events, terminal input, ticks; redraw scheduling
//! - `state`: all mutable UI state ([`state::AppState`]) and how events mutate it
//! - `layout`: rects per width tier (spec §8)
//! - `ui`: widgets — rail, dialog, panel, input, footer, overlays, toasts
//! - `keys`: key bindings and the interaction mode machine (spec §5–§7)
//! - `commands`: slash commands
//! - `terminal`: raw mode / alt screen / mouse capture / panic hook

pub mod app;
pub mod commands;
pub mod keys;
pub mod keys_picker;
pub mod layout;
pub mod state;
pub mod terminal;
pub mod ui;

use std::path::PathBuf;

use codeapp_client::ClientOptions;
use codeapp_types::{Attach, Config};

pub use app::run;

/// What `codeapp` (no subcommand) passes to the TUI.
#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub client: ClientOptions,
    pub config: Config,
    pub attach: Attach,
    /// Sent as the first user message when given on the command line.
    pub initial_prompt: Option<String>,
    pub cwd: PathBuf,
    pub app_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("client: {0}")]
    Client(#[from] codeapp_client::ClientError),
    #[error("terminal: {0}")]
    Io(#[from] std::io::Error),
}
