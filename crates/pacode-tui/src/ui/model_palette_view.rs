//! Drawing for the model palette: the box, the search line, the grouped rows,
//! the effort line and the hints. The rows and the rules live in
//! [`crate::ui::model_palette`]; this module only paints them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::{ModelInfo, ModelKind};

use crate::binding::Action as KeyAction;
use crate::state::{AppState, Focus, Overlay};
use crate::ui::chrome;
use crate::ui::model_catalog::{Kind, accepted_efforts, capability_tags, ctx_label, price_label};
use crate::ui::model_palette::{GroupStatus, ModelPalette, Row, Sources};
use crate::ui::overlays::{menu_block, selected_row, title_with_badge};

/// Rows inside the border that are not the list: search, spacer, effort, hint.
const CHROME_ROWS: u16 = 4;
/// The narrowest name column before a right-hand column is dropped.
const MIN_NAME: usize = 14;
const CTX_W: usize = 5;
/// Favourite + ZDR marks, two cells each.
const MARKS_W: usize = 4;

/// The palette as one frame draws it, built from a single pass over its rows.
pub(crate) struct PreparedPalette {
    /// The window start for the box's full size (not the unfolding one):
    /// `prepare_frame` stores it so the next frame starts from it.
    scroll: usize,
    /// `None` when the box is too small to draw anything.
    content: Option<PaletteContent>,
}

struct PaletteContent {
    block: Block<'static>,
    lines: Vec<Line<'static>>,
    cursor: Option<(u16, u16)>,
}

/// Build the palette's rows once and derive the whole frame from them: the
/// box in `container` (sized to the rows, grown while it unfolds after
/// `elapsed` ms), the window it scrolls to and the lines it draws. Nothing is
/// kept across frames.
pub(crate) fn prepare(
    state: &AppState,
    palette: &ModelPalette,
    container: Rect,
    elapsed: Option<u64>,
    opts: &RenderOptions,
) -> (Rect, PreparedPalette) {
    let src = Sources::of(state);
    let rows = palette.rows(&src);
    let selected = palette.selected_index(&rows);
    let full = crate::ui::popup::palette_area(container, box_height(rows.len()));
    let scroll = ModelPalette::window_from(palette.scroll, &rows, selected, list_height(full));
    let area = elapsed.map_or(full, |ms| crate::ui::popup::grow_center(full, ms));
    let content = (area.width >= 4 && area.height >= 3).then(|| {
        let view = View {
            state,
            palette,
            src: &src,
            rows: &rows,
            selected,
            scroll,
        };
        view.content(area, opts)
    });
    (area, PreparedPalette { scroll, content })
}

/// Keep the window this frame resolved, so moving inside it next frame does
/// not shift it.
pub(crate) fn store_scroll(state: &mut AppState, prepared: &PreparedPalette) {
    if let Focus::Overlay(Overlay::ModelPalette { palette }) = &mut state.session.focus {
        palette.scroll = prepared.scroll;
    }
}

pub(crate) fn draw(
    frame: &mut Frame,
    area: Rect,
    prepared: &PreparedPalette,
    opts: &RenderOptions,
) {
    let Some(content) = prepared.content.as_ref() else {
        return;
    };
    let inner = content.block.inner(area);
    crate::ui::overlays::clear_panel(frame, area, opts);
    frame.render_widget(content.block.clone(), area);
    frame.render_widget(Paragraph::new(content.lines.clone()), inner);
    if let Some(position) = content.cursor {
        frame.set_cursor_position(position);
    }
}

/// What one frame's content is built from.
struct View<'v, 'a> {
    state: &'v AppState,
    palette: &'v ModelPalette,
    src: &'v Sources<'a>,
    rows: &'v [Row<'a>],
    selected: Option<usize>,
    /// The stored window start for the full-size box.
    scroll: usize,
}

impl View<'_, '_> {
    fn content(&self, area: Rect, opts: &RenderOptions) -> PaletteContent {
        let View {
            state,
            palette,
            src,
            rows,
            selected,
            scroll,
        } = *self;
        let highlighted = selected.and_then(|index| rows.get(index));

        let base = if palette.for_zdr {
            "ZDR: pick a capable model".to_string()
        } else {
            "Select model".to_string()
        };
        let base = match palette.kind {
            Kind::All => base,
            kind => format!("{base}{}{}", opts.glyphs.dot_sep, kind.label()),
        };
        let base = if palette.all_types {
            format!("{base}{}all types", opts.glyphs.dot_sep)
        } else {
            base
        };
        let title = title_with_badge(&base, state.lists.badge(crate::cache::ListKind::Models));
        let chords = chord_hint(state, src, highlighted, opts);
        let block = menu_block(
            &title,
            &truncate_to_width(&chords, usize::from(area.width.saturating_sub(4)), true),
            opts,
        );
        let inner = block.inner(area);

        let width = usize::from(inner.width);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{} ", opts.glyphs.prompt), opts.theme.accent),
                Span::styled(palette.query.clone(), opts.theme.fg),
            ]),
            Line::default(),
        ];
        let list_h = list_height(area);
        if rows.is_empty() {
            if state.models.is_empty() && state.auth_providers.is_empty() {
                lines.push(Line::from(Span::styled("  loading…", opts.theme.faint)));
            } else {
                lines.push(Line::from(Span::styled("  no matches", opts.theme.dim)));
                lines.push(Line::from(Span::styled(
                    "  backspace to widen the search",
                    opts.theme.faint,
                )));
            }
        } else {
            let columns = Columns::fit(rows, width);
            let current = state.meta.as_ref().map(|meta| meta.model.to_string());
            let logout_chord = state
                .keymap
                .hint(KeyAction::ModelPickerLogout)
                .unwrap_or_default();
            // The unfolding box is shorter than the full one: it windows from
            // the full box's start, as the full box will.
            let start = ModelPalette::window_from(scroll, rows, selected, list_h);
            for (index, row) in rows.iter().enumerate().skip(start).take(list_h) {
                let is_sel = selected == Some(index);
                let line = match row {
                    Row::Gap => Line::default(),
                    Row::Header {
                        title,
                        provider,
                        status,
                        kind,
                    } => header_line(
                        HeaderView {
                            title,
                            provider: provider.as_deref(),
                            status,
                            kind: *kind,
                            selected: is_sel,
                        },
                        palette,
                        &logout_chord,
                        opts,
                    ),
                    Row::Model { model, recent, .. } => {
                        let is_current = current.as_deref() == Some(&model.route.to_string());
                        model_line(model, *recent, is_sel, is_current, &columns, src, opts)
                    }
                };
                lines.push(if is_sel {
                    selected_row(line, inner.width, opts)
                } else {
                    line
                });
            }
        }
        while lines.len() < usize::from(inner.height.saturating_sub(2)) {
            lines.push(Line::default());
        }
        lines.truncate(usize::from(inner.height.saturating_sub(2)));
        lines.push(effort_line(palette, highlighted, opts));
        lines.push(hint_line(highlighted, width, opts));
        PaletteContent {
            block,
            lines,
            cursor: crate::ui::popup::query_cursor(inner, &palette.query, 2, 0),
        }
    }
}

/// Which right-hand columns fit, and how wide the price column is.
struct Columns {
    name: usize,
    price: Option<usize>,
    tags: Option<usize>,
}

impl Columns {
    fn fit(rows: &[Row], width: usize) -> Self {
        let models = rows.iter().filter_map(|row| match row {
            Row::Model { model, .. } => Some(*model),
            Row::Gap | Row::Header { .. } => None,
        });
        let (price_w, tags_w) = models.fold((0, 0), |(p, t), model| {
            (
                p.max(display_width(&price_label(model.pricing))),
                t.max(capability_tags(model).len()),
            )
        });
        // Pointer column, then the name, then the right-hand cells.
        let avail = width.saturating_sub(2 + CTX_W + MARKS_W);
        let mut price = Some(price_w.min(13));
        let mut tags = (tags_w > 0).then_some(tags_w);
        let used = |p: Option<usize>, t: Option<usize>| {
            p.map_or(0, |w| w + 2) + t.map_or(0, |w| w + 2) + 1
        };
        if avail.saturating_sub(used(price, tags)) < MIN_NAME {
            tags = None;
        }
        if avail.saturating_sub(used(price, tags)) < MIN_NAME {
            price = None;
        }
        Self {
            name: avail.saturating_sub(used(price, tags)),
            price,
            tags,
        }
    }
}

fn model_line(
    model: &ModelInfo,
    recent: bool,
    selected: bool,
    current: bool,
    columns: &Columns,
    src: &Sources,
    opts: &RenderOptions,
) -> Line<'static> {
    let pointer = if selected { opts.glyphs.pointer } else { " " };
    let name = truncate_to_width(&model.display_name, columns.name, true);
    let mut used = display_width(&name);
    let name_style = if current {
        opts.theme.accent.patch(opts.theme.bold)
    } else if selected {
        opts.theme.bold
    } else {
        opts.theme.fg
    };
    let mut spans = vec![
        Span::styled(format!("{pointer} "), opts.theme.accent),
        Span::styled(name, name_style),
    ];
    // A recent row can come from any provider, so it says which.
    if recent && used + 3 < columns.name {
        let provider = truncate_to_width(&model.route.provider, columns.name - used - 2, true);
        used += 2 + display_width(&provider);
        spans.push(Span::styled(format!("  {provider}"), opts.theme.faint));
    }
    let mut right = format!(
        "{}{:>CTX_W$}",
        " ".repeat(columns.name - used + 1),
        ctx_label(model.context_window)
    );
    if let Some(w) = columns.price {
        let price = truncate_to_width(&price_label(model.pricing), w, true);
        right.push_str(&format!("  {price:<w$}"));
    }
    if let Some(w) = columns.tags {
        right.push_str(&format!("  {:<w$}", capability_tags(model)));
    }
    spans.push(Span::styled(right, opts.theme.dim));
    let flags = src
        .flags
        .get(&model.route.to_string())
        .copied()
        .unwrap_or_default();
    let star = if opts.glyphs.ascii { "*" } else { "★" };
    spans.push(Span::styled(
        if flags.favorite {
            format!(" {star}")
        } else {
            "  ".to_string()
        },
        opts.theme.yellow,
    ));
    spans.push(Span::styled(
        if flags.zdr_capable { " Z" } else { "  " },
        opts.theme.green,
    ));
    Line::from(spans)
}

struct HeaderView<'r> {
    title: &'r str,
    provider: Option<&'r str>,
    status: &'r GroupStatus,
    kind: Option<&'static str>,
    selected: bool,
}

fn header_line(
    header: HeaderView,
    palette: &ModelPalette,
    logout_chord: &str,
    opts: &RenderOptions,
) -> Line<'static> {
    let HeaderView {
        title,
        provider,
        status,
        kind,
        selected,
    } = header;
    let pointer = if selected { opts.glyphs.pointer } else { " " };
    let mut spans = vec![
        Span::styled(format!("{pointer} "), opts.theme.accent),
        chrome::section(title, opts),
    ];
    if provider.is_some() && provider == palette.confirm_logout.as_deref() {
        spans.push(Span::raw(" "));
        spans.push(chrome::badge("!", opts.theme.red, opts));
        spans.push(Span::styled(
            format!(" {logout_chord} again to sign out {title}"),
            opts.theme.red.patch(opts.theme.bold),
        ));
        return Line::from(spans);
    }
    let login = palette
        .login
        .as_ref()
        .filter(|login| provider == Some(login.provider.as_str()));
    // The status mark is a badge in the status tone, the words beside it in
    // the same tone — the toast idiom, so it survives ANSI-16 and mono.
    let status: Option<(&str, String, _)> = match (login, status) {
        (Some(login), _) if login.failed => Some(("!", login.text.clone(), opts.theme.red)),
        (Some(login), _) => Some((opts.glyphs.running, login.text.clone(), opts.theme.yellow)),
        (None, GroupStatus::Plain) => None,
        (None, GroupStatus::SignedIn) => Some((
            opts.glyphs.main_dot,
            "signed in".to_string(),
            opts.theme.green,
        )),
        (None, GroupStatus::SignIn) => Some((
            opts.glyphs.agent_dot,
            "enter to sign in".to_string(),
            opts.theme.dim,
        )),
        (None, GroupStatus::Error(reason)) => Some(("!", reason.clone(), opts.theme.red)),
    };
    if let Some((mark, text, tone)) = status {
        spans.push(Span::raw(" "));
        spans.push(chrome::badge(mark, tone, opts));
        spans.push(Span::styled(format!(" {text}"), tone));
    }
    if let Some(kind) = kind {
        spans.push(Span::styled(format!("  {kind}"), opts.theme.faint));
    }
    Line::from(spans)
}

fn effort_line(
    palette: &ModelPalette,
    highlighted: Option<&Row>,
    opts: &RenderOptions,
) -> Line<'static> {
    let mut spans = vec![Span::styled("effort  ", opts.theme.dim)];
    let model = match highlighted {
        Some(Row::Model { model, .. }) => model,
        Some(Row::Header {
            status: GroupStatus::SignedIn | GroupStatus::Plain,
            ..
        }) => {
            spans.push(Span::styled("pick a model below", opts.theme.faint));
            return Line::from(spans);
        }
        Some(Row::Header { .. }) => {
            spans.push(Span::styled("sign in to see models", opts.theme.faint));
            return Line::from(spans);
        }
        Some(Row::Gap) | None => {
            spans.push(Span::styled("—", opts.theme.faint));
            return Line::from(spans);
        }
    };
    let accepted = accepted_efforts(model);
    if accepted.is_empty() {
        spans.push(Span::styled("not adjustable", opts.theme.faint));
        return Line::from(spans);
    }
    let current = palette.effort_for(model);
    for (index, effort) in accepted.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(opts.glyphs.dot_sep, opts.theme.dim));
        }
        spans.push(if Some(effort) == current {
            Span::styled(
                format!("[{effort}]"),
                opts.theme.accent.patch(opts.theme.bold),
            )
        } else {
            Span::styled(effort.to_string(), opts.theme.dim)
        });
    }
    Line::from(spans)
}

fn hint_line(highlighted: Option<&Row>, width: usize, opts: &RenderOptions) -> Line<'static> {
    let arrows = if opts.glyphs.ascii { "^v" } else { "↑↓" };
    let enter = match highlighted {
        Some(Row::Header {
            status: GroupStatus::SignedIn,
            ..
        }) => "enter add account",
        Some(Row::Model {
            signed_in: false, ..
        })
        | Some(Row::Header { .. }) => "enter sign in",
        Some(Row::Model { .. }) | Some(Row::Gap) | None => "enter select",
    };
    let hint = [
        format!("{arrows} move"),
        enter.to_string(),
        "tab effort".to_string(),
        "esc close".to_string(),
    ]
    .join(opts.glyphs.dot_sep);
    Line::from(Span::styled(
        truncate_to_width(&hint, width, true),
        opts.theme.dim,
    ))
}

/// The palette's own chords, spelled from the live keymap so a rebound chord
/// shows its own spelling and an unbound one drops out. Account and logout
/// appear only where they would do something.
fn chord_hint(
    state: &AppState,
    src: &Sources,
    highlighted: Option<&Row>,
    opts: &RenderOptions,
) -> String {
    let auth = highlighted
        .and_then(Row::provider)
        .and_then(|provider| src.auth.iter().find(|info| info.id == provider));
    let mut items = vec![
        (KeyAction::ModelPickerFavorite, "fav"),
        (KeyAction::ModelPickerZdr, "zdr"),
        (KeyAction::ModelPickerKind, "kind"),
        (KeyAction::ModelPickerRefresh, "refresh"),
    ];
    // Only a catalog that has something besides chat models has anything to
    // show or hide.
    if src.models.iter().any(|model| model.kind != ModelKind::Chat) {
        items.push((KeyAction::ModelPickerTypes, "types"));
    }
    if auth.is_some_and(|info| info.accounts.len() > 1) {
        items.push((KeyAction::ModelPickerAccount, "account"));
    }
    if auth.is_some_and(|info| !info.accounts.is_empty()) {
        items.push((KeyAction::ModelPickerLogout, "logout"));
    }
    let parts: Vec<String> = items
        .into_iter()
        .filter_map(|(action, label)| Some(format!("{} {label}", state.keymap.hint(action)?)))
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" {} ", parts.join(opts.glyphs.dot_sep))
}

/// List rows a palette box of `area` holds: the box less its border and the
/// search, spacer, effort and hint rows.
fn list_height(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(2 + CHROME_ROWS))
}

/// The list never shrinks below this, so the empty and loading states and a
/// one-model search still read as a dialog.
const MIN_LIST: usize = 4;

/// The box height `list_rows` rows ask for: border, chrome, the rows and one
/// spacer above the effort line.
fn box_height(list_rows: usize) -> u16 {
    let list = list_rows.saturating_add(1).max(MIN_LIST);
    u16::try_from(list)
        .unwrap_or(u16::MAX)
        .saturating_add(2 + CHROME_ROWS)
}
