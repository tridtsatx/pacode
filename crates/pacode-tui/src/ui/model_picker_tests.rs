use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use pacode_render::{ColorMode, RenderOptions, Theme};
use pacode_types::{Effort, ModelInfo, ModelRoute, Pricing};

use super::*;

fn opts() -> RenderOptions {
    RenderOptions::with_theme_color(100, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn model(provider: &str, id: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider.to_string(), id.to_string()),
        display_name: id.to_string(),
        context_window: Some(200_000),
        supports_reasoning: true,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }
}

fn with_efforts(mut info: ModelInfo, efforts: &[Effort]) -> ModelInfo {
    info.supported_efforts = Some(efforts.to_vec());
    info
}

fn flags() -> Flags {
    Flags::new()
}

fn area() -> Rect {
    Rect::new(0, 0, 100, 24)
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn lines_text(picker: &ModelPicker, models: &[ModelInfo], flags: &Flags) -> Vec<String> {
    lines(picker, models, flags, area(), &opts())
        .iter()
        .map(line_text)
        .collect()
}

/// The model rows: everything between the `Kind:` filter and the detail line.
fn row_texts(picker: &ModelPicker, models: &[ModelInfo], flags: &Flags) -> Vec<String> {
    let text = lines_text(picker, models, flags);
    let start = text
        .iter()
        .position(|line| line.starts_with("Kind: "))
        .map_or(0, |index| index + 1);
    let end = text
        .iter()
        .position(|line| line.starts_with("effort  "))
        .unwrap_or(text.len());
    text[start..end]
        .iter()
        .filter(|line| !line.trim().is_empty())
        .cloned()
        .collect()
}

/// The line under the two columns: what the highlighted model is, in full.
fn detail_text(picker: &ModelPicker, models: &[ModelInfo], flags: &Flags) -> String {
    lines_text(picker, models, flags)
        .into_iter()
        .find(|line| line.trim_start().starts_with("detail"))
        .unwrap_or_default()
}

/// The effort row is the last-but-one line; its chips are the spans after the
/// `effort  ` label, in ladder order.
fn effort_chips(picker: &ModelPicker, models: &[ModelInfo], flags: &Flags) -> Vec<(Effort, Style)> {
    let rendered = lines(picker, models, flags, area(), &opts());
    let row = rendered
        .iter()
        .rev()
        .nth(1)
        .expect("the effort row is always drawn");
    row.spans
        .iter()
        .skip(1)
        .map(|span| {
            // The chip reads `<marker>low    `; the marker is a glyph under
            // `ascii_only = false`, so cut at the first ASCII letter.
            let effort = Effort::parse(
                span.content
                    .trim()
                    .trim_start_matches(|c: char| !c.is_ascii_alphanumeric()),
            )
            .unwrap_or_else(|| panic!("chip is not an effort: {:?}", span.content));
            (effort, span.style)
        })
        .collect()
}

#[test]
fn an_effort_outside_the_models_list_is_unselectable() {
    let opus = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Low, Effort::Medium, Effort::High],
    );
    let models = vec![opus];
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::Low);

    assert!(picker.select_effort(Effort::High, &models, &flags));
    assert_eq!(picker.effort, Effort::High);
    assert!(
        !picker.select_effort(Effort::XHigh, &models, &flags),
        "xhigh is outside the model's efforts"
    );
    assert!(
        !picker.select_effort(Effort::Max, &models, &flags),
        "max is outside the model's efforts"
    );
    assert_eq!(
        picker.effort,
        Effort::High,
        "a refused pick leaves the cursor where it was"
    );
}

#[test]
fn an_unavailable_effort_is_drawn_faint() {
    let opus = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Low, Effort::Medium, Effort::High],
    );
    let models = vec![opus];
    let opts = opts();
    let chips = effort_chips(&ModelPicker::new(Effort::High), &models, &flags());

    assert_eq!(chips.len(), Effort::ALL.len());
    for (effort, style) in chips {
        let available = [Effort::Low, Effort::Medium, Effort::High].contains(&effort);
        if available {
            assert_ne!(
                style, opts.theme.faint,
                "{effort} is available and must not be greyed out"
            );
        } else {
            assert_eq!(style, opts.theme.faint, "{effort} must be greyed out");
        }
    }
}

/// The selected chip is the only one wearing the accent: an unavailable level
/// can never be highlighted, because it can never be picked.
#[test]
fn an_unavailable_effort_is_never_highlighted() {
    let model = with_efforts(model("anthropic", "opus"), &[Effort::Low, Effort::Max]);
    let opts = opts();
    let mut picker = ModelPicker::new(Effort::Max);
    // Force the cursor onto a level the model rejects — a stale session effort
    // after a route change, say.
    picker.effort = Effort::XHigh;

    let chips = effort_chips(&picker, &[model], &flags());
    let selected: Vec<Effort> = chips
        .iter()
        .filter(|(_, style)| *style == opts.theme.bold.patch(opts.theme.accent))
        .map(|(effort, _)| *effort)
        .collect();
    assert!(
        selected.is_empty(),
        "no chip may be highlighted while the cursor sits on an unavailable one"
    );
}

#[test]
fn cycling_forward_lands_on_the_next_available_effort() {
    // medium and xhigh are missing, so `low` must step straight to `high`.
    let model = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Low, Effort::High, Effort::Max],
    );
    let mut picker = ModelPicker::new(Effort::Low);
    let models = vec![model];
    let flags = flags();

    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::High)
    );
    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::Max)
    );
}

#[test]
fn cycling_backward_lands_on_the_next_available_effort() {
    let model = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Low, Effort::High, Effort::Max],
    );
    let mut picker = ModelPicker::new(Effort::Max);
    let models = vec![model];
    let flags = flags();

    assert_eq!(
        picker.cycle_effort(false, &models, &flags),
        Some(Effort::High)
    );
    assert_eq!(
        picker.cycle_effort(false, &models, &flags),
        Some(Effort::Low)
    );
}

/// The wrap-around rule: past the last available level the cycle comes back to
/// the first *available* one, not to the first rung of the ladder. A picker
/// that wrapped to `Effort::ALL[0]` would land on `medium` — greyed out and
/// unpickable — for a model that never accepted it.
#[test]
fn cycling_wraps_to_the_first_available_effort_not_the_first_rung() {
    let opus = with_efforts(model("anthropic", "opus"), &[Effort::Medium, Effort::Max]);
    let mut picker = ModelPicker::new(Effort::Max);
    let models = vec![opus];
    let flags = flags();

    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::Medium),
        "wrap lands on the first supported level, skipping low"
    );

    let opus = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Medium, Effort::High, Effort::Max],
    );
    let models = vec![opus];
    let mut picker = ModelPicker::new(Effort::Max);
    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::Medium)
    );
    assert_eq!(
        picker.cycle_effort(false, &models, &flags),
        Some(Effort::Max),
        "and the same holds backwards"
    );
}

/// `None` means the catalog enumerated nothing, which is not the same as
/// "no effort works": every rung stays selectable and the cycle walks the whole
/// ladder. A "grey out whatever is not in the list" implementation gets exactly
/// this case wrong, and most of the catalog is in exactly this case.
#[test]
fn an_unknown_ladder_keeps_every_effort_available() {
    let model = model("bubna", "gemini-3.8-flash");
    assert!(model.supported_efforts.is_none());
    let models = vec![model];
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::Low);

    assert!(picker.select_effort(Effort::XHigh, &models, &flags));
    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::Max)
    );
    assert_eq!(
        picker.cycle_effort(true, &models, &flags),
        Some(Effort::Low)
    );
    assert_eq!(
        picker.cycle_effort(false, &models, &flags),
        Some(Effort::Max)
    );

    let opts = opts();
    for (effort, style) in effort_chips(&picker, &models, &flags) {
        assert_ne!(
            style, opts.theme.faint,
            "{effort} is unknown, not unsupported — it must not be greyed out"
        );
    }
}

/// A catalog that enumerated an empty set is the one case where the effort row
/// has nothing to offer: the cycle must report that rather than spin or fall
/// back to a level the catalog excluded.
#[test]
fn a_model_with_no_available_effort_cannot_cycle() {
    let model = with_efforts(model("openai", "gpt-4o"), &[]);
    let models = vec![model];
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::Medium);

    assert_eq!(picker.cycle_effort(true, &models, &flags), None);
    assert_eq!(picker.cycle_effort(false, &models, &flags), None);
    assert_eq!(picker.effort, Effort::Medium);
    assert!(
        !picker.select_effort(Effort::Low, &models, &flags),
        "nothing is selectable, low included"
    );
}

/// Arriving at a model whose ladder excludes the session's effort must not
/// leave the row pointing at a chip the user cannot pick.
#[test]
fn arriving_at_a_model_snaps_the_effort_onto_a_supported_level() {
    // Rows sort by display name, so the ids are spelled to keep the intended
    // order: `claude-opus` first, `gemini-flash` second.
    let opus = with_efforts(
        model("anthropic", "claude-opus"),
        &[Effort::Low, Effort::High],
    );
    let flash = with_efforts(
        model("bubna", "gemini-flash"),
        &[Effort::Medium, Effort::Max],
    );
    let models = vec![opus, flash];
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::XHigh);
    picker.row = 0;

    picker.move_row(1, &models, &flags);
    assert_eq!(picker.effort, Effort::Medium, "nearest level below xhigh");

    picker.move_row(-1, &models, &flags);
    assert_eq!(
        picker.effort,
        Effort::Low,
        "downgraded to the nearest level the new model accepts"
    );
}

#[test]
fn a_click_on_a_greyed_chip_changes_nothing() {
    let model = with_efforts(model("anthropic", "opus"), &[Effort::Low, Effort::Max]);
    let models = vec![model];
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::Low);

    let xhigh = 8 + 3 * 9; // `effort  ` label, then low / medium / high / xhigh
    assert_eq!(effort_under(xhigh as u16), Some(Effort::XHigh));
    assert!(!picker.select_effort(Effort::XHigh, &models, &flags));
    assert_eq!(picker.effort, Effort::Low);

    let max = 8 + 4 * 9;
    assert_eq!(effort_under(max as u16), Some(Effort::Max));
    assert!(picker.select_effort(Effort::Max, &models, &flags));
}

/// The click box is the effort row wherever the overlay sits on screen: a
/// picker opened at the bottom of the dialog must not need the row's y
/// hardcoded twice.
#[test]
fn a_click_maps_to_a_chip_from_the_overlay_rect() {
    let area = Rect::new(6, 40, 100, 24);
    let effort_row = 40 + 24 - 2;

    assert_eq!(effort_at(area, 6 + 8, effort_row), Some(Effort::Low));
    assert_eq!(
        effort_at(area, 6 + 8 + 2 * 9, effort_row),
        Some(Effort::High)
    );
    assert_eq!(
        effort_at(area, 6 + 8, effort_row - 1),
        None,
        "the detail line above it is not a chip"
    );
    assert_eq!(
        effort_at(area, 6 + 8, effort_row + 1),
        None,
        "the hint footer below it is not a chip either"
    );
    assert_eq!(effort_at(area, 6, effort_row), None, "left of the chips");
}

/// The chips keep their slots whatever the model supports, so a coordinate
/// means the same level on every model — the row must not reflow when the
/// highlighted model changes.
#[test]
fn effort_chips_keep_their_columns_across_models() {
    let opus = with_efforts(
        model("anthropic", "opus"),
        &[Effort::Low, Effort::High, Effort::Max],
    );
    let flash = model("bubna", "flash");
    let flags = flags();
    let mut picker = ModelPicker::new(Effort::High);
    let models = vec![opus.clone(), flash.clone()];

    let before = effort_chips(&picker, &models, &flags);
    picker.move_row(1, &models, &flags);
    let after = effort_chips(&picker, &models, &flags);

    assert_eq!(
        before.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
        after.iter().map(|(e, _)| *e).collect::<Vec<_>>()
    );
    assert_eq!(before.len(), Effort::ALL.len());
}

#[test]
fn a_click_left_of_the_chips_hits_nothing() {
    assert_eq!(effort_under(0), None);
    assert_eq!(effort_under(7), None);
    assert_eq!(effort_under(8), Some(Effort::Low));
    assert_eq!(effort_under(8 + 5 * 9), None, "past the last chip");
}

#[test]
fn a_priced_model_shows_its_rate_and_an_unpriced_one_shows_a_dash() {
    let mut expensive = model("anthropic", "opus");
    expensive.display_name = "Claude Opus 5".to_string();
    expensive.supports_vision = true;
    expensive.supported_efforts = Some(vec![Effort::Low, Effort::High]);
    expensive.pricing = Some(Pricing {
        input_per_m: 5.0,
        output_per_m: 25.0,
        cache_read_per_m: None,
        cache_write_per_m: None,
    });

    let mut free = model("bubna", "gemini-3.8-flash");
    free.display_name = "Gemini 3.8 Flash".to_string();
    free.pricing = Some(Pricing::default());

    let unknown = model("local", "mystery");
    let models = vec![expensive, free, unknown];
    let flags = flags();

    // `row_texts` is the whole list, so the row under the cursor has to be
    // indexed by `picker.row` — taking the first row three times checked the
    // same model three times.
    let row_at = |picker: &ModelPicker, row: usize| {
        let mut rows = row_texts(picker, &models, &flags);
        rows.remove(row)
    };

    let mut picker = ModelPicker::default();
    let row = row_at(&picker, picker.row);
    assert!(row.contains("Claude Opus 5"), "row: {row}");
    assert!(row.contains("200k ctx"), "row: {row}");
    assert!(row.contains("$5.00/$25 per M"), "row: {row}");
    assert!(row.contains("RVE"), "tags: {row}");

    picker.row = 1;
    let row = row_at(&picker, picker.row);
    assert!(row.contains("free"), "row: {row}");

    picker.row = 2;
    let row = row_at(&picker, picker.row);
    assert!(
        row.contains("—"),
        "an unknown price is never a number: {row}"
    );
    assert!(!row.contains('$'), "row: {row}");

    let detail = detail_text(&picker, &models, &flags);
    // The detail line names the route and then whatever the model has: an
    // unknown price is a dash, never a number. Asserting the whole string would
    // pin the column order as well, and a column added later is not a defect.
    assert!(
        detail.starts_with("detail local/mystery"),
        "detail: {detail}"
    );
    assert!(detail.contains("—"), "an unknown price is a dash: {detail}");
    assert!(!detail.contains('$'), "detail: {detail}");
}

#[test]
fn the_detail_line_names_the_accepted_ladder() {
    let models = vec![
        with_efforts(
            model("anthropic", "claude-opus"),
            &[Effort::Max, Effort::Low],
        ),
        model("bubna", "gemini-flash"),
    ];
    let flags = flags();
    let mut picker = ModelPicker::default();
    let detail = detail_text(&picker, &models, &flags);

    assert!(
        detail.contains("efforts low/max"),
        "ladder order, not catalog order: {detail}"
    );

    // Moved the way a keypress moves it, and the detail line re-read after: the
    // cursor index alone is not the highlight, and asserting on a line captured
    // before the move would pass whatever the first row said.
    picker.move_row(1, &models, &flags);
    let detail = detail_text(&picker, &models, &flags);
    assert!(
        detail.contains("all efforts"),
        "an unknown ladder says so instead of claiming every level: {detail}"
    );
}

#[test]
fn provider_counts_follow_the_filters() {
    let mut vision = model("anthropic", "opus");
    vision.supports_vision = true;
    let models = vec![
        vision,
        model("anthropic", "haiku"),
        model("openai", "gpt-5.1-codex"),
    ];
    let flags = flags();
    let mut picker = ModelPicker::default();

    let providers = picker.providers(&models, &flags);
    assert_eq!(providers[0].key, None);
    assert_eq!(providers[0].count, 3, "the all row counts every provider");
    assert_eq!(
        providers
            .iter()
            .map(|row| (row.key.clone().unwrap_or_default(), row.count))
            .collect::<Vec<_>>(),
        vec![
            (String::new(), 3),
            ("anthropic".to_string(), 2),
            ("openai".to_string(), 1),
        ]
    );

    picker.kind = Kind::Vision;
    let providers = picker.providers(&models, &flags);
    assert_eq!(providers[0].count, 1, "only the vision model is left");
    assert_eq!(
        providers
            .iter()
            .map(|row| row.key.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec![String::new(), "anthropic".to_string()],
        "a provider with nothing left under the filter leaves the column"
    );

    for ch in "haiku".chars() {
        picker.push_char(ch);
    }
    let providers = picker.providers(&models, &flags);
    assert_eq!(providers[0].count, 0, "the search leaves nothing");
}

#[test]
fn choosing_a_provider_scopes_the_models_and_recentres_the_cursor() {
    let models = vec![
        model("anthropic", "opus"),
        model("openai", "gpt-5.1-codex"),
        model("openai", "gpt-5.1"),
    ];
    let flags = flags();
    let mut picker = ModelPicker::default();
    picker.move_row(2, &models, &flags);
    assert_eq!(picker.row, 2);

    // all → anthropic → openai
    picker.move_provider(1, &models, &flags);
    assert_eq!(picker.provider.as_deref(), Some("anthropic"));
    assert_eq!(picker.row, 0);
    assert_eq!(picker.models(&models, &flags).len(), 1);

    picker.move_provider(1, &models, &flags);
    assert_eq!(picker.provider.as_deref(), Some("openai"));
    assert_eq!(picker.models(&models, &flags).len(), 2);

    picker.move_provider(1, &models, &flags);
    assert_eq!(picker.provider, None, "the all row wraps the column back");

    picker.backspace();
    assert_eq!(picker.provider, None);
}

#[test]
fn backspace_walks_back_out_of_the_provider_then_the_search() {
    let models = vec![model("anthropic", "opus"), model("openai", "gpt-5.1")];
    let flags = flags();
    let mut picker = ModelPicker::default();
    picker.move_provider(1, &models, &flags);
    assert_eq!(picker.provider.as_deref(), Some("anthropic"));

    for ch in "opus".chars() {
        picker.push_char(ch);
    }
    assert_eq!(picker.query, "opus");

    picker.backspace();
    assert_eq!(picker.query, "opu");
    assert_eq!(picker.provider.as_deref(), Some("anthropic"));

    // Three take the last characters off "opu"; the fourth finds the search
    // empty and climbs back out of the provider.
    for _ in 0..4 {
        picker.backspace();
    }
    assert_eq!(picker.query, "");
    assert_eq!(
        picker.provider, None,
        "an empty search backs out of the provider"
    );
    assert_eq!(picker.models(&models, &flags).len(), 2);
}

#[test]
fn a_search_that_matches_nothing_says_so() {
    let models = vec![model("anthropic", "opus")];
    let flags = flags();
    let mut picker = ModelPicker::default();
    for ch in "zzz".chars() {
        picker.push_char(ch);
    }
    let text = lines_text(&picker, &models, &flags);
    assert!(
        text.iter().any(|line| line.contains("no matching models")),
        "{text:?}"
    );
    assert!(text.iter().any(|line| line.contains("backspace")));
}

#[test]
fn an_empty_catalog_reads_as_loading() {
    let rows = row_texts(&ModelPicker::default(), &[], &flags());
    assert_eq!(rows, vec!["  loading…".to_string()]);
}

#[test]
fn the_search_caps_its_length_like_every_other_filter() {
    let mut picker = ModelPicker::default();
    for _ in 0..(crate::paste::FILTER_MAX_CHARS + 10) {
        picker.push_char('a');
    }
    assert_eq!(picker.query.chars().count(), crate::paste::FILTER_MAX_CHARS);
}

#[test]
fn the_model_cursor_is_clamped_to_a_list_that_shrank() {
    let models = vec![
        model("anthropic", "opus"),
        model("openai", "gpt-5.1"),
        model("openai", "gpt-5"),
    ];
    let flags = flags();
    let mut picker = ModelPicker::default();
    picker.move_row(2, &models, &flags);
    assert_eq!(picker.row, 2);

    // A refresh that dropped the model under the cursor must not leave the
    // picker pointing past the end of its own list.
    let shrunk = vec![model("anthropic", "opus")];
    picker.move_row(1, &shrunk, &flags);
    assert_eq!(picker.row, 0);
    assert!(picker.highlighted(&shrunk, &flags).is_some());
}

#[test]
fn the_kind_row_marks_the_active_filter() {
    let models = vec![model("anthropic", "opus")];
    let flags = flags();
    let opts = opts();
    let mut picker = ModelPicker::default();
    let rendered = lines(&picker, &models, &flags, area(), &opts);
    let kind_line = &rendered[2];
    let active: Vec<&str> = kind_line
        .spans
        .iter()
        .filter(|span| span.style == opts.theme.bold.patch(opts.theme.accent))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(active, vec!["all"]);

    picker.cycle_kind();
    let rendered = lines(&picker, &models, &flags, area(), &opts);
    let active: Vec<&str> = rendered[2]
        .spans
        .iter()
        .filter(|span| span.style == opts.theme.bold.patch(opts.theme.accent))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(active, vec!["reasoning"]);

    picker.cycle_kind();
    picker.cycle_kind();
    assert_eq!(picker.kind, Kind::All, "the filter row wraps");
}

#[test]
fn zdr_only_hides_models_the_user_has_not_marked() {
    let models = vec![model("anthropic", "opus"), model("openai", "gpt-5.1")];
    let mut flags = flags();
    flags.insert(
        "anthropic/opus".to_string(),
        pacode_types::ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    let picker = ModelPicker {
        zdr_only: true,
        ..ModelPicker::default()
    };

    let visible = picker.models(&models, &flags);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].route.to_string(), "anthropic/opus");
    assert_eq!(picker.providers(&models, &flags)[0].count, 1);
    assert!(
        row_texts(&picker, &models, &flags)
            .remove(0)
            .contains("ZDR")
    );
}

#[test]
fn the_window_keeps_the_cursor_on_screen_without_an_empty_lead() {
    // 20 models in a list shorter than the catalog: the window must scroll only
    // once the cursor reaches the bottom, never pad the top with blanks.
    let models: Vec<ModelInfo> = (0..20)
        .map(|i| model("anthropic", &format!("m{i:02}")))
        .collect();
    let flags = flags();
    let mut picker = ModelPicker::default();

    let rows = row_texts(&picker, &models, &flags);
    assert!(rows[0].contains("m00"), "{}", rows[0]);

    picker.move_row(9, &models, &flags);
    let rows = row_texts(&picker, &models, &flags);
    assert!(
        rows[0].contains("m00"),
        "the window starts at the top: {}",
        rows[0]
    );
    assert!(rows[9].contains("m09"), "{}", rows[9]);

    // Row 18 is the first past the bottom of an 18-row window, so the list
    // scrolls by exactly one row there.
    picker.move_row(9, &models, &flags);
    let rows = row_texts(&picker, &models, &flags);
    assert!(rows[0].contains("m01"), "scrolled by one row: {}", rows[0]);
    assert!(rows[9].contains("m10"), "{}", rows[9]);
}
