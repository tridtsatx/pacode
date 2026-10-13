use std::collections::VecDeque;
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;

use pacode_render::RenderOptions;

use crate::at_complete::AtCandidate;
use crate::bash::complete::Candidate;
use crate::commands::SlashCommand;
use crate::layout::ScreenLayout;
use crate::state::input::{AtKey, BashKey};
use crate::state::{AppState, Focus, Overlay};

mod draw;

pub const MAX_VISIBLE: u16 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupId {
    Slash,
    At,
    Bash,
    PromptQueue,
    Menu,
    EffortPicker,
    ModePicker,
    QuestionPicker,
    ThemePicker,
    RailOverlay,
    Analyze,
    Settings,
    ModelPalette,
}

impl PopupId {
    pub fn is_full(self) -> bool {
        !matches!(
            self,
            Self::Slash | Self::At | Self::Bash | Self::PromptQueue
        )
    }

    fn is_bottom(self) -> bool {
        matches!(
            self,
            Self::EffortPicker | Self::ModePicker | Self::QuestionPicker | Self::ThemePicker
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PopupWindow {
    pub start: usize,
    pub len: usize,
    pub selected: usize,
    pub total: usize,
}

#[derive(Debug)]
enum PopupContent<'a> {
    Slash(Vec<SlashCommand>),
    At {
        at_byte_index: usize,
        cursor_byte: usize,
        candidates: &'a [AtCandidate],
    },
    Bash {
        candidates: &'a [Candidate],
    },
    PromptQueue(&'a VecDeque<String>),
    Full,
}

#[derive(Debug)]
pub(crate) struct MenuPopup {
    pub view: crate::ui::menu::MenuView,
    pub ui: crate::ui::menu::MenuUiState,
}

pub(crate) struct FullPopup {
    pub id: PopupId,
    pub area: Rect,
    pub menu: Option<MenuPopup>,
    /// The model palette's frame, built from one pass over its rows.
    pub palette: Option<crate::ui::model_palette_view::PreparedPalette>,
}

pub(crate) enum PreparedInlineContent {
    Slash(Vec<SlashCommand>),
    At(Vec<AtCandidate>),
    Bash(Vec<Candidate>),
    PromptQueue(Vec<String>),
}

pub(crate) struct PreparedInlinePopup {
    pub area: Rect,
    pub window: PopupWindow,
    pub content: PreparedInlineContent,
}

#[derive(Debug)]
pub struct ResolvedPopup<'a> {
    id: PopupId,
    area: Rect,
    window: PopupWindow,
    content: PopupContent<'a>,
}

impl ResolvedPopup<'_> {
    pub fn id(&self) -> PopupId {
        self.id
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn is_full(&self) -> bool {
        self.id.is_full()
    }

    pub fn window(&self) -> PopupWindow {
        self.window
    }

    pub(crate) fn accepted(&self) -> Option<AcceptedCompletion> {
        match &self.content {
            PopupContent::At {
                at_byte_index,
                cursor_byte,
                candidates,
            } => Some(AcceptedCompletion::At {
                at_byte_index: *at_byte_index,
                cursor_byte: *cursor_byte,
                candidate: candidates.get(self.window.selected)?.clone(),
            }),
            PopupContent::Bash { candidates, .. } => Some(AcceptedCompletion::Bash {
                replacement: candidates.get(self.window.selected)?.replacement.clone(),
            }),
            PopupContent::Slash(commands) => Some(AcceptedCompletion::Slash {
                name: commands.get(self.window.selected)?.name.to_string(),
            }),
            PopupContent::PromptQueue(_) | PopupContent::Full => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum InputCompletion {
    At { key: AtKey, at_byte_index: usize },
    Bash { key: BashKey },
}

impl InputCompletion {
    pub(crate) fn detect(state: &AppState) -> Option<Self> {
        if matches!(state.focus, Focus::Overlay(_)) {
            return None;
        }
        let byte_cursor =
            crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
        if !state.input.at_closed
            && let Some(query) =
                pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor)
        {
            return Some(InputCompletion::At {
                key: AtKey {
                    query: query.query.clone(),
                    cursor_byte: byte_cursor,
                    cwd: state.cwd(),
                },
                at_byte_index: query.at_byte_index,
            });
        }
        if state.input.text.starts_with('!') && !state.input.bash_complete_closed {
            return Some(InputCompletion::Bash {
                key: BashKey {
                    text: state.input.text[1..].to_string(),
                    cursor: state.input.cursor.saturating_sub(1),
                    cwd: state.cwd(),
                    history_len: state.input.bash_history.len(),
                },
            });
        }
        None
    }
}

pub fn resolve<'a>(
    state: &'a AppState,
    layout: &ScreenLayout,
    now: Instant,
) -> Option<ResolvedPopup<'a>> {
    let Some(popup) = resolve_full(state, layout, now) else {
        return resolve_inline(state, layout);
    };
    let FullPopup { id, area, .. } = popup;
    Some(ResolvedPopup {
        id,
        area,
        window: PopupWindow::default(),
        content: PopupContent::Full,
    })
}

pub(crate) fn resolve_full(
    state: &AppState,
    layout: &ScreenLayout,
    now: Instant,
) -> Option<FullPopup> {
    let Focus::Overlay(overlay) = &state.focus else {
        return None;
    };
    let id = match overlay {
        Overlay::EffortPicker { .. } => PopupId::EffortPicker,
        Overlay::ModePicker { .. } => PopupId::ModePicker,
        Overlay::QuestionPicker => PopupId::QuestionPicker,
        Overlay::ThemePicker { .. } => PopupId::ThemePicker,
        Overlay::RailOverlay { .. } => PopupId::RailOverlay,
        Overlay::Analyze { .. } => PopupId::Analyze,
        Overlay::Settings { .. } => PopupId::Settings,
        Overlay::ModelPalette { .. } => PopupId::ModelPalette,
        // A menu with nothing on its stack has no level to draw, and reporting
        // it as a full overlay would suppress the inline completions the
        // reader's own prompt is showing.
        Overlay::Menu { stack, .. } if stack.is_empty() => return None,
        Overlay::Menu { .. } => PopupId::Menu,
    };
    if let Focus::Overlay(Overlay::ModelPalette { palette }) = &state.focus {
        let elapsed = state
            .overlay_since
            .map(|opened| now.saturating_duration_since(opened).as_millis() as u64);
        let opts = state.render_options(layout.screen.width);
        let (area, prepared) = crate::ui::model_palette_view::prepare(
            state,
            palette,
            overlay_container(layout),
            elapsed,
            &opts,
        );
        return Some(FullPopup {
            id,
            area,
            menu: None,
            palette: Some(prepared),
        });
    }
    let mut area = full_area(state, layout, now, id);
    let menu = match (id, &state.focus) {
        (PopupId::Menu, Focus::Overlay(Overlay::Menu { stack, ui })) if !stack.is_empty() => {
            let elapsed = state
                .overlay_since
                .map(|opened| now.saturating_duration_since(opened).as_millis() as u64);
            let container = center_container(overlay_container(layout), elapsed);
            let view = crate::ui::menu::view_for(stack, state, ui);
            area = crate::ui::menu::overlay_area(container, &view, ui);
            Some(MenuPopup {
                view,
                ui: ui.clone(),
            })
        }
        _ => None,
    };
    Some(FullPopup {
        id,
        area,
        menu,
        palette: None,
    })
}

pub(crate) fn resolve_inline<'a>(
    state: &'a AppState,
    layout: &ScreenLayout,
) -> Option<ResolvedPopup<'a>> {
    let completion = InputCompletion::detect(state);
    resolve_inline_detected(state, layout, completion.as_ref())
}

fn resolve_inline_detected<'a>(
    state: &'a AppState,
    layout: &ScreenLayout,
    completion: Option<&InputCompletion>,
) -> Option<ResolvedPopup<'a>> {
    match completion {
        Some(InputCompletion::At { key, at_byte_index }) => {
            let candidates = state.input.at_comp.candidates_for(key).unwrap_or(&[]);
            let selected = state.input.at_index % candidates.len().max(1);
            let area = above_input(layout, layout.input.width.min(50), candidates.len());
            return Some(ResolvedPopup {
                id: PopupId::At,
                area,
                window: visible_window(candidates.len(), selected, area.height),
                content: PopupContent::At {
                    at_byte_index: *at_byte_index,
                    cursor_byte: key.cursor_byte,
                    candidates,
                },
            });
        }
        Some(InputCompletion::Bash { key }) => {
            let candidates = state.input.bash_comp.candidates_for(key).unwrap_or(&[]);
            let selected = state.input.bash_complete_index % candidates.len().max(1);
            let area = above_input(layout, layout.input.width.min(50), candidates.len());
            return Some(ResolvedPopup {
                id: PopupId::Bash,
                area,
                window: visible_window(candidates.len(), selected, area.height),
                content: PopupContent::Bash { candidates },
            });
        }
        None => {}
    }

    if state.input.text.starts_with('/') && !state.input.text.contains(' ') {
        let commands = crate::commands::matching(&state.input.text[1..]);
        if !commands.is_empty() {
            let selected = state.input.slash_index % commands.len();
            let area = above_input(layout, layout.input.width.min(50), commands.len());
            return Some(ResolvedPopup {
                id: PopupId::Slash,
                area,
                window: visible_window(commands.len(), selected, area.height),
                content: PopupContent::Slash(commands),
            });
        }
    }

    if state.input.prompt_queue.is_empty() {
        return None;
    }
    let total = state.input.prompt_queue.len();
    let area = above_input(layout, layout.input.width.min(60), total);
    Some(ResolvedPopup {
        id: PopupId::PromptQueue,
        area,
        window: visible_window(total, 0, area.height),
        content: PopupContent::PromptQueue(&state.input.prompt_queue),
    })
}

pub(crate) fn resolve_inline_prepared(
    state: &AppState,
    layout: &ScreenLayout,
    completion: Option<&InputCompletion>,
) -> Option<PreparedInlinePopup> {
    let popup = resolve_inline_detected(state, layout, completion)?;
    let window = popup.window;
    let content = match popup.content {
        PopupContent::Slash(commands) => PreparedInlineContent::Slash(
            commands
                .iter()
                .skip(window.start)
                .take(window.len)
                .cloned()
                .collect(),
        ),
        PopupContent::At { candidates, .. } => PreparedInlineContent::At(
            candidates
                .iter()
                .skip(window.start)
                .take(window.len)
                .cloned()
                .collect(),
        ),
        PopupContent::Bash { candidates } => PreparedInlineContent::Bash(
            candidates
                .iter()
                .skip(window.start)
                .take(window.len)
                .cloned()
                .collect(),
        ),
        PopupContent::PromptQueue(queue) => PreparedInlineContent::PromptQueue(
            queue
                .iter()
                .skip(window.start)
                .take(window.len)
                .cloned()
                .collect(),
        ),
        PopupContent::Full => return None,
    };
    Some(PreparedInlinePopup {
        area: popup.area,
        window,
        content,
    })
}

pub fn resolve_current(state: &AppState) -> Option<ResolvedPopup<'_>> {
    resolve(state, &crate::layout::current(state), Instant::now())
}

pub(crate) enum AcceptedCompletion {
    At {
        at_byte_index: usize,
        cursor_byte: usize,
        candidate: AtCandidate,
    },
    Bash {
        replacement: String,
    },
    Slash {
        name: String,
    },
}

pub(crate) fn draw(frame: &mut Frame, popup: Option<&PreparedInlinePopup>, opts: &RenderOptions) {
    let Some(popup) = popup.filter(|popup| popup.area.width > 0 && popup.area.height > 0) else {
        return;
    };
    draw::draw(frame, popup, opts);
}

pub(crate) fn prepare_scan_wants(state: &mut AppState, completion: Option<&InputCompletion>) {
    let (at, bash) = match completion {
        Some(InputCompletion::At { key, .. }) => (Some(key.clone()), None),
        Some(InputCompletion::Bash { key }) => (None, Some(key.clone())),
        None => (None, None),
    };
    state.input.at_comp.set_want(at);
    state.input.bash_comp.set_want(bash);
}

pub fn above_input(layout: &ScreenLayout, width: u16, rows: usize) -> Rect {
    let height = layout.preview.y.saturating_sub(layout.dialog.y);
    let container = Rect::new(layout.input.x, layout.dialog.y, layout.input.width, height);
    bottom_area(container, width, (rows.min(MAX_VISIBLE as usize)) as u16)
}

pub fn bottom_area(container: Rect, width: u16, height: u16) -> Rect {
    let Some((width, height)) = fitted(container, width, height) else {
        return Rect::new(container.x, container.y, 0, 0);
    };
    Rect::new(
        container.x + (container.width - width) / 2,
        container.bottom() - height,
        width,
        height,
    )
}

pub fn top_area(container: Rect, width: u16, height: u16, offset: u16) -> Rect {
    let Some((width, height)) = fitted(container, width, height) else {
        return Rect::new(container.x, container.y, 0, 0);
    };
    Rect::new(
        container.x + (container.width - width) / 2,
        container.y + offset.min(container.height - height),
        width,
        height,
    )
}

pub fn center_area(container: Rect, width: u16, height: u16) -> Rect {
    let Some((width, height)) = fitted(container, width, height) else {
        return Rect::new(container.x, container.y, 0, 0);
    };
    Rect::new(
        container.x + (container.width - width) / 2,
        container.y + (container.height - height) / 2,
        width,
        height,
    )
}

fn fitted(container: Rect, width: u16, height: u16) -> Option<(u16, u16)> {
    (container.width > 0 && container.height > 0)
        .then_some((width.min(container.width), height.min(container.height)))
}

pub fn query_cursor(area: Rect, query: &str, prefix_width: u16, row: u16) -> Option<(u16, u16)> {
    if row >= area.height || area.width <= prefix_width {
        return None;
    }
    let max = area.width - prefix_width - 1;
    let column = pacode_render::display_width(query).min(max as usize) as u16;
    Some((area.x + prefix_width + column, area.y + row))
}

fn visible_window(total: usize, selected: usize, height: u16) -> PopupWindow {
    let len = total.min(height as usize);
    let selected = selected.min(total.saturating_sub(1));
    let start = selected
        .saturating_add(1)
        .saturating_sub(len)
        .min(total.saturating_sub(len));
    PopupWindow {
        start,
        len,
        selected,
        total,
    }
}

pub(crate) fn bottom_picker_container(layout: &ScreenLayout) -> Rect {
    if layout.dialog.width > 0 {
        Rect::new(
            layout.dialog.x,
            layout.screen.y,
            layout.dialog.width,
            layout.screen.height,
        )
    } else {
        layout.screen
    }
}

/// The area every centred overlay (menus, settings, analyze, the model
/// palette) is centred in: the whole left column above the image strip and
/// the prompt's top rule — the transcript *and* an open panel. Not the dialog
/// rect: with a panel open that is 45–50 % of the column, and an overlay
/// centred there is squeezed to a sliver beside empty space (B25).
pub(crate) fn overlay_container(layout: &ScreenLayout) -> Rect {
    Rect::new(
        layout.input_top.x,
        layout.dialog.y,
        layout.input_top.width,
        layout.preview.y.saturating_sub(layout.dialog.y),
    )
}

fn full_area(state: &AppState, layout: &ScreenLayout, now: Instant, id: PopupId) -> Rect {
    let elapsed = state
        .overlay_since
        .map(|opened| now.saturating_duration_since(opened).as_millis() as u64);
    if id.is_bottom() {
        let container = bottom_picker_container(layout);
        let area = bottom_area(container, container.width, state.bottom_picker_height());
        return elapsed.map_or(area, |ms| grow_bottom(area, ms));
    }
    let area = center_container(overlay_container(layout), elapsed);
    match id {
        PopupId::RailOverlay => {
            let width = ((area.width as u32 * 78) / 100).max(20) as u16;
            let height = area.height.saturating_sub(4).max(6);
            top_area(area, width, height, 2)
        }
        _ => area,
    }
}

/// The model palette's box in the shared [`overlay_container`]: 44 to 84
/// columns wide (the container's own width below 44), centred across, and
/// `height` rows tall up to [`PALETTE_MAX_HEIGHT`] and the container. The top
/// sits where the tallest box would sit centred, so a filter that shrinks the
/// list shortens the box from below without moving its search line. The
/// container already leaves out the prompt rules and the footer.
pub(crate) fn palette_area(container: Rect, height: u16) -> Rect {
    if container.width == 0 || container.height == 0 {
        return Rect::new(container.x, container.y, 0, 0);
    }
    let width = container
        .width
        .saturating_sub(4)
        .clamp(44.min(container.width), 84);
    let tallest = container.height.min(PALETTE_MAX_HEIGHT);
    Rect::new(
        container.x + (container.width - width) / 2,
        container.y + (container.height - tallest) / 2,
        width,
        height.min(tallest),
    )
}

/// The model palette's tallest box.
pub(crate) const PALETTE_MAX_HEIGHT: u16 = 26;

fn center_container(area: Rect, elapsed: Option<u64>) -> Rect {
    elapsed.map_or(area, |ms| grow_center(area, ms))
}

pub(crate) fn grow_center(area: Rect, elapsed_ms: u64) -> Rect {
    center_area(area, area.width, eased_height(area.height, elapsed_ms))
}

pub(crate) fn grow_bottom(area: Rect, elapsed_ms: u64) -> Rect {
    let height = eased_height(area.height, elapsed_ms);
    Rect::new(area.x, area.bottom() - height, area.width, height)
}

fn eased_height(height: u16, elapsed_ms: u64) -> u16 {
    if height == 0 || elapsed_ms >= crate::state::OVERLAY_GROW_MS {
        return height;
    }
    let elapsed = elapsed_ms as f32 / crate::state::OVERLAY_GROW_MS as f32;
    let eased = 1.0 - (1.0 - elapsed).powi(3);
    ((height as f32) * eased).round().max(1.0) as u16
}

#[cfg(test)]
#[path = "popup_tests.rs"]
mod popup_tests;
