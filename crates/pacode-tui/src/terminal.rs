//! Terminal setup/teardown: raw mode, alt screen, bracketed paste, mouse capture
//! (`ui.mouse`), keyboard enhancement flags when supported (so `shift+enter` and
//! `alt+arrows` arrive), a panic hook that restores the terminal. Frames are
//! bracketed in DEC 2026 synchronized output (`begin_sync`/`end_sync`) so the
//! terminal presents a finished frame instead of a half-painted one.

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

/// DEC 2026 synchronized-output begin: the terminal buffers output and
/// presents it atomically when the matching end arrives (the Devin CLI emits
/// the same pair, which is why its frames never tear). Terminals that do not
/// implement the mode ignore the private sequence, so no probe is needed.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// DEC 2026 synchronized-output end. Written defensively on every teardown
/// path — dying between begin and end would otherwise leave the terminal
/// frozen in synchronized mode with later output never presented.
const SYNC_END: &[u8] = b"\x1b[?2026l";

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
        // REPORT_EVENT_TYPES + REPORT_ALL_KEYS_AS_ESCAPE_CODES: Press/Repeat/
        // Release arrive distinctly and a bare Shift press is an event — the
        // double-Shift menu gesture needs both (§4.2). `keys::handle_key`
        // drops Release events; Repeat runs the same action as Press so a
        // held key auto-repeats (auto-scroll, held-char typing).
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
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

    /// Open a DEC 2026 synchronized update around the next `terminal.draw`.
    /// Best effort: write errors are ignored exactly like `clear_image_area`
    /// treats its escape — the worst outcome is a torn frame, not a panic.
    /// Written through `std::io::stdout()` — the same shared handle the
    /// crossterm backend writes frames to — so the ordering against the
    /// backend's own buffered output is deterministic.
    pub fn begin_sync(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(SYNC_BEGIN);
        let _ = stdout.flush();
    }

    /// Close the synchronized update opened by [`Self::begin_sync`], then
    /// flush so the frame and the end marker go out together.
    pub fn end_sync(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(SYNC_END);
        let _ = stdout.flush();
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

    /// Clear input-strip image escapes. Kitty gets an explicit delete;
    /// iterm2/sixel have no delete command, so the caller must follow with
    /// `terminal.clear()` before the next draw — that repaints the text cells
    /// the images covered. Never overwrite those cells with spaces here:
    /// direct tty writes the buffer knows nothing about corrupt ratatui's
    /// diff and leave holes (e.g. a truncated separator line).
    pub fn clear_image_escapes(&mut self) -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b_Ga=d,d=a\x1b\\");
        stdout.flush()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        // SYNC_END first: if drop raced an open synchronized update, later
        // teardown output must not stay buffered in the frozen mode.
        let _ = stdout.write_all(SYNC_END);
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
    // SYNC_END first: a panic between begin_sync and end_sync must not leave
    // the terminal frozen in synchronized mode.
    let _ = stdout.write_all(SYNC_END);
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
    // SYNC_END first: suspend may run while a synchronized update is open,
    // and the child's output must reach the screen.
    let _ = stdout.write_all(SYNC_END);
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
    // Same flag set as `TerminalGuard::new` — keep them in lockstep.
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
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
