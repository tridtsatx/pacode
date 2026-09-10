//! Plugins overlay: what is installed, and what the marketplace offers.
//!
//! The Installed tab merges plugins live in the runtime with plugins the
//! marketplace installed (they share a row when a name matches), so a plugin
//! that only ships skills or MCP servers is still visible.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::{RenderOptions, truncate_to_width};
use pacode_types::time::now_ms;
use pacode_types::{MarketplacePluginInfo, PluginInfo};

use crate::state::PluginsTab;
use crate::ui::overlays::{age_ago, empty_lines, menu_block, selected_row, window_groups};

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
    let block = menu_block("Plugins", hint, opts);

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    let mut lines = vec![tab_bar(view, opts)];
    if let Some(status) = status_line(view, opts) {
        lines.push(status);
    }

    let budget = (inner.height as usize).saturating_sub(lines.len());
    match view.tab {
        PluginsTab::Installed => lines.extend(installed_rows(view, inner.width, budget, opts)),
        PluginsTab::Discover => lines.extend(discover_rows(view, inner.width, budget, opts)),
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn tab_bar(view: &PluginsView<'_>, opts: &RenderOptions) -> Line<'static> {
    let counts = [view.plugins.len(), view.market.len()];
    let mut spans = Vec::new();
    for (tab, count) in [PluginsTab::Installed, PluginsTab::Discover]
        .into_iter()
        .zip(counts)
    {
        let style = if tab == view.tab {
            opts.theme.selected_bg
        } else {
            opts.theme.faint
        };
        spans.push(Span::styled(format!(" {} ", tab.title()), style));
        spans.push(Span::styled(format!("{count} "), opts.theme.dim));
    }
    Line::from(spans)
}

/// The line under the tabs: the search box, or why a tab is empty.
fn status_line(view: &PluginsView<'_>, opts: &RenderOptions) -> Option<Line<'static>> {
    match view.tab {
        PluginsTab::Installed => None,
        PluginsTab::Discover => {
            if view.source.is_empty() {
                return None;
            }
            let mut spans = vec![
                Span::styled("> ", opts.theme.accent),
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

/// The lines one installed plugin takes: a head row, then a description, a
/// notes row and an unsupported-components warning as the data exists.
fn installed_lines(
    p: &PluginInfo,
    is_sel: bool,
    width: usize,
    now: u64,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let sep = opts.glyphs.dot_sep;
    // Runtime-loaded plugins get the filled dot; marketplace-only ones the ring.
    let (dot, dot_style) = if p.loaded {
        (opts.glyphs.main_dot, opts.theme.green)
    } else {
        (opts.glyphs.agent_dot, opts.theme.dim)
    };

    let mut tail = String::new();
    if !p.version.is_empty() {
        tail.push_str(&format!("{sep}{}", p.version));
    }
    if !p.kind.is_empty() {
        tail.push_str(&format!("{sep}{}", p.kind));
    }
    if !p.tools.is_empty() {
        tail.push_str(&format!("{sep}{} tools", p.tools.len()));
    }
    if !p.commands.is_empty() {
        tail.push_str(&format!("{sep}{} commands", p.commands.len()));
    }

    let head_w = 4 + pacode_render::display_width(&p.name);
    let mut head_spans = vec![
        Span::styled(
            format!("{} ", if is_sel { opts.glyphs.pointer } else { " " }),
            opts.theme.accent,
        ),
        Span::styled(format!("{dot} "), dot_style),
        Span::styled(p.name.clone(), opts.theme.bold),
        Span::styled(
            truncate_to_width(&tail, width.saturating_sub(head_w), opts.glyphs.ascii),
            opts.theme.dim,
        ),
    ];
    if let Some(ref err) = p.error {
        let avail = width.saturating_sub(head_w + pacode_render::display_width(&tail) + 4);
        if avail > 4 {
            head_spans.push(Span::styled(
                format!("{sep}{}", truncate_to_width(err, avail, opts.glyphs.ascii)),
                opts.theme.red,
            ));
        }
    }
    let mut lines = vec![Line::from(head_spans)];

    if !p.description.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                truncate_to_width(&p.description, width.saturating_sub(4), opts.glyphs.ascii),
                opts.theme.dim,
            ),
        ]));
    }

    // Where the plugin came from and what it contributes beyond tools.
    let mut notes: Vec<String> = Vec::new();
    if !p.source.is_empty() {
        notes.push(p.source.clone());
    }
    if !p.mcp_servers.is_empty() {
        notes.push(format!("mcp: {}", p.mcp_servers.join(", ")));
    }
    if p.skill_dirs > 0 {
        notes.push(format!("{} skills", p.skill_dirs));
    }
    if !p.author.is_empty() {
        notes.push(format!("by {}", p.author));
    }
    if p.installed_at_ms > 0 {
        notes.push(format!("installed {}", age_ago(p.installed_at_ms, now)));
    }
    if !notes.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                truncate_to_width(&notes.join(sep), width.saturating_sub(4), true),
                opts.theme.faint,
            ),
        ]));
    }

    if !p.unsupported.is_empty() {
        let warn = format!(
            "! {} are declared but not applied",
            p.unsupported.join(", ")
        );
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                truncate_to_width(&warn, width.saturating_sub(4), opts.glyphs.ascii),
                opts.theme.yellow,
            ),
        ]));
    }

    lines
}

fn installed_rows(
    view: &PluginsView<'_>,
    width: u16,
    budget: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    if view.plugins.is_empty() {
        let dash = if opts.glyphs.ascii { "-" } else { "—" };
        return empty_lines(
            "No plugins installed",
            &format!("tab Discover {dash} browse the marketplace"),
            opts,
        );
    }

    let now = now_ms();
    let sel = view.index.min(view.plugins.len() - 1);
    let groups: Vec<Vec<Line<'static>>> = view
        .plugins
        .iter()
        .enumerate()
        .map(|(i, p)| installed_lines(p, i == sel, width as usize, now, opts))
        .collect();
    let heights: Vec<usize> = groups.iter().map(Vec::len).collect();
    let start = window_groups(&heights, sel, budget);

    let mut lines = Vec::new();
    for (i, group) in groups.iter().enumerate().skip(start) {
        // A group that alone exceeds the budget still renders — the paragraph
        // clips it — because the selected plugin must always be on screen.
        if i > sel && lines.len() + group.len() > budget {
            break;
        }
        for line in group {
            if i == sel {
                lines.push(selected_row(line.clone(), width, opts));
            } else {
                lines.push(line.clone());
            }
        }
    }
    lines
}

fn discover_rows(
    view: &PluginsView<'_>,
    width: u16,
    budget: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    if view.source.is_empty() {
        return empty_lines(
            "No marketplace configured",
            "/plugins owner/repo to browse one",
            opts,
        );
    }
    if view.market.is_empty() {
        return if view.loading {
            vec![Line::from(Span::styled("loading…", opts.theme.faint))]
        } else if view.query.is_empty() {
            empty_lines(
                "This marketplace lists no plugins",
                "try another source with /plugins owner/repo",
                opts,
            )
        } else {
            empty_lines(
                "Nothing matches that search",
                "backspace to clear the filter",
                opts,
            )
        };
    }

    let sep = opts.glyphs.dot_sep;
    // Each entry is two rows: name + version + author, then the description.
    let (start, sel) = window(view.index, view.market.len(), budget / 2);

    view.market
        .iter()
        .enumerate()
        .skip(start)
        .take(budget / 2)
        .flat_map(|(i, p)| {
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
                tail.push_str(&format!("{sep}{}", p.version));
            }
            if !p.author.is_empty() {
                tail.push_str(&format!("{sep}{}", p.author));
            }
            if p.update_available {
                tail.push_str(&format!("{sep}update"));
            }
            if let Some(ref v) = p.installed_version {
                tail.push_str(&format!("{sep}installed {v}"));
            }

            let head_w = 6 + pacode_render::display_width(&p.name);
            let head = Line::from(vec![
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
            ]);

            let mut sub_spans = vec![Span::raw("    ")];
            if !p.description.is_empty() {
                sub_spans.push(Span::styled(
                    truncate_to_width(
                        &p.description,
                        (width as usize).saturating_sub(4),
                        opts.glyphs.ascii,
                    ),
                    opts.theme.dim,
                ));
            }
            if !p.category.is_empty() {
                sub_spans.push(Span::styled(
                    format!("{sep}{}", p.category),
                    opts.theme.faint,
                ));
            }
            if !p.marketplace.is_empty() {
                sub_spans.push(Span::styled(
                    format!("{sep}{}", p.marketplace),
                    opts.theme.faint,
                ));
            }
            let sub = Line::from(sub_spans);

            if selected {
                vec![
                    selected_row(head, width, opts),
                    selected_row(sub, width, opts),
                ]
            } else {
                vec![head, sub]
            }
        })
        .collect()
}
