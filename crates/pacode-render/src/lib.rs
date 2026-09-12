//! Text rendering for the TUI: markdown → ratatui lines, grapheme-aware wrapping,
//! diff rendering, glyph/theme tables with ASCII fallback, paced stream buffer and
//! the bounded line cache. No terminal IO here.

pub mod cache;
pub mod diff;
pub mod markdown;
pub mod palettes;
pub mod stream;
pub mod syntax;
pub mod theme;
pub mod wrap;

pub use cache::LineCache;
pub use diff::{diff_stat, render_diff, render_diff_ex, unified_diff};
pub use markdown::render_markdown;
pub use palettes::builtin_palettes;
pub use stream::{StreamBuffer, StreamKind, StreamOp};
pub use syntax::{Lang, Lexer, Token, TokenKind, detect_lang, token_style};
pub use theme::{
    BashRole, ColorMode, Glyphs, Palette, Theme, detect_truecolor, parse_color, rgb_to_ansi16,
};
pub use wrap::{display_width, truncate_spans, truncate_to_width, wrap_line, wrap_text};

/// Rendering parameters shared by all renderers.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderOptions {
    pub width: u16,
    pub theme: Theme,
    pub glyphs: Glyphs,
    /// Terminal colour fidelity (`[ui] color` / `COLORTERM` / `NO_COLOR`).
    /// Controls degradation steps such as diff background bands.
    pub color: ColorMode,
    /// Draw the model's reasoning line (`[ui] thinking`). Off by default.
    pub thinking: bool,
}

impl RenderOptions {
    pub fn new(width: u16, ascii_only: bool) -> Self {
        Self {
            width,
            thinking: false,
            color: ColorMode::detect(),
            theme: Theme::default(),
            glyphs: Glyphs::new(ascii_only),
        }
    }

    pub fn with_theme(width: u16, ascii_only: bool, theme: Theme) -> Self {
        Self {
            width,
            theme,
            glyphs: Glyphs::new(ascii_only),
            color: ColorMode::detect(),
            thinking: false,
        }
    }

    /// Force a colour mode (the `ui.color` setting flows in through
    /// `PACODE_COLOR`; tests set it explicitly).
    pub fn color_mode(mut self, color: ColorMode) -> Self {
        self.color = color;
        self
    }

    /// Draw the model's reasoning line, per `[ui] thinking`.
    pub fn thinking(mut self, thinking: bool) -> Self {
        self.thinking = thinking;
        self
    }
}
