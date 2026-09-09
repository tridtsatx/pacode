//! Terminal setup/teardown: raw mode, alt screen, bracketed paste, mouse capture
//! (`ui.mouse`), keyboard enhancement flags when supported (so `shift+enter` and
//! `alt+arrows` arrive), a panic hook that restores the terminal.

use std::io::Stdout;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
#[cfg(not(unix))]
use crossterm::terminal::supports_keyboard_enhancement;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub struct TerminalGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
}

impl TerminalGuard {
    pub fn enter(mouse: bool) -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;

        if mouse {
            let _ = execute!(stdout, EnableMouseCapture);
        }

        // `supports_keyboard_enhancement()` blocks up to 2 s waiting for a terminal
        // reply, which is the whole first-frame budget. Terminals that do not
        // support the kitty protocol ignore the push sequence, so on unix we push
        // unconditionally (same choice as codex).
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;
        #[cfg(unix)]
        let push = true;
        #[cfg(not(unix))]
        let push = supports_keyboard_enhancement().unwrap_or(false);
        if push {
            let _ = execute!(stdout, PushKeyboardEnhancementFlags(flags));
        }

        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_on_panic();
            prev_hook(info);
        }));

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self { terminal, mouse })
    }

    pub fn mouse_enabled(&self) -> bool {
        self.mouse
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        if self.mouse {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

/// Restore the terminal from a panic hook (best effort, never panics).
pub fn restore_on_panic() {
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
