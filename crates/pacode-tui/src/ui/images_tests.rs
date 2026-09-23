//! Pure layout tests for the thumbnail strip: token→column mapping
//! (`token_center`), slot edge resolution (`place_slots`), and the combined
//! `layout_slots` ordering/clamping/fallback.

use std::path::PathBuf;

use pacode_render::RenderOptions;
use pacode_types::Config;
use ratatui::layout::Rect;

use super::*;
use crate::state::{AppState, InputState};

fn input_state(text: &str) -> InputState {
    let mut s = InputState::default();
    s.insert_str(text);
    s
}

fn app_state(text: &str) -> AppState {
    let mut state = AppState::new(Config::default(), "1.0.0".to_string(), 120, 34);
    state.input.insert_str(text);
    state
}

fn opts() -> RenderOptions {
    RenderOptions::new(80, false)
}

fn live(labels: &[&str]) -> Vec<(usize, String, PathBuf)> {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (
                i + 1,
                l.to_string(),
                PathBuf::from(format!("/p{}.png", i + 1)),
            )
        })
        .collect()
}

#[test]
fn token_center_first_line() {
    // "see [Image 2] ok": token at byte 4, prompt prefix 2 → text col 6,
    // token 9 cells wide → center at 6 + 4 = 10 inside the input rect.
    let s = input_state("see [Image 2] ok");
    let input = Rect::new(0, 10, 80, 3);
    assert_eq!(token_center(&s, input, "[Image 2]", 2), Some(10));
    // An input rect offset shifts the absolute column.
    let input = Rect::new(5, 10, 80, 3);
    assert_eq!(token_center(&s, input, "[Image 2]", 2), Some(15));
}

#[test]
fn token_center_wrapped_second_line() {
    // Outer width 10 → text width 8: "see " fills line 0, "[Image " lands
    // on line 1 — the anchor is the column where the token STARTS.
    let s = input_state("see [Image 2] ok");
    let input = Rect::new(0, 10, 10, 3);
    assert_eq!(token_center(&s, input, "[Image 2]", 2), Some(6)); // 2 + 0 + 4
}

#[test]
fn token_center_bash_mode() {
    // "!ls [Image 1]": the `!` becomes the 2-cell prefix, so the token's
    // start col is 2 + width("ls ") = 5; center 5 + 4 = 9.
    let s = input_state("!ls [Image 1]");
    let input = Rect::new(0, 10, 80, 1);
    assert_eq!(token_center(&s, input, "[Image 1]", 2), Some(9));
    // The busy prefix pushes it right.
    assert_eq!(token_center(&s, input, "[Image 1]", 9), Some(16));
}

#[test]
fn token_center_absent_is_none() {
    let s = input_state("no tokens");
    assert_eq!(
        token_center(&s, Rect::new(0, 10, 80, 1), "[Image 1]", 2),
        None
    );
}

#[test]
fn place_slots_centers_and_clamps() {
    let area = Rect::new(0, 0, 60, 6);
    // Ideal left edges (center − slot_w/2) that fit come back untouched.
    assert_eq!(place_slots(&[5, 35], 20, area), vec![5, 35]);
    // Ideal at the left edge stays; the next slot can't overlap it.
    assert_eq!(place_slots(&[0, 10], 20, area), vec![0, 20]);
    // Ideals past the right edge pull back into bounds, non-overlapping.
    let xs = place_slots(&[45, 50], 20, area);
    assert_eq!(xs, vec![20, 40]);
    for (i, x) in xs.iter().enumerate() {
        assert!(*x >= area.x);
        assert!(x + 20 <= area.right());
        if i > 0 {
            assert!(*x >= xs[i - 1] + 20, "slots must not overlap");
        }
    }
}

#[test]
fn place_slots_tight_strip_packs() {
    // slot_w * n > area.width (the caller's `.max(4)` case): positions stay
    // inside the area and never overlap; the caller clips the tail width.
    let area = Rect::new(0, 0, 10, 6);
    let xs = place_slots(&[u16::MAX; 3], 4, area);
    assert_eq!(xs, vec![0, 4, 8]);
}

#[test]
fn layout_slots_follow_text_order_not_insertion() {
    // Records passed in insertion order; the text mentions token 2 first.
    let state = app_state("[Image 2] then [Image 1]");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(0, 4, 80, 6);
    let slots = layout_slots(
        &state,
        &live(&["[Image 1]", "[Image 2]"]),
        input,
        area,
        &opts(),
    );
    let order: Vec<usize> = slots.iter().map(|(t, ..)| *t).collect();
    assert_eq!(order, vec![2, 1], "slots come out in text order");
    // Centres 6 and 21 → gap 15 → slots narrow to 15 so both can centre:
    // token 2 (byte 0 → center 6) → ideal 0; token 1 (byte 15 → center 21)
    // → ideal 14, clamped to the previous slot's right edge.
    let rects: Vec<(u16, u16)> = slots.iter().map(|(.., r)| (r.x, r.width)).collect();
    assert_eq!(rects, vec![(0, 15), (15, 15)]);
}

#[test]
fn layout_slots_adjacent_tokens_shrink_to_center() {
    // "[Image 1] [Image 2]": centres 6 and 16 — a 20-wide slot can never
    // centre on both, so the strip narrows the thumbs to the 10-col gap.
    let state = app_state("[Image 1] [Image 2]");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(0, 4, 80, 6);
    let slots = layout_slots(
        &state,
        &live(&["[Image 1]", "[Image 2]"]),
        input,
        area,
        &opts(),
    );
    let rects: Vec<(u16, u16)> = slots.iter().map(|(.., r)| (r.x, r.width)).collect();
    // centre 6 → ideal 1; centre 16 → ideal 11 — each thumb centred on its
    // token instead of packing at x=0/20.
    assert_eq!(rects, vec![(1, 10), (11, 10)]);
}

#[test]
fn layout_slots_centers_distant_tokens() {
    // Tokens far enough apart that both slots sit exactly centered.
    let state = app_state("a [Image 1] bbbbbbbbbbbbbbbbbbbbb [Image 2]");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(0, 4, 80, 6);
    let slots = layout_slots(
        &state,
        &live(&["[Image 1]", "[Image 2]"]),
        input,
        area,
        &opts(),
    );
    let xs: Vec<(usize, u16)> = slots.iter().map(|(t, .., r)| (*t, r.x)).collect();
    // t1: byte 2 → center 2+2+4=8 → ideal x 0 (saturating); t2: byte 34 →
    // center 2+34+4=40 → x = 40 - 10 = 30.
    assert_eq!(xs, vec![(1, 0), (2, 30)]);
}

#[test]
fn layout_slots_bash_mode() {
    let state = app_state("!echo [Image 1]");
    let input = Rect::new(0, 10, 80, 1);
    let area = Rect::new(0, 4, 80, 6);
    let slots = layout_slots(&state, &live(&["[Image 1]"]), input, area, &opts());
    assert_eq!(slots.len(), 1);
    // center = 2 + width("echo ") + 4 = 11 → x = 11 - 10 = 1.
    assert_eq!(slots[0].2.x, 1);
    assert_eq!(slots[0].2.width, THUMB_SLOT_W);
}

#[test]
fn layout_slots_narrow_area_shrinks_and_packs() {
    // 3 slots in 40 cols → slot_w = 13; every slot stays inside the area
    // and none overlap.
    let state = app_state("[Image 1] [Image 2] [Image 3]");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(0, 4, 40, 6);
    let slots = layout_slots(
        &state,
        &live(&["[Image 1]", "[Image 2]", "[Image 3]"]),
        input,
        area,
        &opts(),
    );
    assert_eq!(slots.len(), 3);
    let mut prev_right = area.x;
    for (.., r) in &slots {
        assert!(r.x >= prev_right, "overlap");
        assert!(r.right() <= area.right(), "out of bounds");
        prev_right = r.right();
    }
}

#[test]
fn layout_slots_fallback_left_packs_when_no_token_resolves() {
    // Defensive path: a record whose label is absent from the text (the
    // mid-frame deletion race). With NO token resolved the strip is the
    // pre-centering left-packed row.
    let state = app_state("no tokens here");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(2, 4, 50, 6);
    let recs = live(&["[Image 7]", "[Image 8]"]);
    let slots = layout_slots(&state, &recs, input, area, &opts());
    let xs: Vec<u16> = slots.iter().map(|(.., r)| r.x).collect();
    assert_eq!(xs, vec![area.x, area.x + THUMB_SLOT_W]);
}

#[test]
fn layout_slots_partial_fallback_packs_unresolved_right() {
    // One record resolves, one does not: the unresolved packs at the right
    // end after the resolved one.
    let state = app_state("x [Image 1]");
    let input = Rect::new(0, 10, 80, 3);
    let area = Rect::new(0, 4, 80, 6);
    let slots = layout_slots(
        &state,
        &live(&["[Image 1]", "[Image 9]"]),
        input,
        area,
        &opts(),
    );
    assert_eq!(slots.len(), 2);
    // Resolved first (center 2+2+4=8 → x 0), unresolved after it.
    assert_eq!(slots[0].0, 1);
    assert_eq!(slots[0].2.x, 0);
    assert_eq!(slots[1].0, 2);
    assert_eq!(slots[1].2.x, 60); // packed at area.right() - slot_w
}
