//! Double-Shift detection for the menu hub (§4.2): two presses of a Shift
//! modifier key within `keys.double_shift_ms`, with nothing else pressed
//! between them.
//!
//! Shift only arrives as its own event under kitty's
//! `REPORT_ALL_KEYS_AS_ESCAPE_CODES` — on a plain terminal a tap is invisible —
//! so `available` records whether Shift events are ever seen; the footer hint
//! uses it to fall back to `ctrl+,`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};

/// Default maximum interval between the two taps (§4.2); `keys.double_shift_ms`
/// overrides it.
pub const DEFAULT_THRESHOLD_MS: u64 = 400;

/// The detector state. Lives on `AppState`; `keys::handle_key` feeds it every
/// raw event before the `Release`/`Repeat` drop, because a kitty tap arrives
/// as Press + Release.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoubleShift {
    /// When the first tap's Press landed; `None` while not armed.
    first_at: Option<Instant>,
    /// A Shift event was seen at least once — the terminal reports modifier
    /// keys, so the gesture exists. Before that the hint shows `ctrl+,`.
    available: bool,
}

fn is_shift(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Modifier(ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift)
    )
}

impl DoubleShift {
    /// Whether the terminal reports Shift presses at all — the kitty protocol
    /// is live, so the double-tap gesture can be hinted in the footer.
    pub fn available(&self) -> bool {
        self.available
    }

    /// Feed one raw key event.
    ///
    /// `active` is the caller-side gate — `Focus::Normal` and the
    /// `keys.double_shift` switch both on (§4.2). Returns `true` exactly on
    /// the second qualifying tap; `keys::handle_key` then opens the hub.
    pub fn observe(
        &mut self,
        key: KeyEvent,
        now: Instant,
        threshold_ms: u64,
        active: bool,
    ) -> bool {
        if is_shift(key.code) {
            self.available = true;
        }
        if !active {
            // An event outside `Normal` resets the pending pair (§4.2).
            self.first_at = None;
            return false;
        }
        // A Release does not arm and does not break the pair — a kitty tap is
        // Press + Release, so the Release must not clear `first_at`.
        if key.kind != KeyEventKind::Press {
            return false;
        }
        // Only a bare Shift press counts as a tap: Shift held with other
        // modifiers is a chord, and any other key breaks the pair.
        if is_shift(key.code) && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            if let Some(first) = self.first_at
                && now.saturating_duration_since(first).as_millis() <= threshold_ms as u128
            {
                self.first_at = None;
                return true;
            }
            self.first_at = Some(now);
            return false;
        }
        self.first_at = None;
        false
    }
}

#[cfg(test)]
#[path = "double_shift_tests.rs"]
mod double_shift_tests;
