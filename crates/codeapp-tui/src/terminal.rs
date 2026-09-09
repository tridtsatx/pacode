//! Terminal setup/teardown: raw mode, alt screen, bracketed paste, mouse capture
//! (`ui.mouse`), keyboard enhancement flags when supported (so `shift+enter` and
//! `alt+arrows` arrive), a panic hook that restores the terminal.

use std::io::Stdout;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub struct TerminalGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
}

impl TerminalGuard {
    pub fn enter(mouse: bool) -> std::io::Result<Self> {
        let _ = mouse;
        todo!("TerminalGuard::enter")
    }

    pub fn mouse_enabled(&self) -> bool {
        self.mouse
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        todo!("TerminalGuard::drop")
    }
}

/// Restore the terminal from a panic hook (best effort, never panics).
pub fn restore_on_panic() {
    todo!("terminal::restore_on_panic")
}
