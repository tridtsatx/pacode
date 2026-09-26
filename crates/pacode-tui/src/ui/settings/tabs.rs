//! The tab row: one tab per registry section, drawn from the shared glyph
//! table so a terminal without the glyphs falls back to ASCII instead of tofu.
//!
//! A terminal wide enough shows every tab. A narrow one shows a window around
//! the active tab, marked with ellipses, because a bar of icons that says
//! nothing about which section is open is worse than a bar with fewer icons.

use std::ops::Range;

use pacode_render::Glyphs;
use pacode_render::display_width;

#[cfg(test)]
#[path = "tabs_tests.rs"]
mod tabs_tests;

/// One section's tab. Inactive tabs are an icon alone; the active one spells
/// its section out, so the bar answers "where am I" on its own when the section
/// column has been dropped for width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tab {
    pub section: usize,
    pub name: &'static str,
    pub icon: &'static str,
    pub active: bool,
}

impl Tab {
    /// Cells this tab costs: a leading separator, the icon, and the name on the
    /// active one.
    pub fn width(&self) -> usize {
        1 + display_width(self.icon) + if self.active { 1 + display_width(self.name) } else { 0 }
    }
}

/// The visible slice of the tab row, and whether either end is cut off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabBar {
    pub range: Range<usize>,
    pub clipped_left: bool,
    pub clipped_right: bool,
}

impl TabBar {
    /// Everything fits: no window, no ellipses.
    fn whole(count: usize) -> Self {
        Self {
            range: 0..count,
            clipped_left: false,
            clipped_right: false,
        }
    }
}

/// One tab per section, in [`super::model::section_order`] order. The active
/// tab is the one whose section the overlay is showing.
pub fn tabs(sections: &[&'static str], glyphs: &Glyphs, active: usize) -> Vec<Tab> {
    sections
        .iter()
        .enumerate()
        .map(|(index, &name)| Tab {
            section: index,
            name,
            icon: section_icon(name, glyphs),
            active: index == active,
        })
        .collect()
}

/// The tab row's icon for `section`.
///
/// The glyph table has no per-domain icons, so the mapping reuses marks that
/// already mean something — a filled bar is a ceiling, a caret is a bigger
/// font, a corner is a leaf capability. A section the map does not know gets
/// the main dot rather than nothing: an added section has to be reachable, and
/// a reachable section has to look like a tab.
pub fn section_icon(section: &str, glyphs: &Glyphs) -> &'static str {
    match section {
        "provider" => glyphs.running,
        "ui" => glyphs.pointer,
        "exec" => glyphs.prompt,
        "limits" => glyphs.bar_full,
        "daemon" => glyphs.main_dot,
        "context" => glyphs.vline,
        "permissions" => glyphs.ok,
        "mcp" => glyphs.agent_dot,
        "session" => glyphs.chevrons,
        "plugins" => glyphs.hline,
        "skills" => glyphs.vcorner,
        "tools" => glyphs.tool,
        "theme" => glyphs.bar_empty,
        "font" => glyphs.arrow_up,
        "keys" => glyphs.arrow_right,
        "web" => glyphs.arrow_down,
        _ => glyphs.main_dot,
    }
}

/// The window of tabs a bar `width` cells wide shows, always holding the active
/// one. Grows to the right first, then to the left, so the active tab does not
/// jump around as the selection moves down the list.
pub fn window(tabs: &[Tab], width: usize, ellipsis: &str) -> TabBar {
    let count = tabs.len();
    if count == 0 {
        return TabBar::whole(0);
    }
    if tabs.iter().map(Tab::width).sum::<usize>() <= width {
        return TabBar::whole(count);
    }
    let active = tabs
        .iter()
        .position(|tab| tab.active)
        .unwrap_or(0)
        .min(count - 1);
    // An ellipsis plus the space that separates it from the next tab.
    let marker = display_width(ellipsis) + 1;
    // Reserve the markers only on the ends the window actually cuts. The
    // reservation is a promise to the renderer, so each candidate is measured
    // against the row it would really draw.
    for (head, tail) in [(0, 0), (0, marker), (marker, 0), (marker, marker)] {
        if let Some(bar) = try_window(tabs, active, width, head, tail) {
            return bar;
        }
    }
    // Too narrow even for the active tab: it is still drawn, clipped, and the
    // markers stay only if they fit. Claiming a cut the bar cannot show is
    // worse than showing the tab on its own.
    let shown = tabs[active].width();
    let markers_fit = shown + 2 * marker <= width;
    TabBar {
        range: active..active + 1,
        clipped_left: active > 0 && markers_fit,
        clipped_right: active + 1 < count && markers_fit,
    }
}

fn try_window(
    tabs: &[Tab],
    active: usize,
    width: usize,
    head: usize,
    tail: usize,
) -> Option<TabBar> {
    let count = tabs.len();
    let mut start = active;
    let mut end = active + 1;
    while end < count && head + span(&tabs[start..end + 1]) + tail <= width {
        end += 1;
    }
    while start > 0 && head + span(&tabs[start - 1..end]) + tail <= width {
        start -= 1;
    }
    let bar = TabBar {
        range: start..end,
        clipped_left: start > 0,
        clipped_right: end < count,
    };
    let drawn = if bar.clipped_left { head } else { 0 }
        + span(&tabs[bar.range.clone()])
        + if bar.clipped_right { tail } else { 0 };
    if drawn > width {
        return None;
    }
    Some(bar)
}

fn span(tabs: &[Tab]) -> usize {
    tabs.iter().map(Tab::width).sum()
}
