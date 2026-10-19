//! Syntax highlighting for diffs, fenced markdown blocks and file previews.
//!
//! One lexer (`Lexer`) serves all callers. It works line by line and keeps a
//! single piece of state between lines — whether an unterminated block
//! comment / multiline string / YAML scalar is still open — so a diff hunk or
//! a file fragment highlights without the rest of the file. Seven token kinds
//! map onto the existing `Theme` colours; unknown languages are `None`, never
//! an error.

mod code;
mod lang;
mod lexer;
mod misc;

use ratatui::style::Style;

use crate::theme::Theme;

pub use lang::{Lang, detect_lang};
pub use lexer::{Lexer, MAX_LINE_CHARS, Token, TokenKind};

/// The theme colour for a token kind. All seven map to existing fields —
/// palettes need no new roles.
pub fn token_style(theme: &Theme, kind: TokenKind) -> Style {
    match kind {
        TokenKind::Keyword => theme.violet,
        TokenKind::Function => theme.cyan,
        TokenKind::Type => theme.yellow,
        TokenKind::String => theme.green,
        TokenKind::Number => theme.accent,
        TokenKind::Comment => theme.faint,
        TokenKind::Operator => theme.dim,
    }
}
