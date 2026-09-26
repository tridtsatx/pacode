//! The glue between the app and the model picker: opening it, drawing it, and
//! turning a key into a route.
//!
//! The picker's own state and every question it answers — which models are
//! visible, which effort the highlighted model accepts, what one row says — live
//! in [`super::model_picker`]. This module only knows how to reach them from
//! the state the app already has: the catalog and the per-route flags. That split
//! is why a keypress, a click and a repaint cannot disagree about what is on
//! screen.

use pacode_render::RenderOptions;
use pacode_types::{Effort, ModelRoute, Request};

use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::model_picker::{Flags, ModelPicker};

/// Open the picker on the model and effort the session is already using.
/// Starting anywhere else would make the first thing the user sees a lie about
/// the current state.
pub fn open(state: &mut AppState) -> Vec<Action> {
    let effort = state.effort();
    state.focus = Focus::Overlay(Overlay::ModelPickerPage {
        picker: ModelPicker::new(effort),
    });
    state.dirty = true;
    vec![Action::Send(Request::ListModels)]
}

/// Draw the page over the popup area, so the frame comes from the same place
/// every other overlay's does.
pub fn draw(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    picker: &ModelPicker,
    opts: &RenderOptions,
) {
    crate::ui::model_picker_view::draw(frame, area, picker, &state.models, &flags(state), opts);
}

/// The per-route flags the picker reads: ZDR capability and favourite, loaded
/// from `prefs.toml` at session open.
pub fn flags(state: &AppState) -> Flags {
    state.model_flags.clone()
}

/// One key. Returns the actions the app should carry out.
pub fn handle_key(state: &mut AppState, key: crossterm::event::KeyEvent) -> Vec<Action> {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Taken out and put back: every question below reads the catalog and the
    // flags immutably while the page is mutating its own selection.
    let Focus::Overlay(Overlay::ModelPickerPage { picker }) = &mut state.session.focus else {
        return vec![];
    };
    let mut picker = std::mem::take(picker);
    let models = state.models.clone();
    let flags = flags(state);
    let session_route = state.meta.as_ref().map(|meta| meta.model.clone());
    let mut close = false;
    let mut picked = None;
    let mut actions = Vec::new();

    match key.code {
        KeyCode::Esc => close = true,
        KeyCode::Enter => {
            if let Some(model) = picker.highlighted(&models, &flags) {
                picked = Some(ModelRoute::new(&model.route.provider, &model.route.model));
            }
            close = true;
        }
        KeyCode::Down | KeyCode::Char('j') => picker.move_row(1, &models, &flags),
        KeyCode::Up | KeyCode::Char('k') => picker.move_row(-1, &models, &flags),
        KeyCode::Left | KeyCode::Char('h') => picker.move_provider(-1, &models, &flags),
        KeyCode::Right | KeyCode::Char('l') => picker.move_provider(1, &models, &flags),
        KeyCode::Tab => {
            // Tab is the effort ladder here, not focus traversal: the ladder is
            // the thing on this page that is otherwise hard to reach. It skips
            // levels the highlighted model does not accept, so a Tab that lands
            // on a rung always lands on one that exists.
            if picker
                .cycle_effort(
                    !key.modifiers.contains(KeyModifiers::SHIFT),
                    &models,
                    &flags,
                )
                .is_some()
            {
                // The ladder is about the *current* model, so the route is
                // whatever the session is already on.
                picked = session_route.clone();
            }
        }
        KeyCode::Backspace => picker.backspace(),
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            picker.push_char(c);
        }
        _ => {}
    }

    if let Some(route) = picked {
        state.save_pref_model(&route.to_string());
        actions.push(Action::Send(Request::SetModel(route)));
    }
    // The effort always goes out: a Tab that moved the ladder has to reach the
    // daemon, and one that did not is a value the daemon already agrees with.
    actions.push(Action::Send(Request::SetEffort(picker.effort)));

    if let Focus::Overlay(Overlay::ModelPickerPage { picker: slot }) = &mut state.session.focus {
        *slot = picker;
    }
    if close {
        state.focus = Focus::Normal;
    }
    state.dirty = true;
    actions
}

/// The effort the picker is showing, read out of the page's own state.
pub fn effort_of(picker: &ModelPicker) -> Effort {
    picker.effort
}
