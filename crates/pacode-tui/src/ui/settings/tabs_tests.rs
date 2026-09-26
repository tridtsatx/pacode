//! The tab row: what it draws at a width that fits it, and what it gives up
//! at a width that does not.

use pacode_render::Glyphs;

use super::*;
use crate::ui::settings::model::section_order;

fn glyphs(ascii: bool) -> Glyphs {
    Glyphs::new(ascii)
}

#[test]
fn every_section_gets_a_tab_with_a_glyph() {
    let order = section_order();
    let bar = tabs(&order, &glyphs(false), 0);
    assert_eq!(bar.len(), order.len());
    for (index, tab) in bar.iter().enumerate() {
        assert_eq!(tab.name, order[index]);
        assert!(!tab.icon.is_empty(), "{} has no icon", tab.name);
    }
    // A section the map has never seen still looks like a tab, or an added
    // section would show up as a nameless gap.
    assert!(!section_icon("brand-new", &glyphs(false)).is_empty());
}

#[test]
fn the_active_tab_is_the_one_asked_for() {
    let order = section_order();
    let active = order.len() - 1;
    let bar = tabs(&order, &glyphs(false), active);
    assert!(bar[active].active);
    assert_eq!(bar.iter().filter(|t| t.active).count(), 1);
    // Naming the active section is what makes the bar readable on its own.
    assert!(bar[active].width() > bar[0].width());
}

#[test]
fn a_bar_wide_enough_shows_every_tab() {
    let order = section_order();
    let bar = tabs(&order, &glyphs(false), 3);
    let total: usize = bar.iter().map(Tab::width).sum();
    let window = window(&bar, total, "…");
    assert_eq!(window.range, 0..bar.len());
    assert!(!window.clipped_left && !window.clipped_right);
}

#[test]
fn a_narrow_bar_keeps_the_active_tab_and_fits_the_width() {
    let order = section_order();
    for active in 0..order.len() {
        let bar = tabs(&order, &glyphs(false), active);
        for width in [4usize, 12, 24, 38, 60] {
            let window = window(&bar, width, "…");
            assert!(
                window.range.contains(&active),
                "the active tab left a {width}-cell bar: {:?}",
                window.range
            );
            let marker = display_width("…") + 1;
            let drawn = usize::from(window.clipped_left) * marker
                + bar[window.range.clone()]
                    .iter()
                    .map(Tab::width)
                    .sum::<usize>()
                + usize::from(window.clipped_right) * marker;
            // A tab that cannot fit at all is drawn clipped rather than not
            // drawn, so the one row of slack is allowed.
            assert!(
                drawn <= width.max(bar[active].width()),
                "{drawn} cells into a {width}-cell bar at {active}"
            );
        }
    }
}
