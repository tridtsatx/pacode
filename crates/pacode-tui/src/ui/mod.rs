//! Widgets. [`prepare_frame`](prepare_frame) reconciles render state before
//! [`draw`](draw), which paints one frame from an immutable `AppState`.

pub mod analyze;
pub mod anim;
mod cells;
pub mod chrome;
pub mod dialog;
pub mod footer;
mod footer_status;
pub mod header;
pub mod images;
pub mod input;
pub mod mascot;
pub mod menu;
pub mod model_catalog;
pub mod model_palette;
pub mod model_palette_view;
pub mod notice;
pub mod overlays;
pub mod panel;
pub mod phrases;
pub mod picker;
pub mod popup;
pub mod question_picker;
pub mod rail;
pub mod rail_session;
pub mod settings;
pub mod settings_ui;
pub mod theme_picker;
pub mod toast;
pub mod toolcell;

use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::layout::ScreenLayout;
use crate::state::transcript::Transcript;
use crate::state::{AppState, Focus, Overlay, PanelTarget};

/// A small key identifying the focus layer, so a draw can tell "a different
/// popup opened" without matching on payloads. Changing the index inside a
/// picker keeps the same key, so navigating does not replay the unfold. `pub`
/// for tests that set `state.focus` directly and mark it already seen.
pub fn focus_key(focus: &Focus) -> u8 {
    match focus {
        Focus::Normal => 0,
        Focus::SelectAgent { .. } => 1,
        Focus::Panel { .. } => 2,
        Focus::Overlay(overlay) => match overlay {
            Overlay::EffortPicker { .. } => 11,
            Overlay::ModePicker { .. } => 12,
            Overlay::QuestionPicker => 14,
            Overlay::ThemePicker { .. } => 15,
            Overlay::RailOverlay { .. } => 22,
            // Its own key: the page is a reading, not a menu level, and reusing a
            // menu key would replay the menu's unfold when it opens.
            Overlay::Analyze { .. } => 40,
            // Its own key: the settings page is a full screen, not a menu level,
            // and reusing a menu key would replay the menu's unfold.
            Overlay::Settings { .. } => 41,
            Overlay::ModelPalette { .. } => 42,
            // Menu levels key off `MenuId` — the table lives in the menu base
            // (§3.1); 25..=36 here are the `focus_key` range it owns.
            Overlay::Menu { stack, .. } => stack
                .current()
                .map_or(crate::ui::menu::MenuId::Root.focus_key(), |l| {
                    l.id.focus_key()
                }),
        },
    }
}

pub struct PreparedFrame {
    layout: ScreenLayout,
    opts: RenderOptions,
    dialog_opts: RenderOptions,
    dialog_area: Rect,
    banner_area: Option<Rect>,
    activity_area: Option<Rect>,
    dialog: dialog::PreparedDialog,
    input: Rc<input::PreparedInput>,
    picker_base: Rect,
    is_picker: bool,
    full_popup: Option<popup::FullPopup>,
    inline_popup: Option<popup::PreparedInlinePopup>,
    overlay: overlays::PreparedOverlay,
    rail: Option<rail::PreparedRail>,
    panel: Option<panel::PreparedPanel>,
    images: images::PreparedImages,
}

pub fn prepare_frame(area: Rect, state: &mut AppState, now: std::time::Instant) -> PreparedFrame {
    state.sync_render_context();
    let welcome_ms = now.saturating_duration_since(state.started_at).as_millis() as u64;
    let fkey = focus_key(&state.focus);
    if fkey != state.last_focus_key {
        let menu_min = crate::ui::menu::MenuId::Root.focus_key();
        let within_menu = fkey >= menu_min && state.last_focus_key >= menu_min;
        state.last_focus_key = fkey;
        if !within_menu {
            state.overlay_since = (fkey >= 3).then_some(now);
        }
    }
    let layout = crate::layout::compute_for_state(area, state, now);
    let is_picker = state.is_bottom_picker();
    let picker_base = if is_picker {
        popup::bottom_area(
            popup::bottom_picker_container(&layout),
            popup::bottom_picker_container(&layout).width,
            state.bottom_picker_height(),
        )
    } else {
        Rect::new(area.x, area.y, 0, 0)
    };
    let dialog_area = if is_picker {
        let height = picker_base
            .y
            .saturating_sub(layout.dialog.y)
            .saturating_sub(layout.toast.height);
        Rect::new(
            layout.dialog.x,
            layout.dialog.y,
            layout.dialog.width,
            height,
        )
    } else {
        layout.dialog
    };
    let input = input::prepare(&mut state.input, layout.input.width, layout.input.height);
    let completion = popup::InputCompletion::detect(state);
    popup::prepare_scan_wants(state, completion.as_ref());
    let full_popup = popup::resolve_full(state, &layout, now);
    model_palette_view::sync_scroll(state, popup::overlay_container(&layout));
    let rail = rail::resolve(state, layout.rail);
    // The countdown wakeup (`AppState::schedule_redraw_in`) reads what this
    // frame draws; no rail, no countdown on screen.
    state.rail.schedule_rows_drawn = rail.as_ref().map_or(0, |r| r.schedule_rows_drawn);
    let overlay = overlays::prepare(
        state,
        full_popup.as_ref(),
        rail.as_ref().map(|rail| rail.agents_scroll),
    );
    let has_rail_overlay = full_popup
        .as_ref()
        .is_some_and(|popup| popup.id == crate::ui::popup::PopupId::RailOverlay);
    if !has_rail_overlay && let Some(rail) = rail.as_ref() {
        rail::persist(state, rail);
    }
    let (banner_area, dialog_area, activity_area) = dialog_areas(state, dialog_area);
    let opts = state.render_options(area.width);
    let dialog_opts = state.render_options(dialog_area.width);
    let dialog = if dialog_area.width == 0 || dialog_area.height == 0 {
        dialog::PreparedDialog::default()
    } else {
        let anim_frame = state.anim_frame;
        dialog::prepare(
            dialog_area,
            dialog_transcript(state),
            &dialog_opts,
            anim_frame,
            welcome_ms,
        )
    };
    let panel = panel::prepare(state, &layout, welcome_ms);
    let images = images::prepare(layout.preview, layout.input, state, &opts);
    prepare_copy(state, dialog_area, welcome_ms);
    let inline_popup = if full_popup.is_some() {
        None
    } else {
        popup::resolve_inline_prepared(state, &layout, completion.as_ref())
    };
    PreparedFrame {
        layout,
        opts,
        dialog_opts,
        dialog_area,
        banner_area,
        activity_area,
        dialog,
        input,
        picker_base,
        is_picker,
        full_popup,
        inline_popup,
        overlay,
        rail,
        panel,
        images,
    }
}

/// The transcript the dialog column shows: the panel's, when an open agent
/// takes it over, and the session's otherwise. Asking the panel for the buffer
/// reconciles its owner, so a stale one is dropped before it can be painted.
fn dialog_transcript(state: &mut AppState) -> &mut Transcript {
    let agent = match state.panel_agent_target() {
        Some(PanelTarget::Agent(agent)) if state.agent_replaces_dialog() => Some(agent),
        _ => None,
    };
    match agent {
        Some(agent) => state.panel.agent_transcript_for(&agent),
        None => &mut state.transcript,
    }
}

/// Draw the prepared screen; returns the layout used for mouse hit-testing.
pub fn draw(frame: &mut Frame, state: &AppState, prepared: &PreparedFrame) -> ScreenLayout {
    let layout = prepared.layout;
    let opts = &prepared.opts;

    if layout.rail.width > 0 {
        if layout.rail_separator.width > 0 {
            let vline = opts.glyphs.vline;
            let sep_line = Line::from(Span::styled(vline, opts.theme.dim));
            for y in layout.rail_separator.y..layout.rail_separator.bottom() {
                frame.render_widget(
                    Paragraph::new(sep_line.clone()),
                    Rect::new(layout.rail_separator.x, y, 1, 1),
                );
            }
        }
        if let Some(rail) = prepared.rail.as_ref() {
            rail::draw(frame, rail, state, opts);
        }
    }

    if prepared.dialog_area.width > 0 && prepared.dialog_area.height > 0 {
        if let Some(banner_area) = prepared.banner_area {
            let name = match state.panel_agent_target() {
                Some(PanelTarget::Agent(id)) => state
                    .rail
                    .agent(&id)
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| id.to_string()),
                Some(PanelTarget::Task(_))
                | Some(PanelTarget::McpServer(_))
                | Some(PanelTarget::FileDiff { .. })
                | Some(PanelTarget::CommitDiff { .. })
                | None => String::new(),
            };
            let banner = Line::from(vec![
                Span::styled(
                    format!("{} agent ", opts.glyphs.agent_dot),
                    opts.theme.accent,
                ),
                Span::styled(name, opts.theme.bold),
                Span::styled("  ·  esc for the main chat", opts.theme.faint),
            ]);
            frame.render_widget(Paragraph::new(banner), banner_area);
        }
        dialog::draw(frame, prepared.dialog_area, &prepared.dialog);
        if let Some(activity_area) = prepared.activity_area
            && let Some((phase, _)) = state.phase.as_ref()
        {
            let anim_line = anim::render_activity_line(
                state.anim_frame,
                &prepared.dialog_opts,
                phase,
                state.phase_elapsed_ms,
                prepared.dialog_area.width as usize,
            );
            frame.render_widget(Paragraph::new(anim_line), activity_area);
        }
        let first_visible = dialog_first_visible_line(state);
        apply_selection_highlight(
            frame,
            &state.selection,
            prepared.dialog_area,
            first_visible,
            opts.theme.selected_bg,
        );
    }

    if let Some(panel_rect) = layout.panel {
        if let Some(sep_rect) = layout.panel_separator {
            let vline = opts.glyphs.vline;
            let sep_line = Line::from(Span::styled(vline, opts.theme.dim));
            for y in sep_rect.y..sep_rect.bottom() {
                frame.render_widget(
                    Paragraph::new(sep_line.clone()),
                    Rect::new(sep_rect.x, y, 1, 1),
                );
            }
        }
        if let Some(panel) = prepared.panel.as_ref() {
            let panel_opts = state.render_options(panel_rect.width);
            panel::draw(frame, panel_rect, panel, state, &panel_opts);
        }
    }

    if prepared.is_picker {
        let picker_area = prepared
            .full_popup
            .as_ref()
            .map_or(prepared.picker_base, |popup| popup.area);
        picker::draw(frame, picker_area, state, opts);
        if layout.toast.height > 0 {
            let toast_area = Rect::new(
                prepared.picker_base.x,
                prepared.picker_base.y.saturating_sub(layout.toast.height),
                prepared.picker_base.width,
                layout.toast.height,
            );
            toast::draw(frame, toast_area, state, opts);
        }
    } else {
        toast::draw(frame, layout.toast, state, opts);
        input::draw(frame, &layout, state, opts, &prepared.input);
        images::draw(frame, &prepared.images);
        if prepared.full_popup.is_none() {
            popup::draw(frame, prepared.inline_popup.as_ref(), opts);
        }
        footer::draw(frame, layout.footer, state, opts);
    }

    overlays::draw(
        frame,
        prepared.full_popup.as_ref(),
        &prepared.overlay,
        state,
        opts,
    );

    layout
}

fn dialog_areas(state: &AppState, area: Rect) -> (Option<Rect>, Rect, Option<Rect>) {
    if area.width == 0 || area.height == 0 {
        return (None, area, None);
    }
    if state.agent_replaces_dialog() && area.height > 1 {
        let banner = Rect::new(area.x, area.y, area.width, 1);
        let area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
        let (dialog, activity) = dialog_and_activity(state, area);
        return (Some(banner), dialog, activity);
    }
    let (dialog, activity) = dialog_and_activity(state, area);
    (None, dialog, activity)
}

fn dialog_and_activity(state: &AppState, area: Rect) -> (Rect, Option<Rect>) {
    if state.phase.is_none() || area.height <= 1 {
        return (area, None);
    }
    let activity = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    (
        Rect::new(area.x, area.y, area.width, area.height - 1),
        Some(activity),
    )
}

/// First content line on screen: the transcript the dialog column actually
/// painted, and — when an open agent owns that column — only its own buffer.
fn dialog_first_visible_line(state: &AppState) -> usize {
    match state.panel_agent_target() {
        Some(PanelTarget::Agent(agent)) if state.agent_replaces_dialog() => state
            .panel
            .owned_agent_transcript(&agent)
            .map_or(0, |transcript| transcript.first_visible_line),
        _ => state.transcript.first_visible_line,
    }
}

fn prepare_copy(state: &mut AppState, dialog_area: Rect, welcome_ms: u64) {
    let copy_req = state.selection.copy_request;
    let should_copy = match copy_req {
        crate::state::selection::CopyRequest::Explicit => true,
        crate::state::selection::CopyRequest::Auto => state.config.ui.auto_copy,
        crate::state::selection::CopyRequest::None => false,
    };
    state.selection.copy_request = crate::state::selection::CopyRequest::None;
    if !should_copy || !state.selection.is_active() || state.selection.is_empty() {
        return;
    }
    let Some(line_range) = state.selection.line_range() else {
        return;
    };
    let dialog_opts = state.render_options(dialog_area.width);
    let anim_frame = state.anim_frame;
    let width = dialog_area.width;
    let window = dialog::PlainWindow::for_lines(line_range);
    let lines = dialog::plain_lines(
        dialog_transcript(state),
        width,
        &dialog_opts,
        anim_frame,
        welcome_ms,
        window,
    );
    let text = extract_selection_text(&lines, &state.selection, width);
    if !text.is_empty() {
        state.pending_copy = Some(crate::state::PendingCopy {
            text,
            auto: matches!(copy_req, crate::state::selection::CopyRequest::Auto),
        });
    }
}

/// Paint the part of the selection that is currently on screen. The selection
/// itself is in content coordinates, so scrolling moves the highlight with the
/// text rather than leaving it on the same rows.
pub(crate) fn apply_selection_highlight(
    frame: &mut Frame,
    selection: &crate::state::Selection,
    dialog_area: Rect,
    first_visible_line: usize,
    selected_bg: ratatui::style::Style,
) {
    if !selection.is_active() || selection.is_empty() || dialog_area.width == 0 {
        return;
    }
    for screen_row in 0..dialog_area.height {
        let line = first_visible_line + screen_row as usize;
        let Some(col_range) = selection.col_range_for_line(line, dialog_area.width) else {
            continue;
        };
        for col in col_range {
            let x = dialog_area.x + col;
            let y = dialog_area.y + screen_row;
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(cell.style().patch(selected_bg));
            }
        }
    }
}

/// Upper bound on one copy, so selecting a very long transcript cannot build an
/// unbounded string in memory, and so the OSC 52 escape the clipboard write
/// base64s stays inside what a terminal accepts. 256 KiB of text is well past
/// any real selection.
pub const COPY_MAX_CHARS: usize = 256 * 1024;

/// The selected text, taken from the transcript's own lines rather than from the
/// screen buffer, so a selection that runs past the viewport copies in full.
pub(crate) fn extract_selection_text(
    lines: &dialog::PlainLines,
    selection: &crate::state::Selection,
    width: u16,
) -> String {
    let Some(line_range) = selection.line_range() else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut budget = COPY_MAX_CHARS;
    for line_idx in line_range {
        let Some(col_range) = selection.col_range_for_line(line_idx, width) else {
            continue;
        };
        let index = line_idx.saturating_sub(lines.first_line);
        let Some(text) = lines.lines.get(index) else {
            continue;
        };
        // Decoration (bar, marker, gutter) is drawn, never copied.
        let lead = lines.leads.get(index).copied().unwrap_or(0);
        let start = (*col_range.start()).max(u16::try_from(lead).unwrap_or(u16::MAX));
        let slice = if start > *col_range.end() {
            String::new()
        } else {
            crate::state::selection::slice_by_columns(text, start, *col_range.end()).to_string()
        };
        let slice = slice.trim_end().to_string();
        if slice.chars().count() > budget {
            out.push(slice.chars().take(budget).collect());
            break;
        }
        budget -= slice.chars().count();
        out.push(slice);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pacode_types::state::{AgentKind, AgentStatus};
    use pacode_types::{
        AgentId, AgentInfo, Config, Effort, ModelRoute, TranscriptItem, TranscriptKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn prepare_and_draw_frame(frame: &mut Frame<'_>, state: &mut AppState) {
        let prepared = prepare_frame(frame.area(), state, Instant::now());
        draw(frame, state, &prepared);
    }

    fn regression_state(cursor: usize, follow: bool, started_at: Instant) -> AppState {
        let mut config = Config::default();
        config.ui.agent_view = pacode_types::config::AgentView::Panel;
        let mut state = AppState::new(config, "test".into(), 100, 30);
        state.mascot = crate::ui::mascot::MascotKind::Pacman;
        state.started_at = started_at;
        state.input.text = "0\n1\n2\n3\n4\n5\n6\n7".into();
        state.input.cursor = cursor;
        state.input.input_scroll = 99;
        let target = crate::state::PanelTarget::Agent(AgentId::main());
        state.panel.target = Some(target.clone());
        state.focus = Focus::Panel {
            target,
            follow,
            follow_paused: false,
        };
        state.panel.agent_transcript.reset(
            (1_u64..=20)
                .map(|seq| TranscriptItem {
                    seq,
                    agent: AgentId::main(),
                    ts_ms: seq * 1_000,
                    kind: TranscriptKind::User {
                        text: format!("panel line {seq}"),
                    },
                })
                .collect(),
            false,
        );
        state.panel.agent_transcript.scroll_from_bottom = 12;
        state
    }

    fn finished_agent(id: &str, started_at_ms: u64) -> AgentInfo {
        AgentInfo {
            id: AgentId::new(id),
            name: id.to_string(),
            kind: AgentKind::Sub,
            status: AgentStatus::Finished,
            activity: None,
            started_at_ms,
            finished_at_ms: Some(started_at_ms + 1_000),
            tokens_in: 0,
            tokens_out: 0,
            model: ModelRoute::new("test", "model"),
            effort: Effort::High,
            parent: Some(AgentId::main()),
            summary: None,
            error: None,
        }
    }

    #[test]
    fn focus_key_distinguishes_layers_not_indexes() {
        assert_eq!(focus_key(&Focus::Normal), 0);
        assert_eq!(focus_key(&Focus::SelectAgent { index: 2 }), 1);
        assert_ne!(
            focus_key(&Focus::Overlay(Overlay::ModelPalette {
                palette: Default::default(),
            })),
            focus_key(&Focus::Overlay(Overlay::RailOverlay {
                selected: pacode_types::AgentId::main(),
            }))
        );
        // Index changes inside one picker keep the key — no replayed unfold.
        assert_eq!(
            focus_key(&Focus::Overlay(Overlay::EffortPicker { index: 0 })),
            focus_key(&Focus::Overlay(Overlay::EffortPicker { index: 4 }))
        );
        assert_ne!(
            focus_key(&Focus::Normal),
            focus_key(&Focus::Overlay(Overlay::RailOverlay {
                selected: pacode_types::AgentId::main(),
            }))
        );
    }

    #[test]
    fn sequential_draw_reconciles_changed_cursor_and_follow_panel() {
        let started_at = Instant::now() - Duration::from_secs(60);
        let mut sequential = regression_state(15, false, started_at);
        let mut sequential_terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        sequential_terminal
            .draw(|frame| {
                prepare_and_draw_frame(frame, &mut sequential);
            })
            .unwrap();
        assert_eq!(sequential.input.input_scroll, 2);
        assert!(sequential.panel.agent_transcript.scroll_from_bottom > 0);

        sequential.input.cursor = 0;
        sequential.focus = Focus::Panel {
            target: crate::state::PanelTarget::Agent(AgentId::main()),
            follow: true,
            follow_paused: false,
        };
        sequential_terminal
            .draw(|frame| {
                prepare_and_draw_frame(frame, &mut sequential);
            })
            .unwrap();

        let mut cold = regression_state(0, true, started_at);
        let mut cold_terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        cold_terminal
            .draw(|frame| {
                prepare_and_draw_frame(frame, &mut cold);
            })
            .unwrap();

        assert_eq!(sequential.input.input_scroll, cold.input.input_scroll);
        assert_eq!(
            sequential.panel.agent_transcript.scroll_from_bottom,
            cold.panel.agent_transcript.scroll_from_bottom
        );
        let sequential_backend = sequential_terminal.backend();
        let cold_backend = cold_terminal.backend();
        assert_eq!(format!("{sequential_backend}"), format!("{cold_backend}"));
    }

    #[test]
    fn window_wants_reconcile_before_read_only_paint() {
        let mut state = AppState::new(Config::default(), "test".into(), 120, 34);
        state.cwd = "/repo".into();
        state
            .files
            .observe_tool_item("read", &serde_json::json!({ "path": "src/main.rs" }), 1);
        state.focus = Focus::Overlay(Overlay::Menu {
            stack: crate::ui::menu::MenuStack::root(crate::ui::menu::MenuId::Files),
            ui: crate::ui::menu::MenuUiState::default(),
        });
        state.last_focus_key = focus_key(&state.focus);
        let area = Rect::new(0, 0, 120, 34);
        let prepared = prepare_frame(area, &mut state, Instant::now());
        let want = state
            .files
            .preview_want
            .clone()
            .expect("files preview reconciled before paint");
        assert_eq!(want.raw, "src/main.rs");
        assert!(
            prepared
                .full_popup
                .as_ref()
                .and_then(|popup| popup.menu.as_ref())
                .is_some_and(|menu| menu.view.id == crate::ui::menu::MenuId::Files)
        );

        let focus = state.focus.clone();
        let overlay_since = state.overlay_since;
        let mut terminal = Terminal::new(TestBackend::new(120, 34)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state, &prepared);
            })
            .unwrap();
        let first = format!("{}", terminal.backend());
        terminal
            .draw(|frame| {
                draw(frame, &state, &prepared);
            })
            .unwrap();

        assert_eq!(state.files.preview_want.as_ref(), Some(&want));
        assert_eq!(state.focus, focus);
        assert_eq!(state.overlay_since, overlay_since);
        assert_eq!(format!("{}", terminal.backend()), first);
    }

    #[test]
    fn completion_window_is_claimed_after_prepare_not_rearmed_by_paint() {
        let mut state = AppState::new(Config::default(), "test".into(), 80, 24);
        state.input.insert_str("!echo @sr");
        let area = Rect::new(0, 0, 80, 24);
        let prepared = prepare_frame(area, &mut state, Instant::now());
        assert_eq!(
            prepared
                .inline_popup
                .as_ref()
                .map(|popup| popup.window)
                .as_ref(),
            Some(&crate::ui::popup::PopupWindow::default())
        );
        let want = state
            .input
            .at_comp
            .claim_want()
            .expect("at-completion window reconciled before paint");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state, &prepared);
            })
            .unwrap();

        assert_eq!(state.input.at_comp.claim_want(), None);
        assert_eq!(state.input.bash_comp.claim_want(), None);
        assert_eq!(want.query, "sr");
    }

    #[test]
    fn the_prepared_frame_carries_the_options_the_draw_would_have_built() {
        let mut state = AppState::new(Config::default(), "test".into(), 120, 34);
        let area = Rect::new(0, 0, 120, 34);
        let prepared = prepare_frame(area, &mut state, Instant::now());

        assert_eq!(prepared.opts, state.render_options(area.width));
        assert_eq!(
            prepared.dialog_opts,
            state.render_options(prepared.dialog_area.width)
        );
    }

    #[test]
    fn rail_scroll_reconciles_before_read_only_paint() {
        let mut state = AppState::new(Config::default(), "test".into(), 120, 14);
        state.rail.agents = (1..=4)
            .map(|index| finished_agent(&format!("agt_{index}"), index))
            .collect();
        state.focus = Focus::SelectAgent { index: 4 };
        let area = Rect::new(0, 0, 120, 14);
        let prepared = prepare_frame(area, &mut state, Instant::now());
        let rail = prepared.rail.as_ref().expect("rail prepared");
        assert_eq!(rail.selected_subagent, Some(3));
        assert!(rail.agents_scroll > 0);
        assert_eq!(state.rail.agents_scroll, rail.agents_scroll);

        let focus = state.focus.clone();
        let mut terminal = Terminal::new(TestBackend::new(120, 14)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &state, &prepared);
            })
            .unwrap();
        let first = format!("{}", terminal.backend());
        terminal
            .draw(|frame| {
                draw(frame, &state, &prepared);
            })
            .unwrap();

        assert_eq!(state.rail.agents_scroll, rail.agents_scroll);
        assert_eq!(state.focus, focus);
        assert_eq!(format!("{}", terminal.backend()), first);
    }
}
