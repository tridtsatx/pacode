//! The lock's indicator: one reusable piece the footer, the header or a debug
//! view renders wherever it fits.
//!
//! Where it belongs is a layout call, not a policy one, so this module only
//! decides *what* the badge says and how it is styled. The footer's right side
//! is the right home: it is the one piece of chrome on screen in every focus
//! and at every width, and a held notice matters most exactly when the user is
//! reading the transcript or typing a prompt — footer territory. The rail
//! disappears below 80 columns and the header belongs to the session banner,
//! neither of which is a place to lose a count. The badge is a `Spans` list
//! rather than a widget so a debug view can print the same words.

use pacode_render::Theme;
use ratatui::style::Style;
use ratatui::text::Span;

/// What the lock is doing right now, as far as the reader is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockBadge {
    /// Locked, with `held` notices parked. `dropped` is zero unless a cap or a
    /// session switch threw notices away.
    Holding { held: usize, dropped: u64 },
    /// Unlocked, with `left` notices still trickling out.
    Draining { left: usize },
}

impl LockBadge {
    /// `LOCK 3` while holding, `… 3` while draining. The glyph table has no
    /// padlock and the word survives an ASCII terminal, a colourblind reader
    /// and a screen reader alike — colour is never the only carrier here.
    pub fn label(self) -> String {
        match self {
            Self::Holding { held, .. } => format!("LOCK {held}"),
            Self::Draining { left } => format!("… {left}"),
        }
    }

    pub fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Holding { .. } => theme.yellow,
            Self::Draining { .. } => theme.cyan,
        }
    }

    /// The badge, with the drop count appended in faint when notices were
    /// lost — a queue that quietly ate something must say so.
    pub fn spans(self, theme: &Theme) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(self.label(), self.style(theme))];
        if let Self::Holding { dropped, .. } = self
            && dropped > 0
        {
            spans.push(Span::styled(format!(" (+{dropped} dropped)"), theme.faint));
        }
        spans
    }
}
