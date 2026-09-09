use pacode_render::RenderOptions;
use pacode_types::Config;

use super::*;
use crate::state::AppState;

fn make_test_state() -> AppState {
    let config = Config::default();
    AppState::new(config, "0.1.0".into(), 120, 34)
}

#[test]
fn test_footer_render_plugin_status() {
    let mut state = make_test_state();
    let opts = RenderOptions::new(120, false);

    // 1. Without plugin_status
    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row1_text.contains("git-sync"));

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!row2_text.contains("git-sync"));

    // 2. With plugin_status
    state.plugin_status = Some(("git".into(), "git-sync: rebasing".into()));

    let row1 = render_row1(120, &state, &opts);
    let row1_text: String = row1.spans.iter().map(|s| s.content.as_ref()).collect();
    // Mode label is followed by " · git-sync: rebasing"
    assert!(
        row1_text.contains("Build · git-sync: rebasing"),
        "row1_text was: {row1_text}"
    );

    let row2 = render_row2(120, &state, &opts);
    let row2_text: String = row2.spans.iter().map(|s| s.content.as_ref()).collect();
    // Chevrons + permission line followed by " · git-sync: rebasing"
    assert!(
        row2_text.contains(" · git-sync: rebasing"),
        "row2_text was: {row2_text}"
    );
    assert!(row2_text.contains("ask before edits and commands"));
}
