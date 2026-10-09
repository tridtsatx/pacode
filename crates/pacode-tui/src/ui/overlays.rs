//! Overlays drawn over the dialog: the menu base (§3) and the rail overlay
//! (Tiny tier). Width 78% of the dialog column, top offset 2, rounded border,
//! bold title on the top edge, dim key hints on the bottom edge.
//!
//! Shared chrome lives here: `menu_block` draws the border, `selected_row`
//! paints the highlighted row as a full-width bar, and `window_groups`
//! scrolls variable-height rows so the selected one stays fully visible.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};

use pacode_render::RenderOptions;

use crate::state::{AppState, Focus, Overlay};

#[derive(Default)]
pub(crate) struct PreparedOverlay {
    rail: Option<crate::ui::rail::PreparedRail>,
}

pub(crate) fn prepare(
    state: &mut AppState,
    popup: Option<&crate::ui::popup::FullPopup>,
    base_rail_scroll: Option<usize>,
) -> PreparedOverlay {
    let menu = popup
        .filter(|popup| popup.id == crate::ui::popup::PopupId::Menu)
        .and_then(|popup| popup.menu.as_ref());
    state.session.files.preview_want = menu
        .filter(|menu| menu.view.id == crate::ui::menu::MenuId::Files)
        .and_then(|menu| crate::ui::menu::files::preview_want(&menu.view, &menu.ui));
    state.session.rail.task_tail_want = menu
        .filter(|menu| menu.view.id == crate::ui::menu::MenuId::Background)
        .and_then(|menu| crate::ui::menu::background::tail_want(state, &menu.view, &menu.ui));
    let (show_want, more_want) = menu
        .filter(|menu| menu.view.id == crate::ui::menu::MenuId::Git)
        .map_or((None, None), |menu| {
            let inner_w = popup.map_or(0, |popup| popup.area.width.saturating_sub(2));
            let detail_wide = crate::ui::menu::detail_panel_width(inner_w).is_some();
            crate::ui::menu::git::wants(state, &menu.view, &menu.ui, detail_wide)
        });
    state.session.git.show_want = show_want;
    state.session.git.more_want = more_want;
    let rail = popup
        .filter(|popup| popup.id == crate::ui::popup::PopupId::RailOverlay)
        .and_then(|popup| {
            let area = rail_overlay_inner(popup.area);
            let stored_scroll = base_rail_scroll.unwrap_or(state.rail.agents_scroll);
            let rail = crate::ui::rail::resolve_from(state, area, stored_scroll);
            if let Some(rail) = rail.as_ref() {
                crate::ui::rail::persist(state, rail);
            }
            rail
        });
    PreparedOverlay { rail }
}

pub(crate) fn draw(
    frame: &mut Frame,
    popup: Option<&crate::ui::popup::FullPopup>,
    prepared: &PreparedOverlay,
    state: &AppState,
    opts: &RenderOptions,
) {
    let Some(popup) = popup else {
        return;
    };
    if popup.area.width == 0 || popup.area.height == 0 {
        return;
    }
    match popup.id {
        crate::ui::popup::PopupId::Menu => {
            let Some(menu) = popup.menu.as_ref() else {
                return;
            };
            crate::ui::menu::draw_in(frame, popup.area, &menu.view, &menu.ui, opts);
        }
        crate::ui::popup::PopupId::RailOverlay => {
            frame.render_widget(Clear, popup.area);
            draw_rail_overlay(frame, popup.area, prepared, state, opts);
        }
        crate::ui::popup::PopupId::ModelPalette => {
            if let Some(palette) = popup.palette.as_ref() {
                crate::ui::model_palette_view::draw(frame, popup.area, palette);
            }
        }
        crate::ui::popup::PopupId::Settings => {
            let (Focus::Overlay(Overlay::Settings { ui }), Some(_)) =
                (&state.focus, state.overlay_since)
            else {
                return;
            };
            frame.render_widget(Clear, popup.area);
            crate::ui::settings_ui::draw(frame, popup.area, state, ui, opts);
        }
        crate::ui::popup::PopupId::Analyze => {
            let (Some(report), Focus::Overlay(Overlay::Analyze { view })) =
                (state.analyze_report.as_ref(), &state.focus)
            else {
                // Opened before the report landed, or after it was cleared. A
                // blank frame beats a page reading someone else's session.
                return;
            };
            frame.render_widget(Clear, popup.area);
            crate::ui::analyze::draw(frame, popup.area, report, view, opts);
        }
        crate::ui::popup::PopupId::Slash
        | crate::ui::popup::PopupId::At
        | crate::ui::popup::PopupId::Bash
        | crate::ui::popup::PopupId::PromptQueue
        | crate::ui::popup::PopupId::EffortPicker
        | crate::ui::popup::PopupId::ModePicker
        | crate::ui::popup::PopupId::QuestionPicker
        | crate::ui::popup::PopupId::ThemePicker => {}
    }
}

/// The shared overlay frame: rounded border, bold title on the top edge, dim
/// `key action · key action` hints on the bottom edge.
pub(crate) fn menu_block(title: &str, hint: &str, opts: &RenderOptions) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(crate::ui::chrome::border_set(opts))
        .title(Line::from(vec![
            Span::styled(opts.glyphs.hline, opts.theme.border),
            Span::styled(format!(" {title} "), opts.theme.bold),
        ]))
        .title_bottom(Span::styled(hint.to_string(), opts.theme.dim))
        .border_style(opts.theme.border)
}

/// The selected row, padded to the row's full width so the highlight is a bar,
/// not a word.
///
/// A bar with a background of its own (truecolor, or a palette's named
/// `selected_bg`) keeps every span's colour over it; the pointer stays
/// accent. A bar drawn with `REVERSED` (ANSI-16, mono), or with no
/// background at all, would turn each span's own colour into a block of that
/// colour — a patchwork — so there the whole row takes `selected_bg` alone:
/// one uniform bar, badges included.
pub(crate) fn selected_row(line: Line<'static>, width: u16, opts: &RenderOptions) -> Line<'static> {
    let width = width as usize;
    let used: usize = line
        .spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum();
    let bar = opts.theme.selected_bg;
    let keeps_colours = bar.bg.is_some()
        && !bar
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED);
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| {
            let style = if keeps_colours {
                bar.patch(s.style)
            } else {
                bar
            };
            Span::styled(s.content.into_owned(), style)
        })
        .collect();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            opts.theme.selected_bg,
        ));
    }
    Line::from(spans)
}

/// First index of a window over variable-height row groups such that the
/// selected group is the last fully visible one and the whole window fits the
/// line budget — the same rule `window` uses for fixed-height rows.
pub(crate) fn window_groups(heights: &[usize], sel: usize, budget: usize) -> usize {
    if heights.is_empty() || budget == 0 {
        return 0;
    }
    let sel = sel.min(heights.len() - 1);
    let mut start = sel;
    let mut used = heights[sel];
    while start > 0 {
        let next = heights[start - 1];
        if used + next > budget {
            break;
        }
        used += next;
        start -= 1;
    }
    start
}

/// `2h ago`-style age of a timestamp; `just now` under a minute.
pub(crate) fn age_ago(ts_ms: u64, now: u64) -> String {
    let mins = now.saturating_sub(ts_ms) / 60_000;
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / (60 * 24))
    }
}

/// `~/rel/path` when under HOME, else the path as written.
/// `pub(crate)`: the Sessions menu's detail card shows the same shape.
pub(crate) fn short_path(path: &std::path::Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.to_string_lossy().into_owned()
}

thread_local! {
    static HOME: std::cell::RefCell<Option<Option<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

fn home_dir() -> Option<std::path::PathBuf> {
    HOME.with(|cell| {
        let mut cached = cell.borrow_mut();
        if cached.is_none() {
            *cached = Some(std::env::var_os("HOME").map(std::path::PathBuf::from));
        }
        cached.clone().flatten()
    })
}

/// The `… — loading…` / `… — offline copy` title suffix a picker gets from
/// its list's cache health (§8.2).
pub(crate) fn title_with_badge(title: &str, badge: Option<crate::cache::Badge>) -> String {
    match badge {
        Some(crate::cache::Badge::Loading) => format!("{title} — loading…"),
        Some(crate::cache::Badge::OfflineCopy) => format!("{title} — offline copy"),
        None => title.to_string(),
    }
}

fn draw_rail_overlay(
    frame: &mut Frame,
    area: Rect,
    prepared: &PreparedOverlay,
    state: &AppState,
    opts: &RenderOptions,
) {
    let block = menu_block("Plan & Agents", " esc close ", opts);
    frame.render_widget(block, area);
    if let Some(rail) = prepared.rail.as_ref() {
        crate::ui::rail::draw(frame, rail, state, opts);
    }
}

fn rail_overlay_inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_groups_keeps_the_selected_group_fully_visible() {
        // Two-line groups, budget for three groups.
        let heights = vec![2, 2, 2, 2, 2];
        assert_eq!(window_groups(&heights, 0, 6), 0);
        assert_eq!(window_groups(&heights, 2, 6), 0);
        assert_eq!(window_groups(&heights, 3, 6), 1);
        // A taller selected group pulls the window further down.
        let heights = vec![2, 3, 2, 4, 2];
        assert_eq!(window_groups(&heights, 3, 6), 2);
        // A group that alone fills the budget starts at itself.
        let heights = vec![2, 2, 2, 5, 2];
        assert_eq!(window_groups(&heights, 3, 5), 3);
        // Degenerate inputs do not panic.
        assert_eq!(window_groups(&[], 0, 5), 0);
        assert_eq!(window_groups(&heights, 0, 0), 0);
        assert_eq!(window_groups(&heights, 99, 6), 4);
    }

    /// ANSI-16 and mono draw the bar with REVERSED: every span's own colour
    /// (a badge's included) gives way to one bar style across the row.
    #[test]
    fn a_reversed_selection_bar_is_one_uniform_style() {
        for theme in [pacode_render::Theme::ansi(), pacode_render::Theme::mono()] {
            let opts = RenderOptions::with_theme(40, false, theme);
            let line = Line::from(vec![
                Span::styled("▸ ".to_string(), opts.theme.accent),
                Span::styled("ANTHROPIC".to_string(), opts.theme.bold),
                crate::ui::chrome::badge("●", opts.theme.green, &opts),
                Span::styled(" signed in".to_string(), opts.theme.green),
            ]);
            let row = selected_row(line, 30, &opts);
            assert!(
                row.spans
                    .iter()
                    .all(|span| span.style == opts.theme.selected_bg),
                "{row:?}"
            );
        }
    }

    /// A named-colour bar without REVERSED (a user palette's `"blue"`) has a
    /// background of its own: the spans keep their colours over it.
    #[test]
    fn a_named_colour_bar_keeps_the_span_colours() {
        let mut theme = pacode_render::Theme::ansi();
        theme.selected_bg = ratatui::style::Style::default().bg(ratatui::style::Color::Blue);
        let opts = RenderOptions::with_theme(40, false, theme);
        let line = Line::from(vec![
            Span::styled("▸ ".to_string(), opts.theme.accent),
            Span::styled(" signed in".to_string(), opts.theme.green),
        ]);
        let row = selected_row(line, 30, &opts);
        assert_eq!(row.spans[0].style.fg, opts.theme.accent.fg);
        assert_eq!(row.spans[1].style.fg, opts.theme.green.fg);
        assert!(
            row.spans
                .iter()
                .all(|span| span.style.bg == Some(ratatui::style::Color::Blue))
        );
    }

    #[test]
    fn selected_row_pads_to_the_full_width() {
        let opts = RenderOptions::new(40, false);
        let line = Line::from(vec![
            Span::styled("▸ ".to_string(), opts.theme.accent),
            Span::styled("name".to_string(), opts.theme.bold),
        ]);
        let row = selected_row(line, 20, &opts);
        let width: usize = row
            .spans
            .iter()
            .map(|s| pacode_render::display_width(&s.content))
            .sum();
        assert_eq!(width, 20);
        // The pointer keeps the accent colour under the selection bar.
        assert_eq!(row.spans[0].style.fg, opts.theme.accent.fg);
    }

    #[test]
    fn age_ago_reads_like_a_clock() {
        let now = 1_000_000_000_000u64;
        assert_eq!(age_ago(now, now), "just now");
        assert_eq!(age_ago(now - 5 * 60_000, now), "5m ago");
        assert_eq!(age_ago(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(age_ago(now - 2 * 86_400_000, now), "2d ago");
    }

    #[test]
    fn short_path_keeps_the_home_prefix_and_answers_repeatedly() {
        if let Some(home) = home_dir() {
            let inside = home.join("code/pacode");
            assert_eq!(short_path(&inside), "~/code/pacode");
            assert_eq!(short_path(&inside), "~/code/pacode");
        }
        assert_eq!(
            short_path(std::path::Path::new("relative/path")),
            "relative/path"
        );
    }
}
