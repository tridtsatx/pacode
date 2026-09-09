//! Graphics protocol detection from terminal environment variables.

use std::sync::OnceLock;

#[cfg(test)]
#[path = "detect_tests.rs"]
mod detect_tests;

/// Graphics protocols supported by modern terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsProtocol {
    /// Kitty graphics protocol (Kitty, Ghostty, WezTerm).
    Kitty,
    /// iTerm2 inline image protocol (iTerm.app).
    Iterm2,
    /// Sixel graphics protocol. Note: falls back to HalfBlocks in detect/render.
    Sixel,
    /// Fallback half-block UTF-8 characters (`▀`).
    HalfBlocks,
}

static DETECTED: OnceLock<GraphicsProtocol> = OnceLock::new();

/// Detect once, from the environment only — no terminal queries, no timers.
pub fn detect() -> GraphicsProtocol {
    *DETECTED.get_or_init(|| detect_from(&|k| std::env::var(k).ok()))
}

/// Pure detection function testable against any environment variable resolver.
///
/// Detection rules and rationale:
/// 1. Kitty:
///    - `TERM=xterm-kitty` or `KITTY_WINDOW_ID` is set: native Kitty terminal.
///    - `TERM_PROGRAM=ghostty` or `WezTerm`: modern terminal emulators that advertise
///      themselves via `TERM_PROGRAM` and implement the Kitty graphics protocol.
/// 2. Iterm2:
///    - `TERM_PROGRAM=iTerm.app`: macOS iTerm2 sets `TERM_PROGRAM=iTerm.app` and supports
///      the OSC 1337 inline file display protocol.
/// 3. Sixel:
///    - `TERM` contains `foot` or `mlterm`: these lightweight Linux/BSD terminals natively
///      support Sixel graphics.
///    - `TERM` contains `xterm-256color` and `COLORTERM=sixel`: certain xterm builds advertise
///      sixel capability via this pair of variables.
///    - Fallback note: pacode-image does not hand-roll a sixel encoder. As specified,
///      terminals matching Sixel conditions fall back to `HalfBlocks` in the returned
///      protocol so the caller never claims sixel support it does not have.
/// 4. Otherwise:
///    - `HalfBlocks`: Unicode half-block characters (`▀`), works on any modern terminal.
pub fn detect_from(env: &dyn Fn(&str) -> Option<String>) -> GraphicsProtocol {
    // 1. Kitty: native kitty terminal or terminals supporting Kitty graphics protocol
    if env("TERM").as_deref() == Some("xterm-kitty")
        || env("KITTY_WINDOW_ID").is_some_and(|v| !v.is_empty())
        || matches!(
            env("TERM_PROGRAM").as_deref(),
            Some("ghostty") | Some("WezTerm")
        )
    {
        return GraphicsProtocol::Kitty;
    }

    // 2. iTerm2: macOS iTerm.app native inline images
    if env("TERM_PROGRAM").as_deref() == Some("iTerm.app") {
        return GraphicsProtocol::Iterm2;
    }

    // 3. Sixel: recognized from foot, mlterm, or xterm-256color + COLORTERM=sixel.
    // Falls back to HalfBlocks so the caller never claims sixel support it does not have.
    if is_sixel_env(env) {
        return GraphicsProtocol::HalfBlocks;
    }

    // 4. Fallback: half-blocks
    GraphicsProtocol::HalfBlocks
}

/// Check if the environment matches Sixel terminal conditions.
pub fn is_sixel_env(env: &dyn Fn(&str) -> Option<String>) -> bool {
    let term = env("TERM").unwrap_or_default();
    let colorterm = env("COLORTERM").unwrap_or_default();
    term.contains("foot")
        || term.contains("mlterm")
        || (term.contains("xterm-256color") && colorterm == "sixel")
}
