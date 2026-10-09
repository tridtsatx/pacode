//! Text rendering for the TUI: markdown → ratatui lines, grapheme-aware wrapping,
//! diff rendering, glyph/theme tables with ASCII fallback, paced stream buffer and
//! the bounded line cache. No terminal IO here.

pub mod cache;
pub mod diff;
pub mod markdown;
pub mod palettes;
pub mod shimmer;
pub mod stream;
pub mod syntax;
pub mod text;
pub mod theme;
pub mod wrap;

use pacode_types::config::ShimmerMode;

pub use cache::LineCache;
pub use diff::{DIFF_TIMEOUT, diff_hides_lines, render_diff, render_diff_ex, unified_diff};
pub use markdown::render_markdown;
pub use palettes::builtin_palettes;
pub use shimmer::{AnimClock, ShimmerPalette, shimmer_spans, shimmer_tiers};
pub use stream::{StreamBuffer, StreamKind, StreamOp};
pub use syntax::{Lang, Lexer, Token, TokenKind, detect_lang, token_style};
pub use theme::{
    BashRole, ColorMode, Glyphs, Palette, Theme, detect_truecolor, parse_color, rgb_to_ansi16,
};
pub use wrap::{display_width, truncate_spans, truncate_to_width, wrap_line, wrap_text};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderCacheKey {
    pub width: u16,
    pub context: u64,
}

impl RenderCacheKey {
    pub fn new(width: u16, context: u64) -> Self {
        Self { width, context }
    }
}

/// Rendering parameters shared by all renderers.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderOptions {
    pub width: u16,
    pub theme: Theme,
    pub glyphs: Glyphs,
    pub color: ColorMode,
    pub thinking: bool,
    pub expand_hint: String,
    pub cache_epoch: u64,
    /// Which sweep, if any, is drawn across live text.
    pub shimmer: ShimmerMode,
    /// The animation clock a sweep is drawn at. Deliberately *not* part of the
    /// render context that `cache_epoch` digests: a sweep only ever lands on a
    /// row the client redraws on every tick, so a moving clock must not
    /// invalidate the line cache once per frame.
    pub anim: AnimClock,
}

impl RenderOptions {
    pub fn new(width: u16, ascii_only: bool) -> Self {
        Self::with_theme(width, ascii_only, Theme::default())
    }

    pub fn with_theme(width: u16, ascii_only: bool, theme: Theme) -> Self {
        Self::with_theme_color(width, ascii_only, theme, ColorMode::detect())
    }

    pub fn with_theme_color(width: u16, ascii_only: bool, theme: Theme, color: ColorMode) -> Self {
        let theme = match color {
            ColorMode::Mono => Theme::mono(),
            _ => theme,
        };
        Self {
            width,
            theme,
            glyphs: Glyphs::new(ascii_only),
            color,
            thinking: false,
            expand_hint: "ctrl+o".to_string(),
            cache_epoch: 0,
            shimmer: ShimmerMode::default(),
            anim: AnimClock::default(),
        }
    }

    pub fn color_mode(mut self, color: ColorMode) -> Self {
        self.color = color;
        self
    }

    pub fn thinking(mut self, thinking: bool) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn expand_hint(mut self, hint: impl Into<String>) -> Self {
        self.expand_hint = hint.into();
        self
    }

    pub fn cache_epoch(mut self, epoch: u64) -> Self {
        self.cache_epoch = epoch;
        self
    }

    pub fn shimmer(mut self, mode: ShimmerMode) -> Self {
        self.shimmer = mode;
        self
    }

    pub fn anim(mut self, anim: AnimClock) -> Self {
        self.anim = anim;
        self
    }

    pub fn cache_key(&self) -> RenderCacheKey {
        RenderCacheKey::new(self.width, self.cache_epoch)
    }
}
