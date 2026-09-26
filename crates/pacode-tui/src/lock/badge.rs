//! The lock's indicator: one reusable piece the footer, the header or a debug
//! view renders wherever it fits.
//!
//! Where it belongs is a layout call, not a policy one, so this module only
//! decides *what* the badge says and how it is styled. The footer's right side
//! is the right home: it is the one piece of chrome on screen in every focus
//! and at every width, and held notices matter most exactly when the user is
//! reading the transcript or typing a prompt — footer territory. The rail
//! disappears below 80 columns and the header belongs to the session banner,
//! neither of which is a place to lose a count. The badge is a `Spans` list
//! rather than a widget so a debug view can print the same words.

use pacode_render::Theme;
use ratatui::style::Style;
use ratatui::text::Span;

/// What the lock is doing right now, as far as the reader is concerned. Plain
/// data, fed by `Event::NoticeLockChanged` — the daemon owns the policy, this
/// only describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockBadge {
    /// Whether the switch is on. `false` with a non-zero `held` means the
    /// backlog is draining, which reads differently from holding.
    pub locked: bool,
    /// Injections parked right now.
    pub held: usize,
    /// Injections a cap refused since the daemon started.
    pub dropped: u64,
}

impl LockBadge {
    /// `LOCK 3` while holding, `… 3` while draining. The glyph table has no
    /// padlock and the word survives an ASCII terminal, a colourblind reader
    /// and a screen reader alike — colour is never the only carrier here.
    pub fn label(self) -> String {
        if self.locked {
            format!("LOCK {}", self.held)
        } else {
            format!("… {}", self.held)
        }
    }

    pub fn style(self, theme: &Theme) -> Style {
        if self.locked {
            theme.yellow
        } else {
            theme.cyan
        }
    }

    /// The badge, with the drop count appended in faint when notices were
    /// lost — a queue that quietly ate something must say so.
    pub fn spans(self, theme: &Theme) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(self.label(), self.style(theme))];
        if self.dropped > 0 {
            spans.push(Span::styled(
                format!(" (+{} dropped)", self.dropped),
                theme.faint,
            ));
        }
        spans
    }
}
