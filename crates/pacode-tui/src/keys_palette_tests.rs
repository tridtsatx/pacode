use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use pacode_types::{
    AuthState, Config, Effort, ModelFlags, ModelInfo, ModelRoute, ProviderAuthInfo, Request,
    SessionId, SessionMeta,
};

use crate::keys::{Action, handle_key};
use crate::state::{AppState, Focus, Overlay};
use crate::ui::model_palette::ModelPalette;

fn model(provider: &str, id: &str, efforts: Option<&[Effort]>) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider, id),
        display_name: id.to_string(),
        context_window: Some(200_000),
        supports_reasoning: true,
        supports_vision: false,
        supported_efforts: efforts.map(<[Effort]>::to_vec),
        pricing: None,
        ..Default::default()
    }
}

fn meta(route: &str, effort: Effort, zdr: bool) -> SessionMeta {
    let (provider, id) = route.split_once('/').unwrap_or(("p", route));
    SessionMeta {
        id: SessionId::new("ses_palette"),
        name: None,
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: ModelRoute::new(provider, id),
        effort,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr,
    }
}

/// A state on `p/a` at `high`, prefs in a temp dir (the palette's mark keys
/// write them) and no recents leaking in from the real `prefs.toml`.
fn state() -> (AppState, tempfile::TempDir) {
    let mut state = AppState::new(Config::default(), "test".into(), 120, 34);
    let dir = tempfile::tempdir().expect("tempdir");
    state.paths = pacode_config::Paths::under(dir.path());
    state.model_flags.clear();
    state.recent_models.clear();
    state.awaiting_snapshot = false;
    state.meta = Some(meta("p/a", Effort::High, false));
    state.models = vec![
        model("p", "a", None),
        model("p", "b", Some(&[Effort::Low, Effort::Medium])),
        model("p", "haiku", None),
    ];
    (state, dir)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(state: &mut AppState, event: KeyEvent) -> Vec<Action> {
    handle_key(state, event, Instant::now())
}

fn palette(state: &AppState) -> &ModelPalette {
    match &state.focus {
        Focus::Overlay(Overlay::ModelPalette { palette }) => palette,
        other => panic!("expected the model palette, got {other:?}"),
    }
}

fn open(state: &mut AppState) {
    let actions = crate::commands::execute(state, "/model");
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::ListModels),
            Action::Send(Request::ListAuth)
        ]
    );
    palette(state);
}

fn sequence(actions: &[Action]) -> Vec<Request> {
    match actions {
        [Action::Send(request)] => vec![request.clone()],
        [Action::SendSequence { requests }] => requests.clone(),
        other => panic!("expected one send, got {other:?}"),
    }
}

/// B01: navigation, typing, Tab and Esc send nothing at all.
#[test]
fn only_enter_reaches_the_daemon() {
    let (mut state, _dir) = state();
    open(&mut state);
    for event in [
        key(KeyCode::Down),
        key(KeyCode::Up),
        key(KeyCode::Char('x')),
        key(KeyCode::Backspace),
        key(KeyCode::Tab),
        key(KeyCode::BackTab),
        key(KeyCode::PageDown),
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        key(KeyCode::Esc),
    ] {
        assert!(press(&mut state, event).is_empty(), "{event:?} sent");
    }
    assert_eq!(state.focus, Focus::Normal, "esc closes");
}

/// B02: every letter is search text, `h j k l` included; arrows move.
#[test]
fn letters_type_into_the_search() {
    let (mut state, _dir) = state();
    open(&mut state);
    for c in "haiku".chars() {
        assert!(press(&mut state, key(KeyCode::Char(c))).is_empty());
    }
    assert_eq!(palette(&state).query, "haiku");
    assert!(state.model_flags.is_empty(), "plain letters never mark");
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        sequence(&actions),
        vec![Request::SetModel(ModelRoute::new("p", "haiku"))]
    );
}

/// B03 + B13: Tab moves the palette's effort along the *highlighted* model's
/// ladder, and Enter commits route and effort together, in order.
#[test]
fn enter_sends_the_route_then_the_changed_effort() {
    let (mut state, _dir) = state();
    open(&mut state);
    state.recent_models.clear();
    // Opens on the session route `p/a`; move to `p/b` (low/medium only).
    press(&mut state, key(KeyCode::Down));
    assert!(press(&mut state, key(KeyCode::Tab)).is_empty());
    // high snapped to medium on `b`, then Tab wrapped to `low`.
    assert_eq!(palette(&state).effort, Effort::Low);
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        sequence(&actions),
        vec![
            Request::SetModel(ModelRoute::new("p", "b")),
            Request::SetEffort(Effort::Low),
        ]
    );
    assert_eq!(state.focus, Focus::Normal);
}

/// B13: typing moves the highlight; the effort sent is the new model's.
#[test]
fn typing_onto_a_narrower_model_snaps_the_effort() {
    let (mut state, _dir) = state();
    state.meta = Some(meta("p/a", Effort::Max, false));
    open(&mut state);
    press(&mut state, key(KeyCode::Char('b')));
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        sequence(&actions),
        vec![
            Request::SetModel(ModelRoute::new("p", "b")),
            Request::SetEffort(Effort::Medium),
        ]
    );
}

#[test]
fn enter_on_the_current_route_and_effort_sends_nothing() {
    let (mut state, _dir) = state();
    open(&mut state);
    assert!(press(&mut state, key(KeyCode::Enter)).is_empty());
    assert_eq!(state.focus, Focus::Normal);
}

/// B04: with ZDR on, the palette offers only capable routes.
#[test]
fn zdr_on_filters_the_palette() {
    let (mut state, _dir) = state();
    state.meta = Some(meta("p/a", Effort::High, true));
    state.model_flags.insert(
        "p/a".to_string(),
        ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    open(&mut state);
    assert!(palette(&state).zdr_only);
    press(&mut state, key(KeyCode::Down));
    // Only `p/a` is listed, so Down stays there and Enter is a no-op switch.
    assert!(press(&mut state, key(KeyCode::Enter)).is_empty());
}

/// Alt+Z on a route that is not capable: the palette picks a capable one and
/// Enter sends route then ZDR as one ordered sequence — no optimistic flag.
#[test]
fn the_zdr_flow_switches_then_enables_in_order() {
    let (mut state, _dir) = state();
    state.model_flags.insert(
        "p/b".to_string(),
        ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    press(
        &mut state,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT),
    );
    assert!(palette(&state).for_zdr);
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        sequence(&actions),
        vec![
            Request::SetModel(ModelRoute::new("p", "b")),
            Request::SetEffort(Effort::Medium),
            Request::SetZdr(true),
        ]
    );
    assert!(
        state.meta.as_ref().is_some_and(|m| !m.zdr),
        "ZDR shows on once the daemon says so"
    );
}

#[test]
fn mark_chords_toggle_flags_and_follow_rebinds() {
    let (mut state, _dir) = state();
    open(&mut state);
    let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
    let actions = press(&mut state, alt_z);
    assert!(matches!(
        actions.as_slice(),
        [Action::Send(Request::SetModelFlag { model, flags })]
            if model == "p/a" && flags.zdr_capable
    ));
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    press(&mut state, ctrl_g);
    assert!(state.model_flags.get("p/a").is_some_and(|f| f.favorite));
    assert!(palette(&state).query.is_empty(), "chords never type");

    state.keymap.set_bindings(
        crate::binding::Action::ModelPickerFavorite,
        vec![crate::binding::Binding {
            code: KeyCode::Char('m'),
            mods: KeyModifiers::CONTROL,
        }],
    );
    assert!(press(&mut state, ctrl_g).is_empty(), "old chord goes quiet");
    press(
        &mut state,
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
    );
    assert!(state.model_flags.get("p/a").is_some_and(|f| !f.favorite));
}

#[test]
fn the_active_route_keeps_its_zdr_mark_while_zdr_is_on() {
    let (mut state, _dir) = state();
    state.meta = Some(meta("p/a", Effort::High, true));
    state.model_flags.insert(
        "p/a".to_string(),
        ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    open(&mut state);
    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT),
    );
    assert!(actions.is_empty());
    assert!(state.model_flags.get("p/a").is_some_and(|f| f.zdr_capable));
}

#[test]
fn refresh_and_kind_chords() {
    let (mut state, _dir) = state();
    open(&mut state);
    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    );
    assert_eq!(actions, vec![Action::Send(Request::RefreshModels)]);
    press(
        &mut state,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT),
    );
    assert_eq!(
        palette(&state).kind,
        crate::ui::model_catalog::Kind::Reasoning
    );
}

fn provider(id: &str, state: AuthState, accounts: &[&str]) -> ProviderAuthInfo {
    ProviderAuthInfo {
        id: id.to_string(),
        display_name: id.to_string(),
        auth_kind: "oauth".to_string(),
        detail: String::new(),
        recommended: false,
        state,
        accounts: accounts.iter().map(|a| a.to_string()).collect(),
        active: accounts.first().map(|a| a.to_string()),
    }
}

/// `/login` lands on the first provider to sign in to; Enter starts the
/// flow inline and the palette stays open to show it.
#[test]
fn login_lands_on_the_first_sign_in_and_starts_it_inline() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![
        provider("p", AuthState::Configured, &["one"]),
        provider("zed", AuthState::NotConfigured, &[]),
    ];
    crate::commands::execute(&mut state, "/login");
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        actions,
        vec![Action::Send(Request::Login {
            provider: "zed".to_string()
        })]
    );
    assert_eq!(
        palette(&state).login.as_ref().map(|l| l.provider.as_str()),
        Some("zed")
    );

    // Progress lands on the header; Done clears it and keeps the palette.
    let now = Instant::now();
    state.apply_login_progress("zed".to_string(), pacode_types::LoginStage::Waiting, now);
    assert!(
        palette(&state)
            .login
            .as_ref()
            .is_some_and(|l| l.text.contains("waiting") && !l.failed)
    );
    state.apply_login_progress(
        "zed".to_string(),
        pacode_types::LoginStage::Done {
            label: "me".to_string(),
        },
        now,
    );
    assert_eq!(palette(&state).login, None);
}

/// B23: `a` is search text; switching accounts is a chord.
#[test]
fn account_switch_is_a_chord() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![provider("p", AuthState::Configured, &["one", "two"])];
    open(&mut state);
    assert!(press(&mut state, key(KeyCode::Char('a'))).is_empty());
    assert_eq!(palette(&state).query, "a");
    press(&mut state, key(KeyCode::Backspace));

    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
    );
    assert_eq!(
        actions,
        vec![Action::Send(Request::SetAuthAccount {
            provider: "p".to_string(),
            label: "two".to_string(),
        })]
    );
}

/// Signing out takes two presses of the logout chord, the first one asking on
/// the provider's header; any other key in between cancels. `ctrl+d` is not
/// the chord: it still quits, palette or not.
#[test]
fn logout_asks_first_and_ctrl_d_still_quits() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![provider("p", AuthState::Configured, &["one"])];
    open(&mut state);
    let alt_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);

    assert!(
        press(&mut state, alt_x).is_empty(),
        "the first press only asks"
    );
    assert_eq!(palette(&state).confirm_logout.as_deref(), Some("p"));
    assert!(press(&mut state, key(KeyCode::Down)).is_empty());
    assert_eq!(palette(&state).confirm_logout, None, "another key cancels");
    press(&mut state, key(KeyCode::Up));

    press(&mut state, alt_x);
    // Esc while asking only cancels; the palette stays.
    press(&mut state, key(KeyCode::Esc));
    assert_eq!(palette(&state).confirm_logout, None);

    press(&mut state, alt_x);
    let actions = press(&mut state, alt_x);
    assert_eq!(
        actions,
        vec![Action::Send(Request::Logout {
            provider: "p".to_string(),
            label: Some("one".to_string()),
        })]
    );

    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    assert_eq!(actions, vec![Action::Quit]);
}

/// The confirmation is drawn on the provider's header, spelled from the keymap.
#[test]
fn the_logout_question_is_shown_on_the_header() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![provider("p", AuthState::Configured, &["one"])];
    open(&mut state);
    press(
        &mut state,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
    );
    let view = draw(&mut state, 120, 34);
    assert!(view.contains("alt+x again to sign out P"), "{view}");
}

/// Enter on a signed-in provider's header signs in again: another account,
/// as the old login screen allowed.
#[test]
fn enter_on_a_signed_in_header_adds_an_account() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![provider("p", AuthState::Configured, &["one"])];
    open(&mut state);
    press(&mut state, key(KeyCode::Char('p')));
    // The best match for "p" is the provider's header row.
    let mut actions = Vec::new();
    for _ in 0..4 {
        actions = press(&mut state, key(KeyCode::Enter));
        if !actions.is_empty() {
            break;
        }
        press(&mut state, key(KeyCode::Up));
    }
    assert_eq!(
        actions,
        vec![Action::Send(Request::Login {
            provider: "p".to_string()
        })]
    );
}

/// `/login` under ZDR still lists the providers to sign in to.
#[test]
fn login_under_zdr_still_offers_providers() {
    let (mut state, _dir) = state();
    state.config.zdr = pacode_types::ZdrMode::Always;
    state.auth_providers = vec![provider("zed", AuthState::NotConfigured, &[])];
    crate::commands::execute(&mut state, "/login");
    assert!(palette(&state).zdr_only);
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        actions,
        vec![Action::Send(Request::Login {
            provider: "zed".to_string()
        })]
    );
}

/// When a sign-in finishes, the selection follows the provider's group as its
/// models arrive.
#[test]
fn a_finished_sign_in_lands_on_the_new_models() {
    let (mut state, _dir) = state();
    state.auth_providers = vec![provider("zed", AuthState::NotConfigured, &[])];
    crate::commands::execute(&mut state, "/login");
    press(&mut state, key(KeyCode::Enter));
    state.apply_login_progress(
        "zed".to_string(),
        pacode_types::LoginStage::Done {
            label: "me".to_string(),
        },
        Instant::now(),
    );
    state.auth_providers = vec![provider("zed", AuthState::Configured, &["me"])];
    state.models.push(model("zed", "zeta", None));
    let actions = press(&mut state, key(KeyCode::Enter));
    assert_eq!(
        sequence(&actions),
        vec![Request::SetModel(ModelRoute::new("zed", "zeta"))]
    );
}

/// B17: a pick writes no prefs; the accepted `SessionUpdated` does, with the
/// route switch recorded in recents.
#[test]
fn prefs_and_recents_follow_the_daemon_not_the_pick() {
    let (mut state, _dir) = state();
    open(&mut state);
    press(&mut state, key(KeyCode::Down));
    press(&mut state, key(KeyCode::Enter));
    assert_eq!(pacode_config::load_prefs(&state.paths).model, None);
    assert!(state.recent_models.is_empty());

    crate::state::events::apply_event(
        &mut state,
        1,
        pacode_types::Event::SessionUpdated(meta("p/b", Effort::Medium, false)),
        Instant::now(),
    );
    assert_eq!(state.recent_models, vec!["p/b", "p/a"]);
    // The write rides the one prefs outbox, as the event loop hands it off.
    let delta = state
        .take_prefs_write()
        .expect("the switch moves the defaults");
    state.prefs_outbox.post(delta);
    state.prefs_outbox.flush(&state.paths);
    let prefs = pacode_config::load_prefs(&state.paths);
    assert_eq!(prefs.model.as_deref(), Some("p/b"));
    assert_eq!(prefs.recent_models, vec!["p/b", "p/a"]);
    assert_eq!(state.recent_models, vec!["p/b", "p/a"]);

    // An update that does not switch the route leaves recents alone.
    crate::state::events::apply_event(
        &mut state,
        2,
        pacode_types::Event::SessionUpdated(meta("p/b", Effort::Low, false)),
        Instant::now(),
    );
    assert_eq!(state.recent_models, vec!["p/b", "p/a"]);
    assert_eq!(
        state.take_prefs_write(),
        Some(crate::state::PrefsDelta {
            effort: Some(Effort::Low),
            ..Default::default()
        })
    );
}

#[test]
fn recents_are_unique_and_capped() {
    let (mut state, _dir) = state();
    for i in 0..8 {
        state.push_recent_model(&format!("p/m{i}"));
    }
    state.push_recent_model("p/m5");
    assert_eq!(
        state.recent_models,
        vec!["p/m5", "p/m7", "p/m6", "p/m4", "p/m3"]
    );
}

#[test]
fn model_with_argument_sends_route_then_downgrade_in_order() {
    let (mut state, _dir) = state();
    let actions = crate::commands::execute(&mut state, "/model p/b");
    assert_eq!(
        sequence(&actions),
        vec![
            Request::SetModel(ModelRoute::new("p", "b")),
            Request::SetEffort(Effort::Medium),
        ]
    );
    assert_eq!(pacode_config::load_prefs(&state.paths).model, None);
    // The downgrade notice is phrased as a request: a refused switch must not
    // leave it claiming the effort changed.
    assert_eq!(
        notices(&state),
        vec!["p/b does not accept effort high; asking for medium with the switch"]
    );
}

fn notices(state: &AppState) -> Vec<String> {
    state
        .transcript
        .cells
        .iter()
        .filter_map(|cell| {
            if let crate::state::CellKind::Item(pacode_types::TranscriptKind::Notice {
                text, ..
            }) = &cell.kind
            {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect()
}

/// `/model <the current route>` switches nothing; when the session's effort
/// is outside the route's ladder only the effort is asked for, and the notice
/// does not talk about a switch.
#[test]
fn model_on_the_current_route_asks_for_the_effort_alone() {
    let (mut state, _dir) = state();
    state.meta = Some(meta("p/b", Effort::High, false));
    let actions = crate::commands::execute(&mut state, "/model p/b");
    assert_eq!(sequence(&actions), vec![Request::SetEffort(Effort::Medium)]);
    assert_eq!(
        notices(&state),
        vec!["p/b does not accept effort high; asking for medium"]
    );
}

/// `ctrl+y` is the ZDR mark's second default chord, for terminals that eat
/// alt+z.
#[test]
fn ctrl_y_marks_the_row_zdr_capable() {
    let (mut state, _dir) = state();
    open(&mut state);
    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    );
    assert!(matches!(
        actions.as_slice(),
        [Action::Send(Request::SetModelFlag { model, flags })]
            if model == "p/a" && flags.zdr_capable
    ));
    assert!(palette(&state).query.is_empty(), "the chord never types");
}

/// Plain `s` and `z` (the old picker's mark keys) are search text.
#[test]
fn plain_s_and_z_type_into_the_search() {
    let (mut state, _dir) = state();
    open(&mut state);
    for c in ['s', 'z'] {
        assert!(press(&mut state, key(KeyCode::Char(c))).is_empty());
    }
    assert_eq!(palette(&state).query, "sz");
    assert!(state.model_flags.is_empty(), "no mark was toggled");
}

/// A mark chord after a query marks the highlighted match and keeps the
/// query as typed.
#[test]
fn a_mark_after_a_query_marks_the_match() {
    let (mut state, _dir) = state();
    open(&mut state);
    for c in "haiku".chars() {
        press(&mut state, key(KeyCode::Char(c)));
    }
    press(
        &mut state,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
    );
    let actions = press(
        &mut state,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT),
    );
    assert!(matches!(
        actions.as_slice(),
        [Action::Send(Request::SetModelFlag { model, flags })]
            if model == "p/haiku" && flags.zdr_capable && flags.favorite
    ));
    assert!(
        state
            .model_flags
            .get("p/haiku")
            .is_some_and(|f| f.favorite && f.zdr_capable)
    );
    assert!(!state.model_flags.contains_key("p/a"));
    assert_eq!(palette(&state).query, "haiku");
}

/// A full frame at `cols`x`rows`, as the app draws it.
fn draw(state: &mut AppState, cols: u16, rows: u16) -> String {
    let buffer = draw_buffer(state, cols, rows);
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_buffer(state: &mut AppState, cols: u16, rows: u16) -> ratatui::buffer::Buffer {
    // Mark the layer seen, so the palette is drawn full size rather than the
    // first frame of its unfold.
    state.last_focus_key = crate::ui::focus_key(&state.focus);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            let prepared = crate::ui::prepare_frame(frame.area(), state, Instant::now());
            crate::ui::draw(frame, state, &prepared);
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// ANSI-16 and mono: the selected row is one uniform bar between the frame's
/// borders — the header's section style, the status badge and its words all
/// give way to the bar's single style.
#[test]
fn a_non_rgb_selection_is_one_uniform_bar() {
    for mode in ["ansi", "mono"] {
        let (mut state, _dir) = state();
        state.config.ui.color = mode.to_string();
        state.apply_runtime_config("ui.color");
        state.auth_providers = vec![provider("p", AuthState::Configured, &["one"])];
        open(&mut state);
        // The provider header (title, badge, words, kind) is the busiest row.
        press(&mut state, key(KeyCode::Char('p')));
        let buffer = draw_buffer(&mut state, 120, 34);
        let area = buffer.area;
        let pointer = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == "▸")
            .expect("a selected row");
        let (left, y) = (pointer.0 - 1, pointer.1);
        assert_eq!(
            buffer[(left, y)].symbol(),
            "│",
            "{mode}: the frame's left border"
        );
        let right = (pointer.0..area.width)
            .find(|&x| buffer[(x, y)].symbol() == "│")
            .expect("the frame's right border");
        let row: String = (pointer.0..right)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        assert!(row.contains("signed in"), "{mode}: {row}");
        let bar = buffer[pointer].style();
        for x in pointer.0..right {
            assert_eq!(buffer[(x, y)].style(), bar, "{mode}: column {x} of {row}");
        }
    }
}

/// Moving inside the visible rows leaves the list where it is; the frame
/// keeps the window between key presses.
#[test]
fn the_list_holds_still_while_the_selection_moves_inside_it() {
    let (mut state, _dir) = state();
    state.models = (0..30)
        .map(|n| model("p", &format!("m{n:02}"), None))
        .collect();
    open(&mut state);
    draw(&mut state, 60, 20);
    let mut last = 0;
    for _ in 0..25 {
        press(&mut state, key(KeyCode::Down));
        draw(&mut state, 60, 20);
        last = palette(&state).scroll;
    }
    assert!(last > 0, "the list scrolled");
    let before = draw(&mut state, 60, 20);
    press(&mut state, key(KeyCode::Up));
    let after = draw(&mut state, 60, 20);
    assert_eq!(palette(&state).scroll, last, "no shift on a move inside");
    let names = |view: &str| -> Vec<String> {
        view.lines()
            .filter_map(|line| line.find(" m").map(|at| line[at + 1..at + 4].to_string()))
            .collect()
    };
    assert_eq!(names(&before), names(&after));
}
