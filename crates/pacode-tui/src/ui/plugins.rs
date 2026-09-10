//! Plugins overlay: what is loaded, and what the marketplace offers.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::{MarketplacePluginInfo, PluginInfo};

use crate::state::PluginsTab;

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod plugins_tests;

/// Everything the overlay draws. Grouped because the two tabs share one frame.
pub struct PluginsView<'a> {
    pub index: usize,
    pub plugins: &'a [PluginInfo],
    pub tab: PluginsTab,
    pub market: &'a [MarketplacePluginInfo],
    pub query: &'a str,
    pub loading: bool,
    pub stale: bool,
    /// The configured marketplace, empty when there is none.
    pub source: &'a str,
}

/// Draw the plugins overlay.
pub fn draw(frame: &mut Frame, area: Rect, view: &PluginsView<'_>, opts: &RenderOptions) {
    let hint = match view.tab {
        PluginsTab::Installed => " tab discover · x uninstall · esc close ",
        PluginsTab::Discover => " tab installed · type to search · i install · esc close ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Plugins ", opts.theme.bold))
        .title_bottom(Span::styled(hint, opts.theme.dim))
        .border_style(opts.theme.dim);

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let mut lines = vec![tab_bar(view, opts)];
    if let Some(status) = status_line(view, opts) {
        lines.push(status);
    }

    let rows = (inner.height as usize).saturating_sub(lines.len());
    match view.tab {
        PluginsTab::Installed => lines.extend(installed_rows(view, inner.width, rows, opts)),
        PluginsTab::Discover => lines.extend(discover_rows(view, inner.width, rows, opts)),
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn tab_bar(view: &PluginsView<'_>, opts: &RenderOptions) -> Line<'static> {
    let mut spans = Vec::new();
    for tab in [PluginsTab::Installed, PluginsTab::Discover] {
        let style = if tab == view.tab {
            opts.theme.selected_bg
        } else {
            opts.theme.faint
        };
        spans.push(Span::styled(format!(" {} ", tab.title()), style));
        spans.push(Span::raw(" "));
    }
    let count = match view.tab {
        PluginsTab::Installed => view.plugins.len(),
        PluginsTab::Discover => view.market.len(),
    };
    spans.push(Span::styled(format!("({count})"), opts.theme.dim));
    Line::from(spans)
}

/// The line under the tabs: the search box, or why a tab is empty.
fn status_line(view: &PluginsView<'_>, opts: &RenderOptions) -> Option<Line<'static>> {
    match view.tab {
        PluginsTab::Installed => None,
        PluginsTab::Discover => {
            if view.source.is_empty() {
                return Some(Line::from(Span::styled(
                    "no marketplace configured — /plugins owner/repo",
                    opts.theme.yellow,
                )));
            }
            let mut spans = vec![
                Span::styled("search: ", opts.theme.faint),
                Span::styled(view.query.to_string(), opts.theme.fg),
            ];
            if view.loading {
                spans.push(Span::styled("  loading…", opts.theme.faint));
            } else if view.stale {
                // A stale list is still useful; saying so beats pretending.
                spans.push(Span::styled("  offline copy", opts.theme.yellow));
            }
            Some(Line::from(spans))
        }
    }
}

fn window(index: usize, len: usize, rows: usize) -> (usize, usize) {
    if len == 0 || rows == 0 {
        return (0, 0);
    }
    let sel = index.min(len - 1);
    let start = if sel >= rows { sel + 1 - rows } else { 0 };
    (start, sel)
}

fn installed_rows(
    view: &PluginsView<'_>,
    width: u16,
    rows: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    if view.plugins.is_empty() {
        return vec![Line::from(Span::styled(
            "No plugins loaded",
            opts.theme.faint,
        ))];
    }
    let dot = if opts.glyphs.ascii { " . " } else { " · " };
    let (start, sel) = window(view.index, view.plugins.len(), rows);

    view.plugins
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, p)| {
            let selected = i == sel;
            let mut spans = vec![
                Span::styled(
                    format!("{} ", if selected { opts.glyphs.pointer } else { " " }),
                    opts.theme.accent,
                ),
                Span::styled(
                    p.name.clone(),
                    if selected {
                        opts.theme.bold
                    } else {
                        opts.theme.fg
                    },
                ),
            ];
            let mut tail = String::new();
            if !p.version.is_empty() {
                tail.push_str(&format!("{dot}{}", p.version));
            }
            if !p.kind.is_empty() {
                tail.push_str(&format!("{dot}{}", p.kind));
            }
            if !p.tools.is_empty() {
                tail.push_str(&format!("{dot}{} tools", p.tools.len()));
            }
            spans.push(Span::styled(
                truncate_to_width(&tail, (width as usize).saturating_sub(4), true),
                opts.theme.dim,
            ));
            if let Some(err) = &p.error {
                spans.push(Span::styled(format!("{dot}{err}"), opts.theme.red));
            }
            Line::from(spans)
        })
        .collect()
}

fn discover_rows(
    view: &PluginsView<'_>,
    width: u16,
    rows: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    if view.source.is_empty() {
        return Vec::new();
    }
    if view.market.is_empty() {
        let text = if view.loading {
            "loading…"
        } else if view.query.is_empty() {
            "this marketplace lists no plugins"
        } else {
            "nothing matches that search"
        };
        return vec![Line::from(Span::styled(text, opts.theme.faint))];
    }

    let dot = if opts.glyphs.ascii { " . " } else { " · " };
    let (start, sel) = window(view.index, view.market.len(), rows);

    view.market
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, p)| {
            let selected = i == sel;
            let marker = if p.installed {
                opts.glyphs.ok
            } else if selected {
                opts.glyphs.pointer
            } else {
                " "
            };
            let marker_style = if p.installed {
                opts.theme.green
            } else {
                opts.theme.accent
            };
            let mut tail = String::new();
            if !p.version.is_empty() {
                tail.push_str(&format!("{dot}{}", p.version));
            }
            if !p.author.is_empty() {
                tail.push_str(&format!("{dot}{}", p.author));
            }
            if p.update_available {
                tail.push_str(&format!("{dot}update"));
            }
            if !p.description.is_empty() {
                tail.push_str(&format!("{dot}{}", p.description));
            }

            let head_w = pacode_render::display_width(&p.name) + 2;
            Line::from(vec![
                Span::styled(format!("{marker} "), marker_style),
                Span::styled(
                    p.name.clone(),
                    if selected {
                        opts.theme.bold
                    } else {
                        opts.theme.fg
                    },
                ),
                Span::styled(
                    truncate_to_width(&tail, (width as usize).saturating_sub(head_w), true),
                    if p.update_available {
                        opts.theme.yellow
                    } else {
                        opts.theme.dim
                    },
                ),
            ])
        })
        .collect()
}
