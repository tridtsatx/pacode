//! Terminal setup/teardown: raw mode, alt screen, bracketed paste, mouse capture
//! (`ui.mouse`), keyboard enhancement flags when supported (so `shift+enter` and
//! `alt+arrows` arrive), a panic hook that restores the terminal.

use std::io::{Stdout, Write};

use crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
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

    /// Apply font configuration using the terminal backend writer.
    pub fn apply_font(&mut self, cfg: &pacode_types::FontConfig) -> crate::font::FontOutcome {
        let control = crate::font::detect();
        crate::font::apply_with_writer(cfg, control, self.terminal.backend_mut())
    }

    /// Emit an image escape sequence starting at the specified terminal cell position.
    pub fn draw_image_escape(
        &mut self,
        area: ratatui::layout::Rect,
        escape: &str,
    ) -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        execute!(stdout, SavePosition, MoveTo(area.x, area.y))?;
        stdout.write_all(escape.as_bytes())?;
        execute!(stdout, RestorePosition)?;
        stdout.flush()?;
        Ok(())
    }

    /// Clear an image preview area by sending kitty delete command and overwriting cells with spaces.
    pub fn clear_image_area(&mut self, area: ratatui::layout::Rect) -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        execute!(stdout, SavePosition)?;
        let _ = stdout.write_all(b"\x1b_Ga=d,d=a\x1b\\");
        let spaces = " ".repeat(area.width as usize);
        for row in 0..area.height {
            let _ = execute!(stdout, MoveTo(area.x, area.y + row));
            let _ = stdout.write_all(spaces.as_bytes());
        }
        execute!(stdout, RestorePosition)?;
        stdout.flush()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b_Ga=d,d=a\x1b\\");
        let _ = stdout.flush();
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
    let _ = stdout.write_all(b"\x1b_Ga=d,d=a\x1b\\");
    let _ = stdout.flush();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Suspend the TUI: leave alternate screen and raw mode so child processes (e.g. editor) can run.
pub fn suspend(mouse: bool) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b_Ga=d,d=a\x1b\\");
    let _ = stdout.flush();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    if mouse {
        let _ = execute!(stdout, DisableMouseCapture);
    }
    execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Resume the TUI: re-enter raw mode and alternate screen after child process finishes.
pub fn resume(mouse: bool) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        let _ = execute!(stdout, EnableMouseCapture);
    }
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS;
    #[cfg(unix)]
    let push = true;
    #[cfg(not(unix))]
    let push = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if push {
        let _ = execute!(stdout, PushKeyboardEnhancementFlags(flags));
    }
    Ok(())
}

struct SuspendGuard {
    mouse: bool,
    active: bool,
}

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = resume(self.mouse);
        }
    }
}

/// Run a closure with the TUI suspended, restoring the terminal state afterwards.
pub fn run_suspended<F, R>(mouse: bool, f: F) -> std::io::Result<R>
where
    F: FnOnce() -> R,
{
    suspend(mouse)?;
    let mut guard = SuspendGuard {
        mouse,
        active: true,
    };
    let result = f();
    guard.active = false;
    resume(mouse)?;
    Ok(result)
}
