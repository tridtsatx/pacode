//! Key and mouse handling (spec §6, §7 and the mockup).
//!
//! Bindings (all keep the prompt focused):
//! - `alt+↓` / `ctrl+j`: select first agent / next (incl. `and N more`); in BgList: next
//! - `alt+↑` / `ctrl+k`: previous
//! - `enter`: submit prompt when text present; in SelectAgent/BgList with an empty
//!   prompt: open the panel
//! - `alt+b`: follow the selected/open agent
//! - `.` in an empty prompt: BgList overlay; otherwise a literal `.`
//! - `esc`: follow → panel → selection → overlay, one layer per press
//! - `s` in an empty prompt while a panel shows a live agent: `StopAgent`
//! - `k` in an empty prompt while BgList/panel shows a running task: `KillTask`
//! - `shift+tab`: `SetMode(mode.next())`
//! - `ctrl+p`: session picker; `ctrl+c`: interrupt (twice within 2 s: quit)
//! - `pgup`/`pgdn`/`home`/`end`: scroll dialog (or panel when open; `pgup` pauses
//!   follow, `end` resumes it)
//! - `shift+enter` / `alt+enter`: newline in the prompt; `↑`/`↓` with an empty prompt:
//!   prompt history; `ctrl+w`: delete word; `ctrl+u`: clear line
//! - mouse wheel: scroll the zone under the cursor (dialog / panel / rail agents)
//!
//! Every binding produces zero or more [`Action`]s that `app` executes (requests to the
//! daemon) after mutating the state.

use codeapp_types::Request;
use crossterm::event::{KeyEvent, MouseEvent};

use crate::state::AppState;

/// Side effects requested by a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(Request),
    /// Ask the daemon for the panel's content (agent transcript or task output).
    LoadPanel,
    /// Ask for an older history page for the dialog.
    LoadHistory,
    Quit,
}

pub fn handle_key(state: &mut AppState, key: KeyEvent, now: std::time::Instant) -> Vec<Action> {
    let _ = (state, key, now);
    todo!("keys::handle_key")
}

pub fn handle_mouse(
    state: &mut AppState,
    mouse: MouseEvent,
    layout: &crate::layout::ScreenLayout,
) -> Vec<Action> {
    let _ = (state, mouse, layout);
    todo!("keys::handle_mouse")
}
