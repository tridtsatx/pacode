//! Pure layout tests for the thumbnail strip: token→column mapping
//! (`token_center`), slot edge resolution (`place_slots`), and the combined
//! `layout_slots` ordering/clamping/fallback; then the transcript's inline
//! images and the prompt's image-token styling.

use std::path::PathBuf;
use std::time::Instant;

use pacode_render::RenderOptions;
use pacode_types::{AgentId, Config, ImageRef, TranscriptItem, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::*;
use crate::state::transcript::Transcript;
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
    assert_eq!(token_center(&s, input, "[Image 2]"), Some(10));
    // An input rect offset shifts the absolute column.
    let input = Rect::new(5, 10, 80, 3);
    assert_eq!(token_center(&s, input, "[Image 2]"), Some(15));
}

#[test]
fn token_center_wrapped_second_line() {
    // Outer width 10 → text width 8: "see " fills line 0, "[Image " lands
    // on line 1 — the anchor is the column where the token STARTS.
    let s = input_state("see [Image 2] ok");
    let input = Rect::new(0, 10, 10, 3);
    assert_eq!(token_center(&s, input, "[Image 2]"), Some(6)); // 2 + 0 + 4
}

#[test]
fn token_center_bash_mode() {
    // Bash mode draws the same rows as a plain prompt: the `!` is text after
    // the 2-cell prefix, so the token starts at 2 + width("!ls ") = 6 and
    // its center is 6 + 4 = 10 — running or not, the prefix is 2 cells.
    let s = input_state("!ls [Image 1]");
    let input = Rect::new(0, 10, 80, 1);
    assert_eq!(token_center(&s, input, "[Image 1]"), Some(10));
}

#[test]
fn token_center_bash_mode_follows_the_wrap() {
    // Outer width 12 → text width 10: "!echo aaa " fills row 0 and the token
    // starts row 1 at col 0 → 2 + 0 + 4.
    let s = input_state("!echo aaa [Image 1]");
    let input = Rect::new(0, 10, 12, 2);
    assert_eq!(s.wrap_lines(12), vec!["!echo aaa ", "[Image 1]"]);
    assert_eq!(token_center(&s, input, "[Image 1]"), Some(6));
}

#[test]
fn token_center_absent_is_none() {
    let s = input_state("no tokens");
    assert_eq!(token_center(&s, Rect::new(0, 10, 80, 1), "[Image 1]"), None);
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
    let slots = layout_slots(&state, &live(&["[Image 1]", "[Image 2]"]), input, area);
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
    let slots = layout_slots(&state, &live(&["[Image 1]", "[Image 2]"]), input, area);
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
    let slots = layout_slots(&state, &live(&["[Image 1]", "[Image 2]"]), input, area);
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
    let slots = layout_slots(&state, &live(&["[Image 1]"]), input, area);
    assert_eq!(slots.len(), 1);
    // The `!` is text behind the 2-cell prefix:
    // center = 2 + width("!echo ") + 4 = 12 → x = 12 - 10 = 2.
    assert_eq!(slots[0].2.x, 2);
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
    let slots = layout_slots(&state, &recs, input, area);
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
    let slots = layout_slots(&state, &live(&["[Image 1]", "[Image 9]"]), input, area);
    assert_eq!(slots.len(), 2);
    // Resolved first (center 2+2+4=8 → x 0), unresolved after it.
    assert_eq!(slots[0].0, 1);
    assert_eq!(slots[0].2.x, 0);
    assert_eq!(slots[1].0, 2);
    assert_eq!(slots[1].2.x, 60); // packed at area.right() - slot_w
}

fn init_store() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    pacode_image::store::init(dir.path().to_path_buf());
}

fn stored_png(w: u32, h: u32) -> ImageRef {
    init_store();
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        w,
        h,
        image::Rgb([200, 50, 50]),
    ));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .expect("encode");
    let hash = pacode_image::store::put(&bytes, "image/png").expect("put");
    ImageRef {
        hash,
        mime: "image/png".into(),
        width: Some(w),
        height: Some(h),
        name: Some("/tmp/shot.png".into()),
    }
}

fn opts() -> RenderOptions {
    RenderOptions::new(80, false)
}

/// What the event loop does between two draws: decode every image the last
/// draw asked for (off the frame path in the app) and hand the cells back.
fn decode_wants(cache: &mut ImageCellCache) {
    for key in cache.take_wants() {
        let decoded = crate::state::image_cache::decode(&key);
        cache.insert(key, decoded);
    }
}

/// `render_images` once the decodes it asked for have landed.
fn render_decoded(
    images: &[ImageRef],
    width: u16,
    cache: &mut ImageCellCache,
) -> Vec<Line<'static>> {
    render_images(images, width, &opts(), cache);
    decode_wants(cache);
    render_images(images, width, &opts(), cache)
}

fn width_of(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum()
}

#[test]
fn square_image_is_14_rows_and_fits_the_width() {
    let image = stored_png(100, 100);
    let mut cache = ImageCellCache::default();
    let lines = render_decoded(&[image], 80, &mut cache);
    // 14 image rows + the blank line after the picture.
    assert_eq!(lines.len(), 15);
    assert!(lines.last().unwrap().spans.is_empty());
    for line in &lines[..14] {
        assert!(width_of(line) <= 80);
        assert!(width_of(line) > INDENT.len());
    }
    // 100x100 px: 28 pixel rows tall -> 28 columns wide (square pixels).
    assert_eq!(width_of(&lines[0]), INDENT.len() + 28);
    assert_eq!(cache.len(), 1);
}

#[test]
fn wide_image_is_bounded_by_the_cell_width() {
    let image = stored_png(1000, 100);
    let mut cache = ImageCellCache::default();
    let lines = render_decoded(&[image], 40, &mut cache);
    assert!(lines.iter().all(|l| width_of(l) <= 40));
    // 38 columns -> 38 px wide, 3.8 px tall -> 4 px -> 2 cell rows (+ blank).
    assert_eq!(lines.len(), 3);
}

#[test]
fn rows_are_painted_with_half_blocks_in_the_pixel_colours() {
    let image = stored_png(10, 10);
    let mut cache = ImageCellCache::default();
    let lines = render_decoded(&[image], 80, &mut cache);
    let span = &lines[0].spans[1];
    assert!(span.content.starts_with('\u{2580}'));
    assert_eq!(span.style.fg, Some(Color::Rgb(200, 50, 50)));
    assert_eq!(span.style.bg, Some(Color::Rgb(200, 50, 50)));
    // Identical cells are merged into one span.
    assert_eq!(lines[0].spans.len(), 2);
}

#[test]
fn ansi_terminals_get_indexed_colours() {
    let image = stored_png(10, 10);
    let mut cache = ImageCellCache::default();
    cache.truecolor = false;
    let lines = render_decoded(&[image], 80, &mut cache);
    assert!(!matches!(lines[0].spans[1].style.fg, Some(Color::Rgb(..))));
}

#[test]
fn off_draws_a_one_line_stub() {
    let image = stored_png(800, 600);
    let mut cache = ImageCellCache::default();
    cache.enabled = false;
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    assert_eq!(lines.len(), 2);
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text.trim(), "[image: shot.png 800\u{d7}600]");
    assert!(cache.is_empty());
    assert!(cache.take_wants().is_empty(), "off decodes nothing");
}

#[test]
fn missing_bytes_degrade_to_a_stub() {
    init_store();
    let image = ImageRef {
        hash: "d".repeat(64),
        mime: "image/png".into(),
        width: Some(5),
        height: Some(5),
        name: None,
    };
    let mut cache = ImageCellCache::default();
    let lines = render_decoded(&[image], 80, &mut cache);
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("unavailable"), "{text}");
}

#[test]
fn an_image_not_decoded_yet_draws_its_stub_and_asks_for_a_decode() {
    let image = stored_png(100, 100);
    let mut cache = ImageCellCache::default();
    let lines = render_images(std::slice::from_ref(&image), 80, &opts(), &mut cache);
    assert_eq!(lines.len(), 2, "stub + blank: nothing decoded in draw");
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text.trim(), "[image: shot.png 100\u{d7}100]");
    assert!(cache.is_empty());
    let wants = cache.take_wants();
    assert_eq!(wants.len(), 1);
    assert_eq!(wants[0].hash, image.hash);
}

#[test]
fn transcript_cell_draws_the_image_below_the_text_at_width_80() {
    let image = stored_png(100, 100);
    let mut transcript = Transcript::new(50);
    let now = Instant::now();
    transcript.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::User {
                text: "what is in this picture?".into(),
            },
        },
        now,
    );
    transcript.upsert(
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::Images {
                images: vec![image],
            },
        },
        now,
    );

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let render_opts = opts();
    // The first frame draws the stub and asks for the decode; the event loop
    // decodes off the frame path and hands the cells back.
    terminal
        .draw(|frame| {
            let area = frame.area();
            let prepared = crate::ui::dialog::prepare(area, &mut transcript, &render_opts, 0, 0);
            crate::ui::dialog::draw(frame, area, &prepared);
        })
        .expect("draw");
    let wants = transcript.images.take_wants();
    assert_eq!(wants.len(), 1);
    for key in wants {
        let decoded = crate::state::image_cache::decode(&key);
        transcript.image_ready(key, decoded);
    }
    terminal
        .draw(|frame| {
            let area = frame.area();
            let prepared = crate::ui::dialog::prepare(area, &mut transcript, &render_opts, 0, 0);
            crate::ui::dialog::draw(frame, area, &prepared);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..80)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    // The user text, then the picture below it.
    let text_row = (0..24)
        .find(|y| row_text(*y).contains("what is in this picture?"))
        .expect("user text drawn");
    let picture: Vec<u16> = (0..24)
        .filter(|y| row_text(*y).contains('\u{2580}'))
        .collect();
    assert_eq!(picture.len(), 14);
    assert!(picture[0] > text_row);
    // Every picture cell is non-empty and carries a colour.
    assert_eq!(buffer[(3, picture[0])].symbol(), "\u{2580}");
    assert_eq!(buffer[(3, picture[0])].fg, Color::Rgb(200, 50, 50));
    // Cached lines: a second draw reuses the decoded cells.
    assert_eq!(transcript.images.len(), 1);
}

#[test]
fn paste_tokens_in_input_lines_are_styled_separately() {
    let base = Style::default();
    let chip = Style::default().fg(Color::Cyan);
    let spans = style_chips("see [Image 2] and [Image x]", base, chip);
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[1].content, "[Image 2]");
    assert_eq!(spans[1].style, chip);
    assert_eq!(spans[2].content, " and [Image x]");
}
