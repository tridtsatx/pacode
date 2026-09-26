//! The settings overlay: a row of section tabs across the top, the section
//! names down the left, one `Setting  value` row per registry entry on the
//! right, the highlighted setting's description underneath, a scrollbar down
//! the right edge and a fixed hint footer.
//!
//! [`model`] holds what the overlay is looking at — section, highlight,
//! search query, inline edit — and answers a key press with a
//! [`model::SettingsOutcome`] the host carries out. Nothing in here writes the
//! config file, so a value the registry rejects is reported on the description
//! line before anything is saved.
//! [`tabs`] maps a section onto a glyph from the shared table and windows the
//! bar around the active tab. [`draw`] paints the result and never resizes the
//! terminal: every row is clipped to the area it was handed, so a 40x10
//! terminal gets a smaller overlay, not a broken one.

mod draw;
mod model;
mod tabs;

pub use draw::draw;
pub use model::{
    SettingsOutcome, SettingsState, on_key, search_matches, section_entries, section_index,
    section_order, visible_range,
};
pub use tabs::{Tab, TabBar, section_icon};
