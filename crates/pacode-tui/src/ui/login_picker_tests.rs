use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use pacode_render::RenderOptions;
use pacode_types::{AuthState, Config, ProviderAuthInfo};

use crate::state::AppState;
use crate::ui::login_picker::draw;

fn make_state() -> AppState {
    let config = Config::default();
    AppState::new(config, "0.1.0".into(), 100, 30)
}

fn render_picker(state: &AppState, query: &str, selected: usize, ascii_only: bool) -> String {
    let opts = RenderOptions::new(100, ascii_only);
    let backend = TestBackend::new(100, 13);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let area = Rect::new(0, 0, 100, 13);
    terminal
        .draw(|f| draw(f, area, state, query, selected, &opts))
        .expect("draw");
    format!("{}", terminal.backend())
}

#[test]
fn test_render_three_status_states_unicode() {
    let mut state = make_state();
    state.auth_providers = vec![
        ProviderAuthInfo {
            id: "devin".into(),
            display_name: "Devin".into(),
            auth_kind: "oauth".into(),
            detail: "Autonomous AI software engineer".into(),
            recommended: true,
            state: AuthState::Configured,
            accounts: vec!["acc1".into(), "acc2".into()],
            active: Some("acc1".into()),
        },
        ProviderAuthInfo {
            id: "openai".into(),
            display_name: "OpenAI".into(),
            auth_kind: "api_key".into(),
            detail: "GPT-4o and reasoning models".into(),
            recommended: false,
            state: AuthState::NeedsAttention {
                reason: "key expired".into(),
            },
            accounts: vec!["primary".into()],
            active: Some("primary".into()),
        },
        ProviderAuthInfo {
            id: "anthropic".into(),
            display_name: "Anthropic Claude".into(),
            auth_kind: "oauth".into(),
            detail: "Claude 3.5 models".into(),
            recommended: false,
            state: AuthState::NotConfigured,
            accounts: vec![],
            active: None,
        },
    ];

    let output = render_picker(&state, "", 0, false);

    // Title and query
    assert!(output.contains("Sign in to Provider"), "{output}");
    assert!(output.contains("> "), "{output}");

    // All three providers rendered with their names and auth kinds
    assert!(output.contains("Devin"), "{output}");
    assert!(output.contains("[oauth]"), "{output}");
    assert!(output.contains("OpenAI"), "{output}");
    assert!(output.contains("[api_key]"), "{output}");
    assert!(output.contains("Anthropic Claude"), "{output}");

    // Glyphs for the three status states
    // Configured -> ✓
    assert!(
        output.contains('✓'),
        "expected ✓ for Configured in: {output}"
    );
    // NeedsAttention -> ✗
    assert!(
        output.contains('✗'),
        "expected ✗ for NeedsAttention in: {output}"
    );
    // NotConfigured -> ⊘
    assert!(
        output.contains('⊘'),
        "expected ⊘ for NotConfigured in: {output}"
    );

    // Detail line shows selected provider's detail, accounts and active account
    assert!(
        output.contains("Autonomous AI software engineer"),
        "{output}"
    );
    assert!(output.contains("*acc1 (active)"), "{output}");
    assert!(output.contains("acc2"), "{output}");

    // Key hints line includes 'a' because devin has 2 accounts
    assert!(output.contains("a switch account"), "{output}");
    assert!(output.contains("ctrl+d logout"), "{output}");
    assert!(output.contains("enter login"), "{output}");
    assert!(output.contains("esc cancel"), "{output}");
}

#[test]
fn test_render_three_status_states_ascii() {
    let mut state = make_state();
    state.config.ui.ascii_only = true;
    state.auth_providers = vec![
        ProviderAuthInfo {
            id: "devin".into(),
            display_name: "Devin".into(),
            auth_kind: "oauth".into(),
            detail: "Autonomous AI software engineer".into(),
            recommended: true,
            state: AuthState::Configured,
            accounts: vec!["acc1".into()],
            active: Some("acc1".into()),
        },
        ProviderAuthInfo {
            id: "openai".into(),
            display_name: "OpenAI".into(),
            auth_kind: "api_key".into(),
            detail: "GPT-4o and reasoning models".into(),
            recommended: false,
            state: AuthState::NeedsAttention {
                reason: "key expired".into(),
            },
            accounts: vec![],
            active: None,
        },
        ProviderAuthInfo {
            id: "anthropic".into(),
            display_name: "Anthropic Claude".into(),
            auth_kind: "oauth".into(),
            detail: "Claude 3.5 models".into(),
            recommended: false,
            state: AuthState::NotConfigured,
            accounts: vec![],
            active: None,
        },
    ];

    let output = render_picker(&state, "", 0, true);

    // ASCII Glyphs for the three status states
    // Configured -> +
    assert!(
        output.contains('+'),
        "expected + for Configured in: {output}"
    );
    // NeedsAttention -> x
    assert!(
        output.contains('x'),
        "expected x for NeedsAttention in: {output}"
    );
    // NotConfigured -> o
    assert!(
        output.contains('o'),
        "expected o for NotConfigured in: {output}"
    );

    // Since devin only has 1 account, 'a switch account' is not in the hints
    assert!(!output.contains("a switch account"), "{output}");
    assert!(output.contains("ctrl+d logout"), "{output}");
    assert!(output.contains("enter login"), "{output}");
}

#[test]
fn test_render_empty_and_no_match() {
    let mut state = make_state();

    // 1. Empty providers -> loading
    let output = render_picker(&state, "", 0, false);
    assert!(output.contains("loading"), "{output}");

    // 2. Providers exist, but query filters them all out
    state.auth_providers = vec![ProviderAuthInfo {
        id: "devin".into(),
        display_name: "Devin".into(),
        auth_kind: "oauth".into(),
        detail: "Autonomous AI software engineer".into(),
        recommended: false,
        state: AuthState::Configured,
        accounts: vec![],
        active: None,
    }];
    let output = render_picker(&state, "nonexistent", 0, false);
    assert!(output.contains("no matching providers"), "{output}");
    assert!(output.contains("backspace to clear"), "{output}");
}
