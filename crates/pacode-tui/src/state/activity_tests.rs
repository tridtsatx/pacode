use super::*;

fn glyphs() -> Glyphs {
    Glyphs::new(false)
}

#[test]
fn thinking_wording_escalates_at_10_20_and_30_seconds() {
    // The "..." step of the dots cycle sits at frame `2 * DOT_DIVIDER`,
    // matching the old static text.
    let f = 2 * DOT_DIVIDER;
    assert_eq!(thinking_label(0, f, &glyphs()), "thinking...");
    assert_eq!(thinking_label(9_900, f, &glyphs()), "thinking...");
    assert_eq!(
        thinking_label(10_000, f, &glyphs()),
        "thinking a bit more..."
    );
    assert_eq!(
        thinking_label(19_900, f, &glyphs()),
        "thinking a bit more..."
    );
    assert_eq!(thinking_label(20_000, f, &glyphs()), "thinking a lot...");
    assert_eq!(thinking_label(29_900, f, &glyphs()), "thinking a lot...");
    assert_eq!(
        thinking_label(30_000, f, &glyphs()),
        "almost done thinking..."
    );
    assert_eq!(
        thinking_label(120_000, f, &glyphs()),
        "almost done thinking..."
    );
}

#[test]
fn thinking_dots_cycle_with_the_frame() {
    // One dot step every DOT_DIVIDER frames; the suffix holds still between
    // steps so the ~15 fps tick does not strobe it.
    assert_eq!(thinking_label(0, 0, &glyphs()), "thinking.  ");
    assert_eq!(thinking_label(0, DOT_DIVIDER - 1, &glyphs()), "thinking.  ");
    assert_eq!(thinking_label(0, DOT_DIVIDER, &glyphs()), "thinking.. ");
    assert_eq!(thinking_label(0, 2 * DOT_DIVIDER, &glyphs()), "thinking...");
    assert_eq!(thinking_label(0, 3 * DOT_DIVIDER, &glyphs()), "thinking.  ");

    // §9.2: the suffix is 3 cells wide in both glyph sets — `…` never
    // appears in the animation.
    let ascii = Glyphs::new(true);
    for frame in 0..9 * DOT_DIVIDER {
        assert_eq!(
            pacode_render::display_width(trailing_dots(frame, &ascii)),
            3,
            "ascii frame {frame}"
        );
        assert_eq!(
            pacode_render::display_width(trailing_dots(frame, &glyphs())),
            3,
            "unicode frame {frame}"
        );
        assert!(!trailing_dots(frame, &glyphs()).contains('…'));
    }
    assert_eq!(trailing_dots(0, &ascii), ".  ");
    assert_eq!(trailing_dots(DOT_DIVIDER, &ascii), ".. ");
    assert_eq!(trailing_dots(2 * DOT_DIVIDER, &ascii), "...");
}

#[test]
fn thinking_label_keeps_the_same_display_width_on_every_frame() {
    // §9.2: whatever to the right of the label must not move between frames.
    for elapsed in [0_u64, 9_900, 15_000, 25_000, 45_000, 120_000] {
        let width = pacode_render::display_width(&thinking_label(elapsed, 0, &glyphs()));
        for frame in 1..12 {
            assert_eq!(
                pacode_render::display_width(&thinking_label(elapsed, frame, &glyphs())),
                width,
                "elapsed {elapsed} frame {frame}"
            );
        }
    }
}

#[test]
fn thinking_colour_drifts_to_yellow_and_stops_there() {
    let white = Color::Rgb(220, 220, 220);
    let yellow = Color::Rgb(255, 200, 0);

    assert_eq!(thinking_color(0, white, yellow), white);
    assert_eq!(thinking_color(60_000, white, yellow), yellow);
    assert_eq!(thinking_color(600_000, white, yellow), yellow);

    let Color::Rgb(r, g, b) = thinking_color(30_000, white, yellow) else {
        panic!("expected an rgb colour");
    };
    // Halfway: between the two ends on every channel.
    assert!((236..=239).contains(&r), "r={r}");
    assert!((209..=211).contains(&g), "g={g}");
    assert!((109..=111).contains(&b), "b={b}");
}

#[test]
fn thinking_colour_without_rgb_flips_at_the_last_threshold() {
    let from = Color::White;
    let to = Color::Yellow;
    assert_eq!(thinking_color(0, from, to), from);
    assert_eq!(thinking_color(29_999, from, to), from);
    assert_eq!(thinking_color(30_000, from, to), to);
}

#[test]
fn each_phase_words_itself() {
    let g = glyphs();
    let f = 2 * DOT_DIVIDER;
    assert_eq!(Phase::Thinking.label(0, f, &g), "thinking...");
    assert_eq!(Phase::Thinking.label(25_000, f, &g), "thinking a lot...");
    assert_eq!(Phase::Responding.label(99_000, 0, &g), "responding…");
    assert_eq!(
        Phase::Responding.label(99_000, 0, &Glyphs::new(true)),
        "responding..."
    );
    assert_eq!(
        Phase::Tool("write src/foo.rs".to_string()).label(0, 0, &g),
        "write src/foo.rs"
    );
}

#[test]
fn the_displayed_duration_is_floored_so_redraws_do_not_change_it() {
    assert_eq!(displayed_secs(0), 0);
    assert_eq!(displayed_secs(400), 0);
    assert_eq!(displayed_secs(999), 0);
    assert_eq!(displayed_secs(1_000), 1);
    assert_eq!(displayed_secs(1_999), 1);
    assert_eq!(displayed_secs(54_321), 54);
}

#[test]
fn the_tick_is_armed_to_the_next_boundary_not_a_full_second() {
    assert_eq!(ms_to_next_second(0), 1000);
    assert_eq!(ms_to_next_second(400), 600);
    assert_eq!(ms_to_next_second(999), 1);
    assert_eq!(ms_to_next_second(1_000), 1000);
    assert_eq!(ms_to_next_second(54_321), 679);
}

#[test]
fn the_duration_text_is_whole_seconds_then_minutes() {
    assert_eq!(format_activity_secs(0), "0s");
    assert_eq!(format_activity_secs(9), "9s");
    assert_eq!(format_activity_secs(59), "59s");
    assert_eq!(format_activity_secs(60), "1m00s");
    assert_eq!(format_activity_secs(125), "2m05s");
}

#[test]
fn only_long_or_failed_background_jobs_earn_a_transcript_line() {
    use crate::state::{BackgroundOutcome, background_worth_reporting};

    assert!(!background_worth_reporting(BackgroundOutcome::Completed, 0));
    assert!(!background_worth_reporting(
        BackgroundOutcome::Completed,
        29_999
    ));
    assert!(background_worth_reporting(
        BackgroundOutcome::Completed,
        30_000
    ));
    // A job that failed is reported however briefly it ran.
    assert!(background_worth_reporting(BackgroundOutcome::Failed, 5));
    assert!(background_worth_reporting(BackgroundOutcome::Killed, 5));
}

#[test]
fn a_selected_subagent_replaces_the_chat_by_default_and_splits_when_configured() {
    use crate::state::{AppState, Focus, PanelTarget};
    use pacode_types::config::AgentView;
    use pacode_types::{AgentId, Config};

    let mut state = AppState::new(Config::default(), "0.0.0".into(), 100, 30);
    assert_eq!(state.config.ui.agent_view, AgentView::Replace);
    // Nothing selected: the main chat owns the column.
    assert!(!state.agent_replaces_dialog());

    state.focus = Focus::Panel {
        target: PanelTarget::Agent(AgentId::new("agt_1")),
        follow: false,
        follow_paused: false,
    };
    assert!(state.agent_replaces_dialog());

    state.config.ui.agent_view = AgentView::Panel;
    assert!(!state.agent_replaces_dialog());
}
