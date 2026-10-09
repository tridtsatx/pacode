//! Drawing for the model palette: the box, the search line, the grouped rows,
//! the effort line and the hints. The rows and the rules live in
//! [`crate::ui::model_palette`]; this module only paints them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use pacode_render::{RenderOptions, display_width, truncate_to_width};
use pacode_types::ModelInfo;

use crate::binding::Action as KeyAction;
use crate::state::AppState;
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

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    palette: &ModelPalette,
    opts: &RenderOptions,
) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let src = Sources::of(state);
    let rows = palette.rows(&src);
    let selected = palette.selected_index(&rows);
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
    let title = title_with_badge(&base, state.lists.badge(crate::cache::ListKind::Models));
    let chords = chord_hint(state, &src, highlighted, opts);
    let block = menu_block(
        &title,
        &truncate_to_width(&chords, usize::from(area.width.saturating_sub(4)), true),
        opts,
    );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", opts.glyphs.prompt), opts.theme.accent),
            Span::styled(palette.query.clone(), opts.theme.fg),
        ]),
        Line::default(),
    ];
    let list_h = usize::from(inner.height.saturating_sub(CHROME_ROWS));
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
        let columns = Columns::fit(&rows, width);
        let current = state.meta.as_ref().map(|meta| meta.model.to_string());
        let start = window_start(selected.unwrap_or(0), list_h);
        for (index, row) in rows.iter().enumerate().skip(start).take(list_h) {
            let is_sel = selected == Some(index);
            let line = match row {
                Row::Gap => Line::default(),
                Row::Header {
                    title,
                    provider,
                    status,
                } => header_line(title, provider.as_deref(), status, is_sel, palette, opts),
                Row::Model { model, recent, .. } => {
                    let is_current = current.as_deref() == Some(&model.route.to_string());
                    model_line(model, *recent, is_sel, is_current, &columns, &src, opts)
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
    frame.render_widget(Paragraph::new(lines), inner);

    if let Some(position) = crate::ui::popup::query_cursor(inner, &palette.query, 2, 0) {
        frame.set_cursor_position(position);
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

fn header_line(
    title: &str,
    provider: Option<&str>,
    status: &GroupStatus,
    selected: bool,
    palette: &ModelPalette,
    opts: &RenderOptions,
) -> Line<'static> {
    let pointer = if selected { opts.glyphs.pointer } else { " " };
    let mut spans = vec![
        Span::styled(format!("{pointer} "), opts.theme.accent),
        Span::styled(title.to_string(), opts.theme.bold),
    ];
    let login = palette
        .login
        .as_ref()
        .filter(|login| provider == Some(login.provider.as_str()));
    let status = match (login, status) {
        (Some(login), _) if login.failed => Some((format!("! {}", login.text), opts.theme.red)),
        (Some(login), _) => Some((
            format!("{} {}", opts.glyphs.running, login.text),
            opts.theme.yellow,
        )),
        (None, GroupStatus::Plain) => None,
        (None, GroupStatus::SignedIn) => Some((
            format!("{} signed in", opts.glyphs.main_dot),
            opts.theme.green,
        )),
        (None, GroupStatus::SignIn) => Some((
            format!("{} enter to sign in", opts.glyphs.agent_dot),
            opts.theme.dim,
        )),
        (None, GroupStatus::Error(reason)) => Some((format!("! {reason}"), opts.theme.red)),
    };
    if let Some((text, style)) = status {
        spans.push(Span::styled(format!("  {text}"), style));
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
    if auth.is_some_and(|info| info.accounts.len() > 1) {
        items.push((KeyAction::ModelPickerAccount, "account"));
    }
    if auth.is_some_and(|info| !info.accounts.is_empty()) {
        items.push((KeyAction::ModelPickerLogout, "logout"));
    }
    let parts: Vec<String> = items
        .into_iter()
        .filter_map(|(action, label)| {
            let chord = state.keymap.bindings_for(action).first()?;
            Some(format!("{} {label}", crate::binding::format_binding(chord)))
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" {} ", parts.join(opts.glyphs.dot_sep))
}

/// First row of a window of `height` rows that keeps `selected` on screen.
fn window_start(selected: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    (selected + 1).saturating_sub(height)
}
