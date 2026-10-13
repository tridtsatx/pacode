//! Entry point for the settings overlay: the one place that knows how a value
//! leaves the overlay and lands in the config file.
//!
//! The overlay itself (`crate::ui::settings`) is pure — it reads the registry
//! and the live `Config`, and answers a key press with a `SettingsOutcome`.
//! Carrying that out is [`commit`]'s job: writing the owning file, applying
//! the value to the running client, and asking the daemon for a marketplace
//! catalogue when the user changes one. A host wires [`draw`] and [`commit`]
//! and nothing else.
//!
//! The write path mirrors the one the settings menu view uses. It is
//! duplicated rather than shared because the menu's is private to the menu
//! base, and a settings overlay that could not write a value would be a
//! read-only imitation of one.

use crossterm::event::KeyEvent;
use pacode_config::registry;
use pacode_types::ToastLevel;
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};

use pacode_render::RenderOptions;

use crate::keys::Action;
use crate::state::AppState;
use crate::ui::settings::{self, SettingsOutcome, SettingsState};

/// Draw the overlay into `area`.
///
/// `area` is whatever the host reserved; the overlay fills it and never asks
/// for a different one, so a narrow terminal gets a narrower overlay rather
/// than a request to resize.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    ui: &SettingsState,
    opts: &RenderOptions,
) {
    settings::draw(frame, area, &state.config, ui, opts);
}

/// What the host should do with the key it just handed over.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsResult {
    /// The overlay stays open; these actions go out.
    Stay(Vec<Action>),
    /// The overlay closes.
    Close,
}

/// The body rows a key press pages by, measured with the same slot arithmetic
/// the overlay draws with: the border pair, the tab row (only when the section
/// column is dropped for width), the description.
pub fn rows_visible(area: Rect) -> usize {
    let inner = area.inner(Margin::new(1, 1));
    if inner.height == 0 {
        return 1;
    }
    let tab_row = usize::from(settings::shows_tab_row(inner.width));
    usize::from(inner.height).saturating_sub(2 + tab_row).max(1)
}

/// Feed one key to the overlay and carry out whatever it asks for.
///
/// A commit only the daemon can apply — a new plugin marketplace — comes back
/// as an action for the host to dispatch rather than as a local edit.
pub fn commit(
    state: &mut AppState,
    ui: &mut SettingsState,
    key: KeyEvent,
    area: Rect,
) -> SettingsResult {
    match settings::on_key(ui, key, &state.config, rows_visible(area)) {
        SettingsOutcome::Consumed => SettingsResult::Stay(vec![]),
        SettingsOutcome::Close => SettingsResult::Close,
        SettingsOutcome::Commit { key, value } => {
            SettingsResult::Stay(apply_value(state, key, value))
        }
        SettingsOutcome::Reset { key } => SettingsResult::Stay(reset_value(state, key)),
        SettingsOutcome::Run { command } => {
            SettingsResult::Stay(crate::commands::execute(state, command))
        }
    }
}

/// Write `key = value` into the owning config file and apply it to the live
/// `state.config`. A failed write toasts; the in-memory apply runs regardless,
/// matching what the file will hold the next time it saves.
fn apply_value(state: &mut AppState, key: &str, value: pacode_config::toml::Value) -> Vec<Action> {
    if let Err(e) = pacode_config::update_config_value(&state.paths, key, value.clone()) {
        log::warn!("failed to write config key '{key}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
    registry::apply_to_config(&mut state.config, key, &value);
    state.apply_runtime_config(key);
    if key.starts_with("mcp") {
        state.invalidate_cache(crate::cache::ListKind::McpServers);
        state.invalidate_cache(crate::cache::ListKind::Commands);
    }
    if key.starts_with("plugins.") {
        state.invalidate_cache(crate::cache::ListKind::Plugins);
        state.invalidate_cache(crate::cache::ListKind::Marketplace);
        state.invalidate_cache(crate::cache::ListKind::Commands);
    }
    if key == "plugins.marketplace" {
        state.marketplace.clear();
    }
    state.dirty = true;
    runtime_actions(state, key)
}

/// Back to the registry default: the key leaves the owning config file, the
/// in-memory value follows it, and the runtime is told.
fn reset_value(state: &mut AppState, key: &str) -> Vec<Action> {
    if let Err(e) = pacode_config::remove_config_value(&state.paths, key) {
        log::warn!("failed to remove config key '{key}': {e}");
        state.push_toast(
            ToastLevel::Error,
            "Failed to update config".to_string(),
            Some(e.to_string()),
            std::time::Instant::now(),
        );
    }
    registry::reset_in_config(&mut state.config, key);
    state.apply_runtime_config(key);
    if key == "plugins.marketplace" {
        state.marketplace.clear();
    }
    state.dirty = true;
    runtime_actions(state, key)
}

/// The one setting whose value the client cannot apply to itself: a new
/// marketplace is a catalogue only the daemon can fetch.
fn runtime_actions(state: &AppState, key: &str) -> Vec<Action> {
    if key != "plugins.marketplace" {
        return vec![];
    }
    let source = state.config.plugins.marketplace.clone();
    if source.is_empty() {
        return vec![];
    }
    vec![Action::Send(pacode_types::Request::BrowseMarketplace {
        source,
        query: String::new(),
    })]
}
