//! Keys for the model palette.
//!
//! | key | does |
//! |---|---|
//! | letters, digits | type into the search (`h j k l` included) |
//! | ↑ ↓, ctrl+p ctrl+n, pgup pgdn | move the selection |
//! | tab / shift+tab | step the highlighted model's effort (local until enter) |
//! | enter | model: switch to it (+ effort if changed); provider header: sign in (another account when signed in) |
//! | esc | close, nothing sent |
//! | alt+f / alt+z | mark favourite / ZDR-capable |
//! | alt+k | capability filter (all / reasoning / vision) |
//! | ctrl+r | force-refresh the catalog |
//! | alt+a | next stored account of the highlighted provider |
//! | alt+x, twice | sign the highlighted provider out (the first press asks on its header) |
//!
//! The chords come from the keymap (`model_picker_*` actions), so `/keys` can
//! rebind them and the palette's hints spell whatever is bound.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_types::{Effort, ModelRoute, Request, ToastLevel};

use crate::binding::Action as KeyAction;
use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay, PromptBatch};
use crate::ui::model_palette::{Anchor, LoginLine, ModelPalette, Row, Sources};

/// How far page up / page down move the selection.
const PAGE: isize = 10;

/// What opened the palette, which decides where the selection lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// `/model`, the hub's model row: the session's route.
    Model,
    /// `/login`: the first provider that needs a sign-in.
    Login,
    /// Alt+Z on a route that is not ZDR-capable: capable routes only, and
    /// Enter also turns ZDR on.
    Zdr,
}

pub(crate) fn open(state: &mut AppState, opening: Opening) -> Vec<Action> {
    let zdr_only = crate::state::zdr_active(&state.config, state.meta.as_ref());
    let route = state
        .meta
        .as_ref()
        .map(|meta| Anchor::Route(meta.model.clone()));
    let anchor = match opening {
        Opening::Model | Opening::Zdr => route,
        Opening::Login => Some(Anchor::FirstSignIn),
    };
    let palette = ModelPalette::new(state.effort(), zdr_only, opening == Opening::Zdr, anchor);
    state.focus = Focus::Overlay(Overlay::ModelPalette { palette });
    state.dirty = true;
    vec![
        Action::Send(Request::ListModels),
        Action::Send(Request::ListAuth),
    ]
}

pub(crate) fn handle_key(state: &mut AppState, key: KeyEvent) -> Vec<Action> {
    // Taken out and put back: the rows borrow the catalog on `state` while
    // the palette's own selection changes.
    let Focus::Overlay(Overlay::ModelPalette { palette }) = &mut state.session.focus else {
        return vec![];
    };
    let mut palette = std::mem::take(palette);
    let (actions, close) = handle(state, &mut palette, key);
    if close {
        state.focus = Focus::Normal;
    } else if let Focus::Overlay(Overlay::ModelPalette { palette: slot }) = &mut state.session.focus
    {
        *slot = palette;
    }
    state.dirty = true;
    actions
}

/// The palette's own chords — the keymap's `model_picker_*` actions.
#[derive(Clone, Copy)]
enum Chord {
    Favorite,
    Zdr,
    Refresh,
    Kind,
    Account,
    Logout,
}

fn chord_of(state: &AppState, key: KeyEvent) -> Option<Chord> {
    [
        (KeyAction::ModelPickerFavorite, Chord::Favorite),
        (KeyAction::ModelPickerZdr, Chord::Zdr),
        (KeyAction::ModelPickerRefresh, Chord::Refresh),
        (KeyAction::ModelPickerKind, Chord::Kind),
        (KeyAction::ModelPickerAccount, Chord::Account),
        (KeyAction::ModelPickerLogout, Chord::Logout),
    ]
    .into_iter()
    .find(|(action, _)| {
        state
            .keymap
            .bindings_for(*action)
            .iter()
            .any(|binding| crate::binding::matches_key(binding, key))
    })
    .map(|(_, chord)| chord)
}

/// The highlighted row, owned, so the handler can mutate `state` after.
enum Picked {
    Model {
        route: ModelRoute,
        effort: Option<Effort>,
        signed_in: bool,
    },
    SignIn(String),
    Nothing,
}

fn picked(state: &AppState, palette: &ModelPalette) -> Picked {
    let src = Sources::of(state);
    let rows = palette.rows(&src);
    match palette.selected(&rows) {
        Some(Row::Model {
            model, signed_in, ..
        }) => Picked::Model {
            route: model.route.clone(),
            effort: palette.effort_for(model),
            signed_in: *signed_in,
        },
        Some(Row::Header {
            provider: Some(provider),
            ..
        }) => Picked::SignIn(provider.clone()),
        Some(Row::Header { provider: None, .. }) | Some(Row::Gap) | None => Picked::Nothing,
    }
}

fn handle(state: &mut AppState, palette: &mut ModelPalette, key: KeyEvent) -> (Vec<Action>, bool) {
    // A pending logout lives for exactly one key: its chord again confirms,
    // anything else (Esc included) only cancels it.
    if let Some(provider) = palette.confirm_logout.take() {
        if matches!(chord_of(state, key), Some(Chord::Logout)) {
            return (logout(state, palette, provider), false);
        }
        if key.code == KeyCode::Esc {
            return (vec![], false);
        }
    }
    if let Some(chord) = chord_of(state, key) {
        let actions = match chord {
            Chord::Favorite => toggle_mark(state, palette, Mark::Favorite),
            Chord::Zdr => toggle_mark(state, palette, Mark::Zdr),
            // The reply lands like `ListModels`; dispatch marks the list in
            // flight, so the title badges `loading…` while it runs.
            Chord::Refresh => vec![Action::Send(Request::RefreshModels)],
            Chord::Kind => {
                palette.cycle_kind();
                vec![]
            }
            Chord::Account => switch_account(state, palette),
            Chord::Logout => {
                palette.confirm_logout = highlighted_auth(state, palette)
                    .filter(|info| !info.accounts.is_empty())
                    .map(|info| info.id);
                vec![]
            }
        };
        return (actions, false);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let typing = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    let step = match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        KeyCode::Char('p') if ctrl => Some(-1),
        KeyCode::Char('n') if ctrl => Some(1),
        KeyCode::PageUp => Some(-PAGE),
        KeyCode::PageDown => Some(PAGE),
        _ => None,
    };
    if let Some(step) = step {
        let src = Sources::of(state);
        palette.move_by(step, &palette.rows(&src));
        return (vec![], false);
    }
    match key.code {
        KeyCode::Esc => return (vec![], true),
        KeyCode::Enter => return commit(state, palette),
        KeyCode::Tab | KeyCode::BackTab => {
            let forward = key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT);
            let src = Sources::of(state);
            let rows = palette.rows(&src);
            if let Some(Row::Model { model, .. }) = palette.selected(&rows) {
                let model = *model;
                palette.cycle_effort(forward, model);
            }
        }
        KeyCode::Backspace => palette.backspace(),
        KeyCode::Char(c) if typing => palette.push_char(c),
        _ => {}
    }
    (vec![], false)
}

/// Enter. A model switches the session — the route, then the effort if it
/// changed, then ZDR for the Alt+Z flow — as one ordered sequence that stops
/// at the first refusal. Prefs and recents follow the daemon's
/// `SessionUpdated`, never this request. A provider that needs a sign-in
/// starts one and the palette stays open to show it.
fn commit(state: &mut AppState, palette: &mut ModelPalette) -> (Vec<Action>, bool) {
    match picked(state, palette) {
        Picked::Model {
            route,
            signed_in: false,
            ..
        } => (sign_in(palette, route.provider), false),
        Picked::SignIn(provider) => (sign_in(palette, provider), false),
        Picked::Model { route, effort, .. } => {
            let meta = state.meta.as_ref();
            let mut requests = Vec::new();
            if meta.is_none_or(|meta| meta.model != route) {
                requests.push(Request::SetModel(route));
            }
            if let Some(effort) = effort.filter(|e| meta.is_none_or(|meta| meta.effort != *e)) {
                requests.push(Request::SetEffort(effort));
            }
            if palette.for_zdr {
                requests.push(Request::SetZdr(true));
            }
            let actions = if requests.is_empty() {
                vec![]
            } else {
                vec![crate::keys::prompt_batch_action(PromptBatch { requests })]
            };
            (actions, true)
        }
        Picked::Nothing => (vec![], false),
    }
}

fn sign_in(palette: &mut ModelPalette, provider: String) -> Vec<Action> {
    palette.login = Some(LoginLine {
        provider: provider.clone(),
        text: "starting".to_string(),
        failed: false,
    });
    vec![Action::Send(Request::Login { provider })]
}

/// The auth record of the highlighted row's provider.
fn highlighted_auth(
    state: &AppState,
    palette: &ModelPalette,
) -> Option<pacode_types::ProviderAuthInfo> {
    let src = Sources::of(state);
    let rows = palette.rows(&src);
    let provider = palette.selected(&rows)?.provider()?;
    state
        .auth_providers
        .iter()
        .find(|info| info.id == provider)
        .cloned()
}

fn switch_account(state: &AppState, palette: &ModelPalette) -> Vec<Action> {
    let Some(info) = highlighted_auth(state, palette).filter(|info| info.accounts.len() > 1) else {
        return vec![];
    };
    let current = info
        .active
        .as_ref()
        .and_then(|active| info.accounts.iter().position(|acc| acc == active))
        .unwrap_or(0);
    let label = info.accounts[(current + 1) % info.accounts.len()].clone();
    vec![Action::Send(Request::SetAuthAccount {
        provider: info.id,
        label,
    })]
}

/// The confirming press: sign `provider` out, if it is still the one
/// highlighted and still has an account.
fn logout(state: &AppState, palette: &ModelPalette, provider: String) -> Vec<Action> {
    let Some(info) = highlighted_auth(state, palette)
        .filter(|info| info.id == provider && !info.accounts.is_empty())
    else {
        return vec![];
    };
    vec![Action::Send(Request::Logout {
        provider: info.id,
        label: info.active,
    })]
}

/// The per-model flag a mark chord toggles.
#[derive(Clone, Copy)]
enum Mark {
    Zdr,
    Favorite,
}

/// Toggle `mark` on the highlighted model, persist it, and send
/// `SetModelFlag` so the daemon enforces the same flag. The ZDR gate mirrors
/// the daemon's: the active route cannot lose its mark while ZDR is on.
fn toggle_mark(state: &mut AppState, palette: &ModelPalette, mark: Mark) -> Vec<Action> {
    let Picked::Model { route, .. } = picked(state, palette) else {
        return vec![];
    };
    let key = route.to_string();
    let mut flags = state.model_flags.get(&key).copied().unwrap_or_default();
    match mark {
        Mark::Zdr => {
            let active = state.meta.as_ref().is_some_and(|meta| meta.model == route);
            if flags.zdr_capable && active && state.zdr_active() {
                state.push_notice_with_level(
                    ToastLevel::Warn,
                    "can't unmark the active route while ZDR is on".to_string(),
                );
                return vec![];
            }
            flags.zdr_capable = !flags.zdr_capable;
        }
        Mark::Favorite => flags.favorite = !flags.favorite,
    }
    if flags == pacode_types::ModelFlags::default() {
        state.model_flags.remove(&key);
    } else {
        state.model_flags.insert(key.clone(), flags);
    }
    state.save_pref_model_flags();
    vec![Action::Send(Request::SetModelFlag { model: key, flags })]
}

#[cfg(test)]
#[path = "keys_palette_tests.rs"]
mod keys_palette_tests;
