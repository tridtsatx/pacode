use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use tempfile::TempDir;

use pacode_render::{RenderOptions, display_width};
use pacode_types::{
    AuthState, Config, Effort, ModelInfo, ModelKind, ModelRoute, Pricing, ProviderAuthInfo,
    Request, SessionId, SessionMeta,
};

use super::*;
use crate::keys::Action;
use crate::state::model_picker::{SIDE_ALL, SIDE_ROLES, fuzzy_score};
use crate::state::{Focus, Overlay};
use crate::ui::model_picker_keys::{assign, handle_key, open, set_effort};

fn model(provider: &str, id: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider, id),
        display_name: id.to_string(),
        context_window: Some(131_072),
        supports_reasoning: true,
        efforts: vec![Effort::Low, Effort::Medium, Effort::High],
        ..ModelInfo::default()
    }
}

fn priced(mut m: ModelInfo, input: f64, output: f64) -> ModelInfo {
    m.pricing = Some(Pricing {
        input_per_m: input,
        output_per_m: output,
        cache_read_per_m: None,
        cache_write_per_m: None,
    });
    m
}

fn catalog() -> Vec<ModelInfo> {
    let mut gpt = priced(model("openai", "gpt-5"), 1.25, 10.0);
    gpt.context_window = Some(1_048_576);
    gpt.max_output = Some(66_000);
    gpt.vision = true;
    let o3 = priced(model("openai", "o3"), 2.0, 8.0);
    let claude = priced(model("openrouter", "anthropic/claude-sonnet"), 3.0, 15.0);
    let mut llama = model("ollama", "llama3");
    llama.supports_reasoning = false;
    llama.efforts.clear();
    llama.pricing = Some(Pricing::default());
    let mut img = model("openrouter", "google/imagen-4");
    img.kind = ModelKind::Image;
    img.supports_reasoning = false;
    img.efforts.clear();
    vec![gpt, o3, claude, llama, img]
}

fn auth(id: &str, configured: bool) -> ProviderAuthInfo {
    ProviderAuthInfo {
        id: id.to_string(),
        display_name: id.to_string(),
        auth_kind: "api_key".to_string(),
        detail: String::new(),
        recommended: false,
        state: if configured {
            AuthState::Configured
        } else {
            AuthState::NotConfigured
        },
        accounts: Vec::new(),
        active: None,
    }
}

fn state_with(models: Vec<ModelInfo>) -> (AppState, TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    state.paths = pacode_config::Paths::under(tmp.path());
    state.models = models;
    state.auth_providers = vec![auth("anthropic", false), auth("openai", true)];
    set_session(
        &mut state,
        ModelRoute::new("openai", "gpt-5"),
        Effort::Medium,
    );
    (state, tmp)
}

fn set_session(state: &mut AppState, model: ModelRoute, effort: Effort) {
    state.meta = Some(SessionMeta {
        id: SessionId::new("ses_1"),
        name: None,
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model,
        effort,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
    });
}

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn view_keys(state: &AppState) -> Vec<String> {
    let mp = &state.model_picker;
    mp.view
        .iter()
        .map(|&i| mp.entries[i as usize].key.clone())
        .collect()
}

#[test]
fn fuzzy_prefers_contiguous_and_rejects_non_subsequences() {
    assert!(fuzzy_score("gpt5", "openai/gpt-5").is_some());
    assert!(fuzzy_score("zzz", "openai/gpt-5").is_none());
    let contiguous = fuzzy_score("gpt", "openai/gpt-5").unwrap_or(0);
    let scattered = fuzzy_score("gpt", "grand-pt-test").unwrap_or(0);
    assert!(contiguous > scattered);
    assert_eq!(fuzzy_score("", "anything"), Some(0));
}

#[test]
fn empty_query_is_sorted_and_recents_come_first() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    let all = view_keys(&state);
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted);
    assert_eq!(state.model_picker.recent_rows, 0);

    state
        .model_picker
        .touch_recent(&ModelRoute::new("openrouter", "anthropic/claude-sonnet"));
    state.model_picker.recompute(&state.models);
    let keys = view_keys(&state);
    assert_eq!(keys[0], "openrouter/anthropic/claude-sonnet");
    assert_eq!(state.model_picker.recent_rows, 1);
    assert_eq!(keys.len(), all.len(), "a recent model is not listed twice");
}

#[test]
fn typing_filters_on_the_fly_and_ranks_best_first() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    for c in "gpt".chars() {
        handle_key(&mut state, key(KeyCode::Char(c)));
    }
    assert_eq!(view_keys(&state), vec!["openai/gpt-5".to_string()]);
    handle_key(&mut state, key(KeyCode::Backspace));
    handle_key(&mut state, key(KeyCode::Backspace));
    handle_key(&mut state, key(KeyCode::Backspace));
    assert_eq!(state.model_picker.view.len(), 5);
}

#[test]
fn sidebar_groups_configured_providers_first_with_counts() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    let groups = &state.model_picker.groups;
    let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, vec!["ollama", "openai", "openrouter", "anthropic"]);
    assert_eq!(groups[1].count, 2);
    assert!(groups[2].configured);
    assert!(!groups[3].configured);
    assert_eq!(groups[3].count, 0);

    // Down from "All models" selects the first provider and filters to it.
    assert_eq!(state.model_picker.side, SIDE_ALL);
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(view_keys(&state), vec!["ollama/llama3".to_string()]);
    handle_key(&mut state, key(KeyCode::Down));
    assert_eq!(view_keys(&state).len(), 2);
    handle_key(&mut state, key(KeyCode::Up));
    handle_key(&mut state, key(KeyCode::Up));
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.model_picker.side, SIDE_ROLES);
}

#[test]
fn kind_filter_cycles_through_present_kinds() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    assert_eq!(
        state.model_picker.kinds,
        vec![ModelKind::Chat, ModelKind::Image]
    );
    let alt_right = KeyEvent::new(KeyCode::Right, KeyModifiers::ALT);
    handle_key(&mut state, alt_right);
    assert_eq!(state.model_picker.kind, Some(ModelKind::Chat));
    assert_eq!(state.model_picker.view.len(), 4);
    handle_key(&mut state, alt_right);
    assert_eq!(state.model_picker.kind, Some(ModelKind::Image));
    assert_eq!(
        view_keys(&state),
        vec!["openrouter/google/imagen-4".to_string()]
    );
    handle_key(&mut state, alt_right);
    assert_eq!(state.model_picker.kind, None);
    handle_key(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(state.model_picker.kind, Some(ModelKind::Image));
}

fn highlight(state: &mut AppState, route: &ModelRoute) {
    state.model_picker.select_route(route, &state.models);
}

#[test]
fn enter_assigns_the_default_role_like_slash_model() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    highlight(&mut state, &ModelRoute::new("openai", "o3"));
    let actions = handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(
        actions,
        vec![Action::Send(Request::SetModel(ModelRoute::new(
            "openai", "o3"
        )))]
    );
    assert_eq!(state.focus, Focus::Normal);
    assert_eq!(state.model_picker.recent[0], "openai/o3");
    let prefs = pacode_config::load_prefs(&state.paths);
    assert_eq!(prefs.model.as_deref(), Some("openai/o3"));
    assert_eq!(prefs.recent_models, vec!["openai/o3".to_string()]);
}

#[test]
fn role_key_cycles_and_assignments_write_their_config_keys() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    highlight(&mut state, &ModelRoute::new("openai", "o3"));

    handle_key(&mut state, key(KeyCode::Char('r')));
    assert_eq!(state.model_picker.role, PickerRole::Agents);
    let actions = handle_key(&mut state, key(KeyCode::Enter));
    assert!(actions.is_empty());
    assert_eq!(
        state.config.agents.default_model.as_deref(),
        Some("openai/o3")
    );
    assert!(matches!(
        state.focus,
        Focus::Overlay(Overlay::ModelPicker { .. })
    ));

    handle_key(&mut state, key(KeyCode::Char('r')));
    assert_eq!(state.model_picker.role, PickerRole::Compaction);
    handle_key(&mut state, key(KeyCode::Enter));
    assert_eq!(
        state.config.context.compaction_model.as_deref(),
        Some("openai/o3")
    );

    let written = std::fs::read_to_string(&state.paths.config_file).unwrap_or_default();
    assert!(written.contains("default_model"), "config was: {written}");
    assert!(
        written.contains("compaction_model"),
        "config was: {written}"
    );

    handle_key(&mut state, key(KeyCode::Char('r')));
    assert_eq!(state.model_picker.role, PickerRole::Main);
}

#[test]
fn r_types_into_the_search_once_a_query_exists() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    handle_key(&mut state, key(KeyCode::Char('o')));
    handle_key(&mut state, key(KeyCode::Char('r')));
    assert_eq!(state.model_picker.query, "or");
    assert_eq!(state.model_picker.role, PickerRole::Main);
}

#[test]
fn roles_view_enter_picks_the_role_then_the_list() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    handle_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.model_picker.side, SIDE_ROLES);
    handle_key(&mut state, key(KeyCode::Tab));
    let actions = handle_key(&mut state, key(KeyCode::Enter));
    assert!(actions.is_empty());
    assert_eq!(state.model_picker.role, PickerRole::Agents);
    assert_eq!(state.model_picker.side, SIDE_ALL);
}

#[test]
fn changing_model_clamps_an_unsupported_effort_with_a_toast() {
    let mut models = catalog();
    models[1].efforts = vec![Effort::Low, Effort::High, Effort::Max];
    let (mut state, _tmp) = state_with(models);
    set_session(
        &mut state,
        ModelRoute::new("openai", "gpt-5"),
        Effort::Medium,
    );
    let o3 = ModelRoute::new("openai", "o3");
    let actions = assign(&mut state, PickerRole::Main, &o3);
    assert_eq!(
        actions,
        vec![
            Action::Send(Request::SetModel(o3)),
            Action::Send(Request::SetEffort(Effort::Low)),
        ]
    );
    let toast = state
        .toasts
        .back()
        .map(|t| t.title.clone())
        .unwrap_or_default();
    assert_eq!(toast, "effort set to low (model supports low, high, max)");
}

#[test]
fn model_without_efforts_leaves_the_effort_alone() {
    let (mut state, _tmp) = state_with(catalog());
    let llama = ModelRoute::new("ollama", "llama3");
    let actions = assign(&mut state, PickerRole::Main, &llama);
    assert_eq!(actions, vec![Action::Send(Request::SetModel(llama))]);
    assert!(state.toasts.is_empty());
}

#[test]
fn agents_role_clamps_the_configured_subagent_effort() {
    let mut models = catalog();
    models[1].efforts = vec![Effort::Low, Effort::High];
    let (mut state, _tmp) = state_with(models);
    state.config.agents.default_effort = Some(Effort::Medium);
    assign(
        &mut state,
        PickerRole::Agents,
        &ModelRoute::new("openai", "o3"),
    );
    assert_eq!(state.config.agents.default_effort, Some(Effort::Low));
}

#[test]
fn set_effort_clamps_to_the_current_model() {
    let mut models = catalog();
    models[0].efforts = vec![Effort::Low, Effort::High, Effort::Max];
    let (mut state, _tmp) = state_with(models);
    assert_eq!(
        set_effort(&mut state, Effort::XHigh),
        vec![Action::Send(Request::SetEffort(Effort::High))]
    );
    assert_eq!(
        set_effort(&mut state, Effort::Max),
        vec![Action::Send(Request::SetEffort(Effort::Max))]
    );
}

#[test]
fn effort_levels_follow_the_model() {
    let mut models = catalog();
    models[0].efforts = vec![Effort::Low, Effort::High];
    let (mut state, _tmp) = state_with(models);
    assert_eq!(state.effort_levels(), vec![Effort::Low, Effort::High]);
    assert!(state.effort_visible());
    set_session(
        &mut state,
        ModelRoute::new("ollama", "llama3"),
        Effort::Medium,
    );
    assert!(!state.effort_visible());
    set_session(&mut state, ModelRoute::new("unknown", "x"), Effort::Medium);
    assert!(state.effort_visible(), "unlisted model: nothing is hidden");
    assert_eq!(state.effort_levels().len(), 5);
}

#[test]
fn number_formats() {
    assert_eq!(fmt_ctx(1_048_576), "1m");
    assert_eq!(fmt_ctx(2_000_000), "2m");
    assert_eq!(fmt_ctx(1_500_000), "1.5m");
    assert_eq!(fmt_ctx(131_072), "131k");
    assert_eq!(fmt_ctx(512), "512");
    assert_eq!(fmt_price(&Pricing::default()), "free");
    let p = Pricing {
        input_per_m: 0.5,
        output_per_m: 1.5,
        cache_read_per_m: None,
        cache_write_per_m: None,
    };
    assert_eq!(fmt_price(&p), "$0.5/1.5");
}

fn lines_at(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let opts = RenderOptions::new(width, false);
    render_lines(state, width, height, &opts)
        .iter()
        .map(text)
        .collect()
}

#[test]
fn layout_at_80_and_120_columns() {
    for outer in [80u16, 120] {
        let inner = outer - 2;
        let (mut state, _tmp) = state_with(catalog());
        open(&mut state);
        let lines = lines_at(&state, inner, 20);
        assert_eq!(lines.len(), 20);
        for l in &lines {
            assert_eq!(
                display_width(l),
                inner as usize,
                "ragged line at {outer}: {l:?}"
            );
        }
        assert!(lines[0].contains("Roles"), "{}", lines[0]);
        assert!(lines[1].contains("All models (5)"), "{}", lines[1]);
        assert!(lines.iter().any(|l| l.contains("● ollama")));
        assert!(lines.iter().any(|l| l.contains("○ anthropic")));
        assert!(
            lines[0].contains("Kind: all · chat · image"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("> type to search"), "{}", lines[1]);
        let gpt = lines
            .iter()
            .find(|l| l.contains("openai/gpt-5"))
            .expect("gpt row");
        assert!(gpt.contains("1m"), "{gpt}");
        assert!(gpt.contains("$1.25/10"), "{gpt}");
        assert!(gpt.contains('R') && gpt.contains('V'), "{gpt}");
        let free = lines
            .iter()
            .find(|l| l.contains("ollama/llama3"))
            .expect("llama row");
        assert!(free.contains("free"), "{free}");

        let n = lines.len();
        assert!(
            lines[n - 2].contains("gpt-5 · 1m ctx · 66k out · $1.25/10 per M"),
            "{}",
            lines[n - 2]
        );
        assert!(lines[n - 2].contains("· reasoning"), "{}", lines[n - 2]);
        assert!(lines[n - 2].contains("· vision"), "{}", lines[n - 2]);
        assert!(lines[n - 1].contains("● default"), "{}", lines[n - 1]);
        assert!(lines[n - 1].contains("○ agents"), "{}", lines[n - 1]);
        assert!(lines[n - 1].contains("○ compaction"), "{}", lines[n - 1]);
    }
}

#[test]
fn recents_are_separated_by_a_rule() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    state
        .model_picker
        .touch_recent(&ModelRoute::new("openai", "o3"));
    state.model_picker.recompute(&state.models);
    let lines = lines_at(&state, 100, 20);
    let first = lines
        .iter()
        .position(|l| l.contains("openai/o3"))
        .expect("recent row");
    assert!(lines[first + 1].contains('─'), "{}", lines[first + 1]);
}

#[test]
fn narrow_width_drops_the_sidebar_and_stays_aligned() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    let lines = lines_at(&state, 50, 16);
    assert!(lines[0].starts_with("Kind: all"), "{}", lines[0]);
    for l in &lines {
        assert_eq!(display_width(l), 50, "{l:?}");
    }
}

#[test]
fn role_strip_marks_roles_pointing_at_the_highlighted_model() {
    let (mut state, _tmp) = state_with(catalog());
    state.config.agents.default_model = Some("openai/o3".to_string());
    open(&mut state);
    highlight(&mut state, &ModelRoute::new("openai", "o3"));
    let lines = lines_at(&state, 100, 20);
    let strip = lines.last().cloned().unwrap_or_default();
    assert!(strip.contains("○ default"), "{strip}");
    assert!(strip.contains("● agents"), "{strip}");
    // Unset compaction follows the default model, which is gpt-5 here.
    assert!(strip.contains("○ compaction"), "{strip}");
}

#[test]
fn roles_pane_lists_each_role_and_its_target() {
    let (mut state, _tmp) = state_with(catalog());
    open(&mut state);
    handle_key(&mut state, key(KeyCode::Up));
    let lines = lines_at(&state, 100, 12);
    let joined = lines.join("\n");
    assert!(joined.contains("default"), "{joined}");
    assert!(joined.contains("agents"), "{joined}");
    assert!(
        joined.contains("openai/gpt-5 (follows default)"),
        "{joined}"
    );
}

#[test]
fn a_thousand_models_filter_and_render_only_the_window() {
    let mut models = Vec::new();
    for i in 0..1500 {
        models.push(model(&format!("prov{}", i % 7), &format!("model-{i}")));
    }
    let (mut state, _tmp) = state_with(models);
    open(&mut state);
    assert_eq!(state.model_picker.view.len(), 1500);
    for c in "model-14".chars() {
        handle_key(&mut state, key(KeyCode::Char(c)));
    }
    // The 111 substring matches rank above any scattered match.
    assert!(state.model_picker.view.len() >= 111);
    assert!(
        view_keys(&state)[..111]
            .iter()
            .all(|k| k.contains("model-14"))
    );
    handle_key(&mut state, key(KeyCode::End));
    let lines = lines_at(&state, 100, 20);
    assert_eq!(lines.len(), 20);
}
